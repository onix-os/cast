// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! The pruning system for Cast states and assets
//!
//! Quite simply this is a strategy based garbage collector for unused/unwanted
//! system states (i.e. historical snapshots) that cleans up database entries
//! and assets on disk by way of refcounting.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use std::{
    io,
    path::{Path, PathBuf},
};

use fs_err as fs;
use itertools::Itertools;
use thiserror::Error;

use tracing::info;
use tui::Styled;
use tui::{
    dialoguer::{Confirm, theme::ColorfulTheme},
    pretty::autoprint_columns,
};

use crate::client::boot;
use crate::{
    Client, Installation, State,
    client::cache,
    db, package, repository, state,
    transition_identity::{ArchivedStatePruneError, MAX_ARCHIVED_STATE_PRUNE_BATCH, RetainedArchivedStatePrune},
};

/// The prune strategy for removing old states
#[derive(Debug, Clone, Copy)]
pub enum Strategy<'a> {
    /// Keep the most recent N states, remove the rest
    KeepRecent { keep: u64, include_newer: bool },
    /// Removes state(s)
    Remove(&'a [state::Id]),
}

/// Prune old states using [`Strategy`] and garbage collect
/// all cached data related to those states being removed
pub(super) fn prune_states(
    client: &Client,
    strategy: Strategy<'_>,
    yes: bool,
    active_state: &super::active_state_snapshot::ActiveStateLease,
) -> Result<(), super::Error> {
    match prune_states_inner(client, strategy, yes, active_state) {
        Ok(()) => Ok(()),
        Err(Error::ActiveState { source }) => Err(*source),
        Err(source) => Err(super::Error::Prune(source)),
    }
}

fn prune_states_inner(
    client: &Client,
    strategy: Strategy<'_>,
    yes: bool,
    active_state: &super::active_state_snapshot::ActiveStateLease,
) -> Result<(), Error> {
    let installation = &client.installation;
    let layout_db = &client.layout_db;
    let state_db = &client.state_db;
    let install_db = &client.install_db;

    let mut timing = Timing::default();
    let mut instant = Instant::now();

    // Only prune if the Cast root has an active state (otherwise
    // it's probably borked or not setup yet)
    let Some(current_state_id) = active_state.active() else {
        return Err(Error::NoActiveState);
    };
    let current_state = state_db.get(current_state_id)?;

    let state_ids = state_db.list_ids()?;

    // Find each state we need to remove
    let removal_ids = match strategy {
        Strategy::KeepRecent { keep, include_newer } => {
            // Filter for all removal candidates
            let candidates = state_ids
                .iter()
                .filter(|(id, _)| {
                    if include_newer {
                        *id != current_state.id
                    } else {
                        *id < current_state.id
                    }
                })
                .collect::<Vec<_>>();
            // Deduct current state from num candidates to keep
            let candidate_limit = (keep as usize).saturating_sub(1);

            // Calculate how many candidate states over the limit we are
            let num_to_remove = candidates.len().saturating_sub(candidate_limit);

            // Sort ascending and assign first `num_to_remove` as `Status::Remove`
            candidates
                .into_iter()
                .sorted_by_key(|(_, created)| *created)
                .enumerate()
                .filter_map(|(idx, (id, _))| if idx < num_to_remove { Some(*id) } else { None })
                .collect::<Vec<_>>()
        }
        Strategy::Remove(remove) => state_ids
            .iter()
            .filter_map(|(id, _)| remove.contains(id).then_some(*id))
            .collect(),
    };

    // Bail if there's no states to remove
    if removal_ids.is_empty() {
        // TODO: Print no states to be removed
        return Ok(());
    }
    if removal_ids.len() > MAX_ARCHIVED_STATE_PRUNE_BATCH {
        return Err(Error::PruneBatchTooLarge {
            actual: removal_ids.len(),
            limit: MAX_ARCHIVED_STATE_PRUNE_BATCH,
        });
    }

    let mut removals = vec![];

    // Load exact state snapshots for the retained prune session. Package
    // garbage-collection candidates are deliberately recomputed only after
    // the state rows and exact quarantined wrappers are gone.
    for (id, _) in state_ids {
        let state = state_db.get(id)?;
        if removal_ids.contains(&id) {
            if id == current_state.id {
                return Err(Error::PruneCurrent);
            }
            removals.push(state);
        }
    }

    timing.resolve = instant.elapsed();
    info!(
        total_resolved_states = removals.len(),
        resolve_time_ms = timing.resolve.as_millis(),
        "Resolved states marked for removal"
    );
    instant = Instant::now();

    // Print out the states to be removed to the user
    println!("The following state(s) will be removed:");
    println!();
    autoprint_columns(&removals.iter().map(state::ColumnDisplay).collect::<Vec<_>>());
    println!();

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

    // Authenticate every canonical archive and reserve every exact private
    // quarantine while both the active-state coordinator and retained journal
    // lock are held. No database or boot evidence has changed yet.
    revalidate_active_state(active_state, installation)?;
    let mut archived = RetainedArchivedStatePrune::prepare(installation, state_db, &removals)?;
    let boot_exclusions = removals.iter().map(|state| state.id).collect::<BTreeSet<_>>();

    info!(
        total_archived_paths = removals.len(),
        progress = 0.0,
        event_type = "progress_start",
        "Detaching stale archive trees"
    );

    if let Err(primary) = revalidate_active_state(active_state, installation) {
        return Err(restore_archives_after_active_state_failure(
            &mut archived,
            installation,
            primary,
            "revalidate active state after archived-state prune preparation",
        ));
    }
    let detached = match archived.detach_all(installation, state_db) {
        Ok(detached) => detached,
        Err(primary) => {
            return Err(restore_archives_before_boot_change(
                &mut archived,
                installation,
                primary,
                "detach archived-state wrappers",
            ));
        }
    };

    // Boot sees the exact desired post-prune projection while the state rows
    // still exist and the removed wrappers are privately detached. Explicit
    // exclusions are applied before the bounded rollback selection and again
    // while materializing entries; path absence is never prune intent. A
    // failure restores both wrappers and the prior boot projection.
    if let Err(primary) = revalidate_active_state(active_state, installation) {
        return Err(restore_archives_after_active_state_failure(
            &mut archived,
            installation,
            primary,
            "revalidate active state after archived-state wrapper detachment",
        ));
    }
    match boot::synchronize_excluding(client, &current_state, None, &boot_exclusions) {
        Ok(boot::ProjectionSyncOutcome::Applied) => {}
        Ok(boot::ProjectionSyncOutcome::NotApplicable) => {
            return Err(restore_boot_and_archives(
                &mut archived,
                installation,
                client,
                &current_state,
                boot::Error::ExactProjectionSkipped,
            ));
        }
        Ok(outcome) => unreachable!("non-success boot projection returned as success: {outcome:?}"),
        Err(primary) => {
            return Err(restore_boot_and_archives(
                &mut archived,
                installation,
                client,
                &current_state,
                primary,
            ));
        }
    }

    if let Err(primary) = revalidate_active_state(active_state, installation) {
        return Err(restore_archives_and_boot_after_active_state_failure(
            &mut archived,
            installation,
            client,
            &current_state,
            primary,
            "revalidate active state after archived-state boot projection",
        ));
    }
    if let Err(primary) = archived.remove_database_rows(installation, state_db) {
        let not_applied = matches!(
            &primary,
            ArchivedStatePruneError::StateDatabase(source) if source.definitely_not_applied()
        );
        if not_applied {
            return Err(restore_archives_and_boot_after_failure(
                &mut archived,
                installation,
                client,
                &current_state,
                primary,
                "remove exact archived-state database rows",
            ));
        }
        return Err(primary.into());
    }

    timing.prune_db = instant.elapsed();
    info!(
        prune_db_time_ms = timing.prune_db.as_millis(),
        "Durably detached archives and removed exact state rows"
    );
    instant = Instant::now();

    archived.delete_detached(installation)?;

    timing.prune_archives = instant.elapsed();
    info!(
        duration_ms = timing.prune_archives.as_millis(),
        items_processed = detached.len(),
        progress = 1.0,
        event_type = "progress_completed",
    );

    for state in &detached {
        println!(
            "{} {:?}",
            "Removed".green(),
            installation.root_path(state.state.to_string())
        );
    }

    // Only now derive package GC from the remaining authoritative state DB.
    // A partial archive failure can therefore never invalidate package/layout
    // rows or CAS data needed by an unpruned state.
    let package_removals = unreferenced_removed_packages(&removals, state_db)?;
    prune_package_databases(&package_removals, install_db, layout_db)?;

    remove_orphaned_files(
        installation.cache_path("downloads").join("v1"),
        install_db.file_hashes()?,
        |hash| cache::download_path(installation, &hash).ok(),
    )?;
    remove_orphaned_files(installation.assets_path("v2"), layout_db.file_hashes()?, |hash| {
        Some(cache::asset_path(installation, &hash))
    })?;
    timing.orphaned_files = instant.elapsed();
    info!(
        orphaned_file_time_ms = timing.orphaned_files.as_millis(),
        "Removed unreferenced package metadata and files"
    );

    Ok(())
}

fn revalidate_active_state(
    active_state: &super::active_state_snapshot::ActiveStateLease,
    installation: &Installation,
) -> Result<(), Error> {
    active_state
        .revalidate(installation)
        .map_err(|source| Error::ActiveState {
            source: Box::new(source),
        })
}

fn restore_archives_after_active_state_failure(
    archived: &mut RetainedArchivedStatePrune,
    installation: &Installation,
    primary: Error,
    operation: &'static str,
) -> Error {
    if let Err(rollback) = archived.restore_wrappers(installation) {
        return Error::ArchivedStatePruneRollback {
            operation,
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    if let Err(rollback) = archived.retire_reservations() {
        return Error::ArchivedStatePruneRollback {
            operation,
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    primary
}

fn restore_archives_and_boot_after_active_state_failure(
    archived: &mut RetainedArchivedStatePrune,
    installation: &Installation,
    client: &Client,
    current_state: &State,
    primary: Error,
    operation: &'static str,
) -> Error {
    if let Err(rollback) = archived.restore_wrappers(installation) {
        return Error::ArchivedStatePruneRollback {
            operation,
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    if let Err(rollback) = boot::synchronize(client, current_state, None) {
        return Error::ArchivedStatePruneBootRollback {
            operation,
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    if let Err(rollback) = archived.retire_reservations() {
        return Error::ArchivedStatePruneRollback {
            operation,
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    primary
}

fn restore_archives_before_boot_change(
    archived: &mut RetainedArchivedStatePrune,
    installation: &Installation,
    primary: ArchivedStatePruneError,
    operation: &'static str,
) -> Error {
    if let Err(rollback) = archived.restore_wrappers(installation) {
        return Error::ArchivedStatePruneRollback {
            operation,
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    if let Err(rollback) = archived.retire_reservations() {
        return Error::ArchivedStatePruneRollback {
            operation,
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    Error::ArchivedStatePrune {
        source: Box::new(primary),
    }
}

fn restore_archives_and_boot_after_failure(
    archived: &mut RetainedArchivedStatePrune,
    installation: &Installation,
    client: &Client,
    current_state: &State,
    primary: ArchivedStatePruneError,
    operation: &'static str,
) -> Error {
    if let Err(rollback) = archived.restore_wrappers(installation) {
        return Error::ArchivedStatePruneRollback {
            operation,
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    if let Err(rollback) = boot::synchronize(client, current_state, None) {
        return Error::ArchivedStatePruneBootRollback {
            operation,
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    if let Err(rollback) = archived.retire_reservations() {
        return Error::ArchivedStatePruneRollback {
            operation,
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    Error::ArchivedStatePrune {
        source: Box::new(primary),
    }
}

fn restore_boot_and_archives(
    archived: &mut RetainedArchivedStatePrune,
    installation: &Installation,
    client: &Client,
    current_state: &State,
    primary: boot::Error,
) -> Error {
    if primary.projection_sync_outcome() == boot::ProjectionSyncOutcome::NotApplied {
        if let Err(rollback) = archived.restore_all(installation) {
            return Error::BootArchiveRollback {
                primary: Box::new(primary),
                rollback: Box::new(rollback),
            };
        }
        return Error::SyncBoot(primary);
    }

    if let Err(rollback) = archived.restore_wrappers(installation) {
        return Error::BootArchiveRollback {
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    if let Err(rollback) = boot::synchronize(client, current_state, None) {
        return Error::BootProjectionRollback {
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    if let Err(rollback) = archived.retire_reservations() {
        return Error::BootArchiveRollback {
            primary: Box::new(primary),
            rollback: Box::new(rollback),
        };
    }
    Error::SyncBoot(primary)
}

/// Prune all cached data that isn't related to any states
/// or active repositories. This will remove all downloaded
/// stones & unpacked asset data for packages not in that set.
///
/// # Arguments
///
/// * - `state_db`     - Installation's state database
/// * - `install_db`   - Installation's "installed" database
/// * - `layout_db`    - Installation's layout database
/// * - `installation` - Client specific target filesystem encapsulation
/// * - `repositories` - All configured repositories
pub(super) fn prune_cache(
    state_db: &db::state::Database,
    install_db: &db::meta::Database,
    layout_db: &db::layout::Database,
    installation: &Installation,
    repositories: &repository::Manager,
) -> Result<usize, Error> {
    // Prune all packages from our internal DBs that aren't
    // part of a state or an active repository
    {
        // Packages in all states (active + archived)
        let state_packages = state_db
            .all()?
            .into_iter()
            .flat_map(|state| state.selections.into_iter().map(|selection| selection.package))
            .collect::<BTreeSet<_>>();

        // Packages in all active repos
        let repo_packages = repositories.active_package_ids()?;

        // Keep state + active repo packages
        let packages_to_keep = state_packages.into_iter().chain(repo_packages).collect::<BTreeSet<_>>();

        // Prune packages not in `packages_to_keep` from layout db (layout entries)
        {
            let layout_packages = layout_db.package_ids()?;
            let to_remove = layout_packages.difference(&packages_to_keep);
            layout_db.batch_remove(to_remove)?;
        }

        // Prune packages not in `packages_to_keep` from install db (meta entries)
        {
            let install_packages = install_db.package_ids()?;
            let to_remove = install_packages.difference(&packages_to_keep);
            install_db.batch_remove(to_remove)?;
        }
    }

    let mut num_removed_files = 0;

    // Now we can prune "orphaned package artefacts" / packages artefacts
    // on disk but not defined in our internal dbs
    {
        // Remove orphaned downloads (package stones)
        num_removed_files += remove_orphaned_files(
            // root
            installation.cache_path("downloads").join("v1"),
            // final set of hashes to compare against
            install_db.file_hashes()?,
            // path builder using hash
            |hash| cache::download_path(installation, &hash).ok(),
        )?;

        // Remove orphaned assets (unpacked package assets in CAS)
        num_removed_files += remove_orphaned_files(
            // root
            installation.assets_path("v2"),
            // final set of hashes to compare against
            layout_db.file_hashes()?,
            // path builder using hash
            |hash| Some(cache::asset_path(installation, &hash)),
        )?;
    }

    Ok(num_removed_files)
}

fn prune_package_databases(
    packages: &[package::Id],
    install_db: &db::meta::Database,
    layout_db: &db::layout::Database,
) -> Result<(), Error> {
    install_db.batch_remove(packages)?;
    layout_db.batch_remove(packages)?;
    Ok(())
}

fn unreferenced_removed_packages(removed: &[State], state_db: &db::state::Database) -> Result<Vec<package::Id>, Error> {
    let removed = removed
        .iter()
        .flat_map(|state| state.selections.iter().map(|selection| selection.package.clone()))
        .collect::<BTreeSet<_>>();
    let retained = state_db
        .all()?
        .into_iter()
        .flat_map(|state| state.selections.into_iter().map(|selection| selection.package))
        .collect::<BTreeSet<_>>();
    Ok(removed.difference(&retained).cloned().collect())
}

/// Removes all files under `root` that no longer exist in the provided `final_hashes` set
fn remove_orphaned_files(
    root: PathBuf,
    final_hashes: BTreeSet<String>,
    compute_path: impl Fn(String) -> Option<PathBuf>,
) -> Result<usize, Error> {
    // Compute hashes to remove by (installed - final)
    let installed_hashes = enumerate_file_hashes(&root)?;
    let hashes_to_remove = installed_hashes.difference(&final_hashes);

    // Remove each and it's parent dir if empty
    hashes_to_remove.into_iter().try_fold(0, |acc, hash| {
        // Compute path to file using hash
        let Some(file) = compute_path(hash.clone()) else {
            return Ok(acc);
        };
        let partial = file.with_added_extension("part");

        // Remove if it exists
        if file.exists() {
            fs::remove_file(&file)?;
        }

        // Remove partial file if it exists
        if partial.exists() {
            fs::remove_file(&partial)?;
        }

        // Try to remove leading parent dirs if they're
        // now empty
        if let Some(parent) = file.parent() {
            let _ = remove_empty_dirs(parent, &root);
        }

        Ok(acc + 1)
    })
}

/// Returns all nested files under `root` and parses the file name as a hash
fn enumerate_file_hashes(root: impl AsRef<Path>) -> io::Result<BTreeSet<String>> {
    let files = enumerate_files(root)?;

    let path_to_hash = |path: PathBuf| path.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_owned();

    Ok(files.into_iter().map(path_to_hash).collect())
}

/// Returns all nested files under `root`
fn enumerate_files(root: impl AsRef<Path>) -> io::Result<Vec<PathBuf>> {
    use rayon::prelude::*;

    fn recurse(dir: impl AsRef<Path>) -> io::Result<Vec<PathBuf>> {
        let mut dirs = vec![];
        let mut files = vec![];

        if !dir.as_ref().exists() {
            return Ok(vec![]);
        }

        let contents = fs::read_dir(dir.as_ref())?;

        for entry in contents {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();

            if file_type.is_dir() {
                dirs.push(path);
            } else if file_type.is_file() {
                files.push(path);
            }
        }

        let nested_files = dirs
            .par_iter()
            .map(recurse)
            .try_reduce(Vec::new, |acc, files| Ok(acc.into_iter().chain(files).collect()))?;

        Ok(files.into_iter().chain(nested_files).collect())
    }

    recurse(root)
}

/// Remove all empty folders from `starting` and moving up until `root`
///
/// `root` must be a prefix / ancestor of `starting`
fn remove_empty_dirs(starting: &Path, root: &Path) -> io::Result<()> {
    if !starting.starts_with(root) || !starting.is_dir() || !root.is_dir() {
        return Ok(());
    }

    let mut current = Some(starting);

    while let Some(dir) = current.take() {
        if dir.exists() {
            let is_empty = fs::read_dir(dir)?.count() == 0;

            if !is_empty {
                return Ok(());
            }

            fs::remove_dir(dir)?;
        }

        if let Some(parent) = dir.parent()
            && parent != root
        {
            current = Some(parent);
        }
    }

    Ok(())
}

/// Simple timing information for Prune
#[derive(Default)]
pub struct Timing {
    pub resolve: Duration,
    pub prune_db: Duration,
    pub orphaned_files: Duration,
    pub prune_archives: Duration,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("cancelled")]
    Cancelled,
    #[error("no active state found")]
    NoActiveState,
    #[error("cannot prune the currently active state")]
    PruneCurrent,
    #[error("state prune batch has {actual} states, exceeding the retained limit of {limit}")]
    PruneBatchTooLarge { actual: usize, limit: usize },
    #[error("active-state snapshot changed while pruning")]
    ActiveState {
        #[source]
        source: Box<super::Error>,
    },
    #[error("retained archived-state pruning")]
    ArchivedStatePrune {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("{operation} failed and restoring exact archived wrappers also failed")]
    ArchivedStatePruneRollback {
        operation: &'static str,
        #[source]
        primary: Box<dyn std::error::Error + Send + Sync + 'static>,
        rollback: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("{operation} failed; wrappers were restored but restoring the prior boot projection failed")]
    ArchivedStatePruneBootRollback {
        operation: &'static str,
        #[source]
        primary: Box<dyn std::error::Error + Send + Sync + 'static>,
        rollback: Box<boot::Error>,
    },
    #[error("boot synchronization failed and restoring exact archived wrappers also failed")]
    BootArchiveRollback {
        #[source]
        primary: Box<boot::Error>,
        rollback: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("boot synchronization failed; wrappers were restored but restoring the prior boot projection failed")]
    BootProjectionRollback {
        #[source]
        primary: Box<boot::Error>,
        rollback: Box<boot::Error>,
    },
    #[error("db")]
    DB(#[from] db::Error),
    #[error("repository integrity")]
    Repository(#[from] repository::manager::Error),
    #[error("io")]
    Io(#[from] io::Error),
    #[error("string processing")]
    Dialog(#[from] tui::dialoguer::Error),
    #[error("synchronize boot")]
    SyncBoot(#[source] boot::Error),
}

impl From<ArchivedStatePruneError> for Error {
    fn from(source: ArchivedStatePruneError) -> Self {
        Self::ArchivedStatePrune {
            source: Box::new(source),
        }
    }
}
