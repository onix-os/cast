// SPDX-FileCopyrightText: 2024 AerynOS Developers

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, io,
    path::PathBuf,
};

use astr::AStr;
use fs_err as fs;
use rayon::iter::{IntoParallelIterator as _, IntoParallelRefIterator as _, ParallelIterator as _};
use stone::{StoneDigestWriter, StoneDigestWriterHasher, StonePayloadLayoutFile};
use tui::{
    ProgressBar, ProgressStyle, Styled,
    dialoguer::{Confirm, theme::ColorfulTheme},
};
use vfs::tree::BlitFile;

use crate::{
    Client, Package, Signal,
    client::{self, cache},
    package, runtime, signal, state,
};

pub fn verify(client: &Client, yes: bool, verbose: bool) -> Result<(), client::Error> {
    client.require_stateful_scope()?;

    // Retain one cooperating-writer lease from the authoritative scan through
    // removal of any corrupt asset names. Do not carry this non-reentrant
    // process mutex into cache publication or state materialization: each of
    // those paths acquires and retains its own lease at its mutation boundary.
    let writer_lease = client::active_state_authority::ActiveStateAuthority::acquire(&client.installation)?;
    let active_state = writer_lease.active();

    println!("Verifying assets");

    // Get all installed layouts, this is our source of truth
    let layouts = client.layout_db.all()?;

    // Group by unique assets (hash)
    let mut unique_assets = BTreeMap::new();
    for (package, layout) in layouts {
        let StonePayloadLayoutFile::Regular(hash, file) = layout.file else {
            continue;
        };
        unique_assets
            .entry(format!("{hash:02x}"))
            .or_insert_with(Vec::new)
            .push((package, file));
    }

    let pb = ProgressBar::new(unique_assets.len() as u64)
        .with_message("Verifying")
        .with_style(
            ProgressStyle::with_template("\n|{bar:20.red/blue}| {pos}/{len} {wide_msg}")
                .unwrap()
                .progress_chars("■≡=- "),
        );
    pb.tick();

    // For each asset, ensure it exists in the content store and isn't corrupt (hash is correct)
    let mut issues = unique_assets
        .into_par_iter()
        .try_fold(Vec::new, |mut acc, (hash, meta)| -> io::Result<_> {
            // Padded so output is consistent
            let display_hash = format!("{hash:0>32}");

            let path = cache::asset_path(&client.installation, &hash);

            let files = meta.iter().map(|(_, file)| file).cloned().collect::<BTreeSet<_>>();

            pb.set_message(format!("Verifying {display_hash}"));

            if !path.exists() {
                pb.inc(1);
                if verbose {
                    pb.suspend(|| println!(" {} {display_hash} - {files:?}", "×".yellow()));
                }
                acc.push(Issue::MissingAsset {
                    hash,
                    files,
                    packages: meta.into_iter().map(|(package, _)| package).collect(),
                });
                return Ok(acc);
            }

            let mut hasher = StoneDigestWriterHasher::new();
            let mut digest_writer = StoneDigestWriter::new(io::sink(), &mut hasher);
            let mut file = fs::File::open(&path)?;

            // Copy bytes to null sink so we don't
            // explode memory
            io::copy(&mut file, &mut digest_writer)?;

            let verified_hash = format!("{:02x}", hasher.digest128());

            if verified_hash != hash {
                pb.inc(1);
                if verbose {
                    pb.suspend(|| println!(" {} {display_hash} - {files:?}", "×".yellow()));
                }
                acc.push(Issue::CorruptAsset {
                    hash,
                    files,
                    packages: meta.into_iter().map(|(package, _)| package).collect(),
                });
                return Ok(acc);
            }

            pb.inc(1);
            if verbose {
                pb.suspend(|| println!(" {} {display_hash} - {files:?}", "»".green()));
            }

            Ok(acc)
        })
        .try_reduce(Vec::new, try_reduce_vec_concat)?;

    // Get all states
    let states = client.state_db.all()?;

    pb.set_length(states.len() as u64);
    pb.set_position(0);
    pb.suspend(|| {
        println!("Verifying states");
    });

    // Check the VFS of each state exists properly on the FS
    let states_issues = states
        .par_iter()
        .try_fold(Vec::new, |mut acc, state| {
            pb.set_message(format!("Verifying state #{}", state.id));

            let is_active = active_state == Some(state.id);

            let vfs = client.vfs(state.selections.iter().map(|s| &s.package))?;

            let base = if is_active {
                client.installation.root.join("usr")
            } else {
                client.installation.root_path(state.id.to_string()).join("usr")
            };

            let state_issues: Vec<_> = vfs
                .iter()
                .filter_map(|file| {
                    let path = base.join(file.path().strip_prefix("/usr/").unwrap_or_default());

                    // All symlinks for non-active states are broken
                    // since they resolve to the active state path
                    //
                    // Use try_exists to ensure we only check if symlink
                    // itself is missing
                    match path.try_exists() {
                        Ok(true) => None,
                        Ok(false) if path.is_symlink() => None,
                        _ => Some(Issue::MissingVFSPath { path, state: state.id }),
                    }
                })
                .collect();

            pb.inc(1);
            if verbose {
                let mark = if !state_issues.is_empty() {
                    "×".yellow()
                } else {
                    "»".green()
                };
                pb.suspend(|| println!(" {mark} state #{}", state.id));
            }

            acc.extend(state_issues);
            Ok::<_, super::Error>(acc)
        })
        .try_reduce(Vec::new, try_reduce_vec_concat)?;
    issues.extend(states_issues);

    pb.finish_and_clear();

    if issues.is_empty() {
        writer_lease.revalidate(&client.installation)?;
        println!("No issues found");
        return Ok(());
    }

    println!(
        "Found {} issue{}",
        issues.len(),
        if issues.len() == 1 { "" } else { "s" }
    );

    for issue in &issues {
        println!(" {} {issue}", "×".yellow());
    }

    let result = if yes {
        true
    } else {
        Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(" Fixing issues, this will change your system state. Do you wish to continue? ")
            .default(false)
            .interact()?
    };
    if !result {
        return Err(client::Error::Cancelled);
    }

    // Acquire and require interruption protection before the first repair
    // mutation, then retain both guards for the complete repair transaction.
    let _guard = signal::ignore([Signal::SIGINT])?;
    let _inhibitor = signal::inhibit(
        vec!["shutdown", "sleep", "idle", "handle-lid-switch"],
        "cast".into(),
        "Verifying states".into(),
        "block".into(),
    )?;

    // Calculate and resolve the unique set of packages with asset issues
    let issue_packages = issues
        .iter()
        .filter_map(Issue::packages)
        .flatten()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| {
            client.install_db.get(id).map(|meta| Package {
                id: id.clone(),
                meta,
                flags: package::Flags::default(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    // We had some corrupt or missing assets, let's resolve that!
    let writer_snapshot = if !issue_packages.is_empty() {
        writer_lease.revalidate(&client.installation)?;
        // Remove all corrupt assets
        for corrupt_hash in issues.iter().filter_map(Issue::corrupt_hash) {
            let path = cache::asset_path(&client.installation, corrupt_hash);
            fs::remove_file(&path)?;
        }

        // `cache_packages` acquires this coordinator only inside its blocking
        // publication closures. Drop the scan lease before entering the async
        // runtime so no std mutex guard crosses an await and cache publication
        // cannot self-deadlock.
        let writer_snapshot = writer_lease.suspend(&client.installation)?;

        println!("Reinstalling packages");

        // And re-cache all packages that comprise the corrupt / missing asset
        runtime::block_on(client.cache_packages(&issue_packages))?;
        writer_snapshot
    } else {
        writer_lease.suspend(&client.installation)?
    };
    // Now we must fix any states that referenced these packages
    // or had their own VFS issues that require a reblit
    let issue_states = states
        .iter()
        .filter_map(|state| {
            state
                .selections
                .iter()
                .any(|s| issue_packages.iter().any(|p| p.id == s.package))
                .then_some(&state.id)
        })
        .chain(issues.iter().filter_map(Issue::state))
        .collect::<BTreeSet<_>>();

    println!("Reblitting affected states");

    // Resume the exact pre-cache proof rather than recapturing a fresh live
    // tree. If the active state needs repair, consume that authority into its
    // candidate before releasing the coordinator for archived repairs.
    let active_issue = active_state.filter(|id| issue_states.contains(id));
    if let Some(id) = active_issue {
        let state = states
            .iter()
            .find(|state| state.id == id)
            .expect("must come from states originally");
        let current_state = client.state_db.get(state.id)?;
        if current_state != *state {
            return Err(client::Error::VerifyStateChanged { state: state.id });
        }
        let system_model = client.load_or_create_system_snapshot(
            crate::system_model::snapshot_path(&client.installation.root),
            &current_state,
        )?;
        let active_authority = writer_snapshot.resume(&client.installation)?;
        let candidate = client.materialize_active_verify_candidate(
            state.selections.iter().map(|selection| &selection.package),
            active_authority,
        )?;
        client.apply_active_reblit_candidate(candidate, &current_state, system_model)?;
        println!(" {} state #{}", "»".green(), state.id);
    } else {
        drop(writer_snapshot.resume(&client.installation)?);
    }

    // Inactive repair is writable by transaction triggers, so each state gets
    // independent package inodes and its own coordinator token after the live
    // authority has been consumed or released.
    for id in issue_states.into_iter().filter(|id| Some(**id) != active_issue) {
        let state = states
            .iter()
            .find(|state| state.id == *id)
            .expect("must come from states originally");
        let current_state = client.state_db.get(state.id)?;
        if current_state != *state {
            return Err(client::Error::VerifyStateChanged { state: state.id });
        }
        let system_model = client.load_or_create_system_snapshot(
            crate::system_model::snapshot_path(&client.installation.root_path(state.id.to_string())),
            &current_state,
        )?;
        let candidate = client.materialize_archived_repair_root(state.selections.iter().map(|s| &s.package))?;
        client.repair_archived_state(candidate, &current_state, system_model)?;
        println!(" {} state #{}", "»".green(), state.id);
    }

    println!("All issues resolved");

    Ok(())
}

#[derive(Debug)]
enum Issue {
    CorruptAsset {
        hash: String,
        files: BTreeSet<AStr>,
        packages: BTreeSet<package::Id>,
    },
    MissingAsset {
        hash: String,
        files: BTreeSet<AStr>,
        packages: BTreeSet<package::Id>,
    },
    MissingVFSPath {
        path: PathBuf,
        state: state::Id,
    },
}

impl Issue {
    fn corrupt_hash(&self) -> Option<&str> {
        match self {
            Issue::CorruptAsset { hash, .. } => Some(hash),
            Issue::MissingAsset { .. } | Issue::MissingVFSPath { .. } => None,
        }
    }

    fn packages(&self) -> Option<&BTreeSet<package::Id>> {
        match self {
            Issue::CorruptAsset { packages, .. } | Issue::MissingAsset { packages, .. } => Some(packages),
            Issue::MissingVFSPath { .. } => None,
        }
    }

    fn state(&self) -> Option<&state::Id> {
        match self {
            Issue::CorruptAsset { .. } | Issue::MissingAsset { .. } => None,
            Issue::MissingVFSPath { state, .. } => Some(state),
        }
    }
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Issue::CorruptAsset { hash, files, .. } => write!(f, "Corrupt asset {hash} - {files:?}"),
            Issue::MissingAsset { hash, files, .. } => write!(f, "Missing asset {hash} - {files:?}"),
            Issue::MissingVFSPath { path, state } => write!(f, "Missing path {} in state #{state}", path.display()),
        }
    }
}

fn try_reduce_vec_concat<T, E>(mut a: Vec<T>, mut b: Vec<T>) -> Result<Vec<T>, E> {
    a.append(&mut b);
    Ok(a)
}
