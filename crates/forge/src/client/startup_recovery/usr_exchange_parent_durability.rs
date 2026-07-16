//! Complete the two parent-directory durability barriers for exact
//! `UsrExchangeIntent + POST` startup evidence.
//!
//! This executor never renames or retries an exchange. It borrows the same
//! open journal to preserve per-open identity, consumes its evidence authority
//! on every result, and returns ordinary rollback-decision authority only after
//! staging-parent sync, installation-root sync, and a final full evidence
//! sandwich have all succeeded.

use std::io;

use thiserror::Error;

use crate::{installation, transition_journal::TransitionJournalStore};

use super::super::startup_reconciliation::{
    UsrExchangeParentDurabilityAuthority, UsrRollbackDecisionAuthority, UsrRollbackDecisionAuthorityError,
};

#[cfg(test)]
mod tests;

/// Safe code cannot manufacture proof that parent durability normalization
/// completed. The sole constructor remains private to this executor.
pub(in crate::client) struct UsrExchangeParentDurabilityCompletionSeal {
    _private: (),
}

impl UsrExchangeParentDurabilityCompletionSeal {
    fn new() -> Self {
        Self { _private: () }
    }
}

/// Sync both exact rename parents in the production order and convert the
/// consumed Intent+POST typestate into ordinary rollback-decision authority.
pub(in crate::client) fn normalize_usr_exchange_parent_durability<'reservation>(
    journal: &TransitionJournalStore,
    authority: UsrExchangeParentDurabilityAuthority<'reservation>,
) -> Result<UsrRollbackDecisionAuthority<'reservation>, UsrExchangeParentDurabilityError> {
    // This shared revalidation begins with exact per-open journal binding.
    // Nothing may inspect or sync either parent before it succeeds.
    authority.revalidate(journal)?;

    // This method revalidates both retained inventories immediately around
    // syncing the captured `.cast/root/staging` descriptor.
    let (staging_device, staging_inode) =
        authority.sync_retained_staging_parent(|| before_usr_exchange_parent_durability_staging_parent_sync())?;
    record_usr_exchange_parent_durability_staging_parent_synced(staging_device, staging_inode);

    let installation = authority.installation();
    installation.revalidate_root_directory()?;
    require_boundary(DurabilityBoundary::InstallationRootSync)?;
    installation.root_directory().sync_all().map_err(|source| {
        UsrExchangeParentDurabilityError::InstallationRootSync {
            path: installation.root.clone(),
            source,
        }
    })?;
    record_installation_root_synced(installation.root_directory());
    installation.revalidate_root_directory()?;
    installation.revalidate_mutable_namespace()?;

    require_boundary(DurabilityBoundary::FinalEvidenceRevalidation)?;
    run_before_final_evidence_revalidation();
    authority.revalidate(journal)?;
    record_final_evidence_revalidated();

    authority
        .complete(UsrExchangeParentDurabilityCompletionSeal::new())
        .map_err(UsrExchangeParentDurabilityError::from)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurabilityBoundary {
    StagingParentSync,
    InstallationRootSync,
    FinalEvidenceRevalidation,
}

/// Called by the retained staging capability immediately before its sole
/// `sync_all`. In production this is a no-op; tests may fail that exact call
/// boundary without exposing reusable normalization authority.
fn before_usr_exchange_parent_durability_staging_parent_sync() -> io::Result<()> {
    if boundary_is_faulted(DurabilityBoundary::StagingParentSync) {
        Err(io::Error::other(
            "injected startup /usr retained staging-parent sync failure",
        ))
    } else {
        Ok(())
    }
}

fn require_boundary(boundary: DurabilityBoundary) -> Result<(), UsrExchangeParentDurabilityError> {
    if boundary_is_faulted(boundary) {
        #[cfg(test)]
        return Err(UsrExchangeParentDurabilityError::InjectedFault {
            point: fault_point(boundary),
        });
        #[cfg(not(test))]
        unreachable!("production parent-durability boundaries cannot be faulted");
    }
    Ok(())
}

#[cfg(test)]
fn boundary_is_faulted(boundary: DurabilityBoundary) -> bool {
    USR_EXCHANGE_PARENT_DURABILITY_FAULT.with(|slot| {
        if slot.get() == Some(fault_point(boundary)) {
            slot.set(None);
            true
        } else {
            false
        }
    })
}

#[cfg(not(test))]
fn boundary_is_faulted(_boundary: DurabilityBoundary) -> bool {
    false
}

/// Record a successful sync without adding a production observation or a
/// pathname lookup.
fn record_usr_exchange_parent_durability_staging_parent_synced(device: u64, inode: u64) {
    #[cfg(test)]
    record_event(UsrExchangeParentDurabilityEvent::StagingParentSynced { device, inode });
    #[cfg(not(test))]
    let _ = (device, inode);
}

#[cfg(test)]
fn record_installation_root_synced(root: &std::fs::File) {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = root
        .metadata()
        .expect("inspect retained installation-root descriptor after successful sync");
    record_event(UsrExchangeParentDurabilityEvent::InstallationRootSynced {
        device: metadata.dev(),
        inode: metadata.ino(),
    });
}

#[cfg(not(test))]
fn record_installation_root_synced(_root: &std::fs::File) {}

fn record_final_evidence_revalidated() {
    #[cfg(test)]
    record_event(UsrExchangeParentDurabilityEvent::FinalEvidenceRevalidated);
}

#[cfg(test)]
fn record_event(event: UsrExchangeParentDurabilityEvent) {
    USR_EXCHANGE_PARENT_DURABILITY_EVENTS.with(|events| events.borrow_mut().push(event));
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrExchangeParentDurabilityError {
    #[error("revalidate exact startup /usr parent-durability authority")]
    Authority(#[from] UsrRollbackDecisionAuthorityError),
    #[error("revalidate the retained installation root around parent-durability normalization")]
    Installation(#[from] installation::Error),
    #[error("sync retained installation root during startup /usr durability normalization at `{}`", path.display())]
    InstallationRootSync {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[cfg(test)]
    #[error("injected startup /usr parent-durability fault at {point:?}")]
    InjectedFault {
        point: UsrExchangeParentDurabilityFaultPoint,
    },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UsrExchangeParentDurabilityFaultPoint {
    StagingParentSync,
    InstallationRootSync,
    FinalEvidenceRevalidation,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UsrExchangeParentDurabilityEvent {
    StagingParentSynced { device: u64, inode: u64 },
    InstallationRootSynced { device: u64, inode: u64 },
    FinalEvidenceRevalidated,
}

#[cfg(test)]
fn fault_point(boundary: DurabilityBoundary) -> UsrExchangeParentDurabilityFaultPoint {
    match boundary {
        DurabilityBoundary::StagingParentSync => UsrExchangeParentDurabilityFaultPoint::StagingParentSync,
        DurabilityBoundary::InstallationRootSync => UsrExchangeParentDurabilityFaultPoint::InstallationRootSync,
        DurabilityBoundary::FinalEvidenceRevalidation => {
            UsrExchangeParentDurabilityFaultPoint::FinalEvidenceRevalidation
        }
    }
}

#[cfg(test)]
std::thread_local! {
    static USR_EXCHANGE_PARENT_DURABILITY_FAULT:
        std::cell::Cell<Option<UsrExchangeParentDurabilityFaultPoint>> = const { std::cell::Cell::new(None) };
    static USR_EXCHANGE_PARENT_DURABILITY_EVENTS:
        std::cell::RefCell<Vec<UsrExchangeParentDurabilityEvent>> = const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_FINAL_EVIDENCE_REVALIDATION:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_usr_exchange_parent_durability_fault(point: UsrExchangeParentDurabilityFaultPoint) {
    USR_EXCHANGE_PARENT_DURABILITY_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(point)).is_none(),
            "parent-durability fault already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn reset_usr_exchange_parent_durability_events() {
    USR_EXCHANGE_PARENT_DURABILITY_EVENTS.with(|events| events.borrow_mut().clear());
}

#[cfg(test)]
pub(crate) fn take_usr_exchange_parent_durability_events() -> Vec<UsrExchangeParentDurabilityEvent> {
    USR_EXCHANGE_PARENT_DURABILITY_EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

#[cfg(test)]
pub(crate) fn arm_before_usr_exchange_parent_durability_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_EVIDENCE_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_final_evidence_revalidation() {
    BEFORE_FINAL_EVIDENCE_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_final_evidence_revalidation() {}
