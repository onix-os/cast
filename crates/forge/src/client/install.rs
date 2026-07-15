// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Installation-specific code for several core Cast operations

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use thiserror::Error;
use tracing::{Instrument, debug, info, info_span, instrument};
use tui::{
    dialoguer::{Confirm, theme::ColorfulTheme},
    pretty::autoprint_columns,
};

use crate::{
    Package, Provider,
    client::{self, Client, FrozenMaterialization},
    package::{self, Flags},
    registry::transaction,
    runtime,
    state::Selection,
};

/// Install a set of packages.
///
/// If this call is successful a new State is recorded into the [`super::db::state::Database`].
/// Upon completion the `/usr` tree is "hot swapped" with the staging tree through `renameat2` call.
#[instrument(skip(client), fields(ephemeral = client.is_ephemeral()))]
pub fn install(client: &mut Client, pkgs: &[&str], yes: bool, simulate: bool) -> Result<Timing, Error> {
    // Resolve input packages
    let input = resolve_input(pkgs, client)?;
    debug!(resolved_packages = input.len(), "Resolved input packages");

    // Resolve the transaction while retaining a proof for the exact active
    // registry snapshot. Drop that proof before the separately guarded
    // metadata resolution and before any later state transition.
    let finalized = client.with_registry_snapshot(|registry| -> Result<Vec<package::Id>, Error> {
        let mut tx = registry.transaction(transaction::Lookup::PreferInstalled)?;
        tx.add(input.clone())?;
        Ok(tx.finalize().cloned().collect())
    })?;

    // Resolve transaction to metadata
    let resolved = client.resolve_packages(finalized.iter())?;

    install_resolved(client, input, resolved, yes, simulate)
}

/// Install an already-resolved package closure without looking up providers or
/// traversing package dependencies again.
///
/// The caller owns closure resolution. Every supplied ID is treated as an
/// exact selection and must still exist in the active repository registry.
pub fn install_exact(
    client: &mut Client,
    packages: &[package::Id],
    yes: bool,
    simulate: bool,
) -> Result<Timing, Error> {
    let input = packages
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let resolved = input
        .iter()
        .map(|id| client.resolve_package(id).map_err(Error::from))
        .collect::<Result<Vec<_>, _>>()?;

    install_resolved(client, input, resolved, yes, simulate)
}

/// Cache and materialize an exact package closure into a canonical frozen root
/// owned by a dedicated cache-only client.
///
/// This is intentionally separate from [`install_exact`]: it never creates a
/// package-manager state and therefore never reaches snapshots, os-release,
/// transaction or system triggers, boot synchronization, or provider and
/// dependency resolution. Package IDs are sorted canonically and resolved only
/// from explicitly configured active repositories by exact content identity.
/// Content, inode type, mode, and atime/mtime are normalized; kernel-assigned
/// inode numbers, device IDs, ctime, and btime are not reproducibility claims.
pub fn materialize_frozen_root(
    client: &mut Client,
    packages: &[package::Id],
    source_date_epoch: i64,
) -> Result<FrozenMaterialization, Error> {
    // Scope validation precedes registry, cache, database, or root side
    // effects. A stateful caller must fail closed.
    client.frozen_root()?;

    let mut timing = Timing::default();
    let mut instant = Instant::now();
    let packages = client.canonical_frozen_package_ids(packages)?;
    let resolved = resolve_exact_packages(client, &packages)?;
    timing.resolve = instant.elapsed();

    instant = Instant::now();
    runtime::block_on(client.cache_packages(&resolved).in_current_span())?;
    timing.fetch = instant.elapsed();

    instant = Instant::now();
    let root = client.blit_frozen_root(&packages, source_date_epoch)?;
    timing.blit = instant.elapsed();

    Ok(FrozenMaterialization { timing, root })
}

fn resolve_exact_packages(client: &Client, packages: &[package::Id]) -> Result<Vec<Package>, Error> {
    packages
        .iter()
        .map(|id| {
            let package = client.resolve_frozen_repository_package(id)?;
            if package.meta.hash.as_deref() != Some(id.as_str()) {
                return Err(Error::FrozenPackageIdentityMismatch {
                    requested: id.clone(),
                    metadata_hash: package.meta.hash,
                });
            }
            Ok(package)
        })
        .collect()
}

fn install_resolved(
    client: &mut Client,
    input: Vec<package::Id>,
    resolved: Vec<Package>,
    yes: bool,
    simulate: bool,
) -> Result<Timing, Error> {
    let mut timing = Timing::default();
    let mut instant = Instant::now();

    // Get installed packages to check against
    let installed =
        client.with_registry_snapshot(|registry| -> Result<Vec<Package>, Error> { Ok(registry.list_installed()?) })?;
    let is_installed = |p: &Package| installed.iter().any(|i| i.meta.name == p.meta.name);

    // Get missing packages that are:
    //
    // Stateful: Not installed
    // Ephemeral: all
    let missing = resolved
        .iter()
        .filter(|p| client.is_ephemeral() || !is_installed(p))
        .collect::<Vec<_>>();

    timing.resolve = instant.elapsed();
    info!(
        total_resolved = resolved.len(),
        missing_packages = missing.len(),
        already_installed = resolved.len() - missing.len(),
        resolve_time_ms = timing.resolve.as_millis(),
        "Package resolution completed"
    );

    // If no new packages exist, exit and print
    // packages already installed
    if missing.is_empty() {
        let installed = resolved
            .iter()
            .filter(|p| is_installed(p) && input.contains(&p.id))
            .collect::<Vec<_>>();

        if !installed.is_empty() {
            println!("The following package(s) are already installed:");
            println!();
            autoprint_columns(&installed);
        }

        return Ok(timing);
    }

    // Testing panic for hyperfine benchmarking purposes (build flag tuning)
    // panic!();

    println!("The following package(s) will be installed:");
    println!();
    autoprint_columns(&missing);
    println!();

    if simulate {
        return Ok(timing);
    }

    // Must we prompt?
    let result = if yes {
        true
    } else {
        Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(" Do you wish to continue? ")
            .default(false)
            .interact()?
    };
    if !result {
        return Err(Error::Cancelled);
    }

    instant = Instant::now();

    let cache_packages_span = info_span!("progress", phase = "cache_packages", event_type = "progress");
    let _cache_packages_guard = cache_packages_span.enter();
    info!(
        total_items = missing.len(),
        progress = 0.0,
        event_type = "progress_start"
    );

    // Cache packages
    runtime::block_on(client.cache_packages(&missing).in_current_span())?;

    timing.fetch = instant.elapsed();
    info!(
        duration_ms = timing.fetch.as_millis(),
        items_processed = missing.len(),
        progress = 1.0,
        event_type = "progress_completed",
    );
    drop(_cache_packages_guard);
    instant = Instant::now();

    // Calculate the new state of packages (old_state + missing)
    let new_state_pkgs = {
        // Only use previous state in stateful mode
        let previous_selections = match client.active_state_for_planning()? {
            Some(id) if !client.is_ephemeral() => client.state_db.get(id)?.selections,
            _ => vec![],
        };
        let missing_selections = missing.iter().map(|p| Selection {
            package: p.id.clone(),
            // Package is explicit if it was one of the input
            // packages provided by the user
            explicit: input.contains(&p.id),
            reason: None,
        });

        missing_selections.chain(previous_selections).collect::<Vec<_>>()
    };

    // Perfect, apply state.
    client.new_state(&new_state_pkgs, "Install")?;

    timing.blit = instant.elapsed();

    info!(
        blit_time_ms = timing.blit.as_millis(),
        total_time_ms = (timing.resolve + timing.fetch + timing.blit).as_millis(),
        "Installation completed successfully"
    );

    Ok(timing)
}

/// Resolves the package arguments as valid input packages. Returns an error
/// if any args are invalid.
#[instrument(skip(client))]
fn resolve_input(pkgs: &[&str], client: &Client) -> Result<Vec<package::Id>, Error> {
    // Parse pkg args into valid / invalid sets
    let mut results = vec![];

    for package in pkgs {
        let (id, pkg) = find_packages(package, client)?;
        if let Some(pkg) = pkg {
            results.push(pkg.id);
        } else {
            return Err(Error::NoPackage(id));
        }
    }

    Ok(results)
}

/// Resolve a package name to the first package
fn find_packages(id: &str, client: &Client) -> Result<(String, Option<Package>), Error> {
    let provider = Provider::from_name(id).unwrap();
    client.with_registry_snapshot(|registry| -> Result<(String, Option<Package>), Error> {
        let result = registry
            .by_provider(&provider, Flags::new().with_available())?
            .into_iter()
            .next();

        // First only, pre-sorted
        Ok((id.into(), result))
    })
}

/// Simple timing information for Install
#[derive(Default)]
pub struct Timing {
    pub resolve: Duration,
    pub fetch: Duration,
    pub blit: Duration,
}

/// Error's specific to installation operations
#[derive(Debug, Error)]
pub enum Error {
    /// The operation was explicitly cancelled at the user's request
    #[error("cancelled")]
    Cancelled,

    /// An error originated in [`client`] module
    #[error("client")]
    Client(#[from] client::Error),

    /// The given package couldn't be found
    #[error("no package found: {0}")]
    NoPackage(String),

    /// A transaction specific error occurred
    #[error("transaction")]
    Transaction(#[from] transaction::Error),

    #[error("registry query")]
    Registry(#[from] crate::registry::Error),

    /// A database specific error occurred
    #[error("db")]
    DB(#[from] crate::db::Error),

    /// Had issues processing user-provided string input
    #[error("string processing")]
    Dialog(#[from] tui::dialoguer::Error),

    /// We forgot how disks work
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error("repository metadata hash {metadata_hash:?} does not match requested frozen package {requested}")]
    FrozenPackageIdentityMismatch {
        requested: package::Id,
        metadata_hash: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use sha2::{Digest as _, Sha256};
    use stone::{StoneHeaderV1FileType, StoneWriter};

    use super::*;
    use crate::{
        Dependency, Installation, Registry, State, db,
        registry::plugin,
        repository,
        state::{self, Selection},
        system_model,
        test_support::prepare_private_installation_root,
    };

    fn package(id: &str, dependencies: BTreeSet<Dependency>) -> Package {
        Package {
            id: package::Id::from(id.to_owned()),
            meta: package::Meta {
                name: package::Name::from(id.to_owned()),
                version_identifier: "1".to_owned(),
                source_release: 1,
                build_release: 1,
                architecture: "x86_64".to_owned(),
                summary: String::new(),
                description: String::new(),
                source_id: id.to_owned(),
                homepage: String::new(),
                licenses: Vec::new(),
                dependencies,
                providers: BTreeSet::new(),
                conflicts: BTreeSet::new(),
                uri: Some(format!("{id}.stone")),
                hash: Some(id.to_owned()),
                download_size: None,
            },
            flags: Flags::new().with_available(),
        }
    }

    fn stateful_client(root: &std::path::Path, packages: Vec<Package>) -> Client {
        prepare_private_installation_root(root);
        let installation = Installation::open(root, None).unwrap();
        let mut registry = Registry::default();
        registry.add_plugin(plugin::Plugin::Test(plugin::Test::new(1, packages)));
        Client::mocked(installation, registry).unwrap()
    }

    fn frozen_client(root: &std::path::Path, packages: Vec<Package>) -> Client {
        prepare_private_installation_root(root);
        let installation_root = root.join("installation");
        if !installation_root.exists() {
            fs_err::create_dir(&installation_root).unwrap();
        }
        prepare_private_installation_root(&installation_root);
        let blit_root = root.join("frozen-root");
        fs_err::create_dir(&blit_root).unwrap();
        let installation = Installation::open_frozen(&installation_root, None).unwrap();
        let repositories = repository::Map::with([(
            repository::Id::new("explicit"),
            repository::Repository {
                description: "explicit frozen test repository".to_owned(),
                source: repository::Source::DirectIndex("https://packages.invalid/stone.index".parse().unwrap()),
                priority: repository::Priority::new(1),
                active: true,
            },
        )]);
        let client = Client::frozen("frozen-test", installation, repositories, blit_root).unwrap();
        let repository = client.repositories.active().next().unwrap();
        let index_bytes = b"frozen repository test snapshot";
        let index_sha256 = hex::encode(Sha256::digest(index_bytes));
        let index_uri = match &repository.repository.source {
            repository::Source::DirectIndex(index_uri) => index_uri.clone(),
            repository::Source::RootIndex(_) => unreachable!("frozen test repository is direct"),
        };
        let snapshot = db::meta::Snapshot::new(
            index_uri,
            index_sha256.clone(),
            u64::try_from(index_bytes.len()).unwrap(),
        )
        .unwrap();
        let immutable = repository::manager::immutable_index_path(&repository, &index_sha256);
        fs_err::create_dir_all(immutable.parent().unwrap()).unwrap();
        fs_err::set_permissions(immutable.parent().unwrap(), std::fs::Permissions::from_mode(0o700)).unwrap();
        fs_err::write(&immutable, index_bytes).unwrap();
        fs_err::set_permissions(&immutable, std::fs::Permissions::from_mode(0o444)).unwrap();
        repository
            .db
            .replace_all_with_snapshot(
                packages.into_iter().map(|package| (package.id, package.meta)).collect(),
                snapshot,
            )
            .unwrap();
        client
    }

    fn metadata_only_package(directory: &std::path::Path, index: usize) -> Package {
        let name = format!("metadata-only-{index:02}");
        let path = directory.join(format!("{name}.stone"));
        let mut package = package(&name, BTreeSet::new());
        package.meta.uri = None;
        package.meta.hash = None;
        package.meta.download_size = None;

        let mut file = fs_err::File::create(&path).unwrap();
        let mut writer = StoneWriter::new(&mut file, StoneHeaderV1FileType::Binary).unwrap();
        let payload = package.meta.clone().to_stone_payload();
        writer.add_payload(payload.as_slice()).unwrap();
        writer.finalize().unwrap();
        drop(file);

        let bytes = fs_err::read(&path).unwrap();
        let id = hex::encode(Sha256::digest(&bytes));
        package.id = package::Id::from(id.clone());
        package.meta.hash = Some(id);
        package.meta.uri = Some(url::Url::from_file_path(&path).unwrap().to_string());
        package.meta.download_size = Some(u64::try_from(bytes.len()).unwrap());
        package
    }

    #[test]
    fn frozen_resolution_uses_only_exact_ids_without_dependency_recomposition() {
        let temporary = tempfile::tempdir().unwrap();
        let dependency = package("dependency", BTreeSet::new());
        let root = package("root", BTreeSet::from([Dependency::package_name("dependency")]));
        let client = frozen_client(temporary.path(), vec![dependency, root.clone()]);

        let resolved = resolve_exact_packages(&client, std::slice::from_ref(&root.id)).unwrap();

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, root.id);
        assert!(
            resolved[0]
                .meta
                .dependencies
                .contains(&Dependency::package_name("dependency"))
        );
    }

    #[test]
    fn frozen_materialization_rejects_stateful_scope_before_registry_or_root_side_effects() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("marker");
        fs_err::write(&marker, b"unchanged").unwrap();
        let mut client = stateful_client(temporary.path(), Vec::new());
        let missing = package::Id::from("missing");

        let error = match materialize_frozen_root(&mut client, &[missing], 1_700_000_000) {
            Ok(_) => panic!("stateful frozen materialization unexpectedly succeeded"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            Error::Client(client::Error::FrozenRootRequiresFrozenClient)
        ));
        assert_eq!(fs_err::read(marker).unwrap(), b"unchanged");
    }

    #[test]
    fn frozen_package_ids_are_sorted_and_duplicates_fail_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let client = stateful_client(temporary.path(), Vec::new());
        let a = package::Id::from("a");
        let z = package::Id::from("z");

        assert_eq!(
            client.canonical_frozen_package_ids(&[z.clone(), a.clone()]).unwrap(),
            [a.clone(), z]
        );
        assert!(matches!(
            client.canonical_frozen_package_ids(&[a.clone(), a.clone()]),
            Err(client::Error::DuplicateFrozenPackage(found)) if found == a
        ));
    }

    #[test]
    fn public_frozen_materialization_ignores_ambient_active_and_cobble_candidates() {
        const STONE: &[u8] = include_bytes!("../../../../tests/fixtures/bash-completion-2.11-1-1-x86_64.stone");
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let intent_path = system_model::intent_path(&installation_root);
        fs_err::create_dir_all(intent_path.parent().unwrap()).unwrap();
        fs_err::write(&intent_path, b"this is deliberately invalid Gluon").unwrap();
        fs_err::create_dir_all(installation_root.join("usr")).unwrap();
        fs_err::write(installation_root.join("usr/.stateID"), b"42").unwrap();

        let cobble_path = temporary.path().join("ambient.stone");
        fs_err::write(&cobble_path, STONE).unwrap();
        let mut cobble = plugin::Cobble::default();
        let id = package::Id::from(cobble.add_package(cobble_path).unwrap());
        let ambient = cobble.package(&id).unwrap();

        let mut explicit = package(id.as_str(), BTreeSet::new());
        explicit.meta.hash = Some("different-repository-identity".to_owned());
        let mut client = frozen_client(temporary.path(), vec![explicit]);
        assert!(client.installation.active_state.is_none());
        assert!(client.installation.system_model.is_none());

        client.install_db.add(id.clone(), ambient.meta.clone()).unwrap();
        client.registry.add_plugin(plugin::Plugin::Cobble(cobble));
        client.registry.add_plugin(plugin::Plugin::Active(plugin::Active::new(
            Some(State {
                id: state::Id::from(42),
                summary: None,
                description: None,
                selections: vec![Selection::explicit(id.clone())],
                created: chrono::Utc::now(),
                kind: state::Kind::Transaction,
            }),
            client.install_db.clone(),
        )));

        // A normal registry query is now dominated by the injected ambient
        // sources, proving the regression setup is meaningful.
        assert_eq!(
            client.resolve_package(&id).unwrap().meta.name.as_str(),
            "bash-completion"
        );
        let marker = temporary.path().join("frozen-root/untouched");
        fs_err::write(&marker, b"before").unwrap();

        let error = match client.materialize_frozen_root(std::slice::from_ref(&id), 1_700_000_000) {
            Ok(_) => panic!("ambient registry candidate crossed the frozen boundary"),
            Err(error) => error,
        };
        let client::Error::Install(source) = error else {
            panic!("unexpected frozen materialization error: {error}");
        };
        let Error::FrozenPackageIdentityMismatch {
            requested,
            metadata_hash,
        } = *source
        else {
            panic!("unexpected frozen install error: {source}");
        };
        assert_eq!(requested, id);
        assert_eq!(metadata_hash.as_deref(), Some("different-repository-identity"));
        assert_eq!(fs_err::read(marker).unwrap(), b"before");
    }

    #[test]
    fn metadata_only_frozen_closure_publishes_without_an_asset_pool() {
        let temporary = tempfile::tempdir().unwrap();
        let package_dir = temporary.path().join("metadata-only-stones");
        fs_err::create_dir(&package_dir).unwrap();
        let packages = (0..15)
            .map(|index| metadata_only_package(&package_dir, index))
            .collect::<Vec<_>>();
        let ids = packages.iter().map(|package| package.id.clone()).collect::<Vec<_>>();
        let mut client = frozen_client(temporary.path(), packages);

        // The helper creates the destination because older frozen-client
        // construction required it. Production materialization is strictly
        // absent-only, so remove it before invoking the public entry point.
        client.discard_frozen_root().unwrap();
        let asset_pool = client.installation.assets_path("v2");
        if asset_pool.exists() {
            fs_err::remove_dir_all(&asset_pool).unwrap();
        }
        assert!(!asset_pool.exists());

        let materialization = client.materialize_frozen_root(&ids, 1_700_000_000).unwrap();

        let frozen_root = temporary.path().join("frozen-root");
        let root_metadata = fs_err::symlink_metadata(&frozen_root).unwrap();
        assert!(root_metadata.is_dir());
        assert_eq!(root_metadata.permissions().mode() & 0o7777, 0o755);
        for (source, target) in super::super::ROOT_ABI_LINKS {
            assert_eq!(
                fs_err::read_link(frozen_root.join(target)).unwrap(),
                std::path::Path::new(source)
            );
        }
        assert!(
            !asset_pool.exists(),
            "a metadata-only closure must not synthesize an unused asset pool"
        );
        assert!(
            fs_err::read_dir(temporary.path()).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".forge-frozen-stage-")),
            "atomic publication must not leak its private stage wrapper"
        );

        let retained_root = temporary.path().join("retained-frozen-root");
        fs_err::rename(&frozen_root, &retained_root).unwrap();
        fs_err::create_dir(&frozen_root).unwrap();
        fs_err::set_permissions(&frozen_root, std::fs::Permissions::from_mode(0o755)).unwrap();
        fs_err::write(frozen_root.join("replacement-marker"), b"must remain untouched").unwrap();

        assert!(matches!(
            materialization.root.revalidate(),
            Err(client::Error::MaterializedFrozenRootReplaced(path)) if path == frozen_root
        ));
        assert!(matches!(
            client.require_materialized_frozen_executables(materialization.root, &ids, &[]),
            Err(client::Error::MaterializedFrozenRootReplaced(path)) if path == frozen_root
        ));
        assert_eq!(
            fs_err::read(frozen_root.join("replacement-marker")).unwrap(),
            b"must remain untouched"
        );
        assert!(fs_err::read_dir(&frozen_root).unwrap().count() == 1);
    }

    #[test]
    fn frozen_client_rejects_other_mutating_apis_before_side_effects() {
        let temporary = tempfile::tempdir().unwrap();
        let mut client = frozen_client(temporary.path(), Vec::new());
        let marker = temporary.path().join("frozen-root/untouched");
        fs_err::write(&marker, b"before").unwrap();
        let package = package::Id::from("missing".to_owned());

        assert!(matches!(
            client.install(&["missing"], true, false),
            Err(client::Error::FrozenClientProhibitedOperation)
        ));
        assert!(matches!(
            client.install_exact(std::slice::from_ref(&package), true, false),
            Err(client::Error::FrozenClientProhibitedOperation)
        ));
        assert!(matches!(
            client.remove(&["missing"], true, false),
            Err(client::Error::FrozenClientProhibitedOperation)
        ));
        assert!(matches!(
            client.sync(true, false),
            Err(client::Error::FrozenClientProhibitedOperation)
        ));
        assert!(matches!(
            runtime::block_on(client.ensure_repos_initialized()),
            Err(client::Error::FrozenClientProhibitedOperation)
        ));
        assert!(matches!(
            runtime::block_on(client.refresh_repositories()),
            Err(client::Error::FrozenClientProhibitedOperation)
        ));
        assert_eq!(fs_err::read(marker).unwrap(), b"before");
    }
}
