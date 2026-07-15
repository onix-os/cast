//! Read-only startup audit for interrupted archived-state pruning.

use std::{ffi::OsString, os::unix::ffi::OsStringExt as _, path::PathBuf};

use thiserror::Error;

use crate::{Installation, installation, transition_journal::TransitionJournalStore};

use super::{QUARANTINE_RELATIVE, RetainedDirectory};

const PRUNE_RESIDUE_PREFIX: &[u8] = b"state-prune-";
const MAX_QUARANTINE_ENTRIES: usize = 4_096;

#[cfg(test)]
std::thread_local! {
    static AFTER_FIRST_SCAN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_after_archived_state_prune_residue_first_scan(hook: impl FnOnce() + 'static) {
    AFTER_FIRST_SCAN.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "archived-state prune residue scan hook already armed"
        );
    });
}

#[cfg(test)]
fn after_first_scan() {
    AFTER_FIRST_SCAN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_first_scan() {}

/// Reject evidence left by an interrupted archived-state prune.
///
/// The caller supplies the retained journal guard so the clean journal
/// observation and this namespace audit belong to one cooperating-writer
/// critical section. Ambient residue is never adopted, repaired, or removed.
pub(crate) fn audit_archived_state_prune_residue(
    installation: &Installation,
    _journal: &TransitionJournalStore,
) -> Result<(), ArchivedStatePruneResidueError> {
    installation.revalidate_root_directory()?;
    let path = installation.state_quarantine_dir();
    let quarantine = RetainedDirectory::open_beneath(installation.root_directory(), QUARANTINE_RELATIVE, path.clone())?;
    quarantine.require_retained()?;

    let mut first = quarantine.entries(MAX_QUARANTINE_ENTRIES)?;
    reject_residue(&path, &first)?;
    first.sort();
    after_first_scan();

    // A second bounded enumeration turns a cooperating namespace change into
    // explicit startup failure rather than a clean observation. The retained
    // journal lock already serializes every Cast transition writer.
    let mut second = quarantine.entries(MAX_QUARANTINE_ENTRIES)?;
    reject_residue(&path, &second)?;
    second.sort();
    if first != second {
        return Err(ArchivedStatePruneResidueError::QuarantineChanged { path });
    }

    quarantine.require_retained()?;
    quarantine.revalidate_beneath(installation.root_directory(), QUARANTINE_RELATIVE)?;
    installation.revalidate_root_directory()?;
    Ok(())
}

fn reject_residue(path: &std::path::Path, entries: &[Vec<u8>]) -> Result<(), ArchivedStatePruneResidueError> {
    if let Some(name) = entries.iter().find(|name| name.starts_with(PRUNE_RESIDUE_PREFIX)) {
        return Err(ArchivedStatePruneResidueError::Residue {
            path: path.join(OsString::from_vec(name.clone())),
        });
    }
    Ok(())
}

#[derive(Debug, Error)]
pub(crate) enum ArchivedStatePruneResidueError {
    #[error("authenticate archived-state prune quarantine")]
    Namespace(#[from] super::Error),
    #[error("revalidate installation around archived-state prune residue audit")]
    Installation(#[from] installation::Error),
    #[error("archived-state prune residue requires manual recovery: {}", path.display())]
    Residue { path: PathBuf },
    #[error("archived-state prune quarantine changed during startup audit: {}", path.display())]
    QuarantineChanged { path: PathBuf },
}
