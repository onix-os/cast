//! Independent namespace proof for routing one preserved ActivateArchived
//! candidate to rollback completion.
//!
//! This proof is read-only and phase specific. It retains both sides of its
//! admission sandwich and requires a fresh matching capture on every
//! revalidation. The accepted namespace is only the already-preserved
//! canonical archived-state slot; it is not the NewState quarantine or
//! ActiveReblit whole-wrapper topology.

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    candidate_preserve_proof::{
        UsrRollbackCandidatePreserveNamespaceError, require_exact_activate_archived_candidate_preserved_topology,
    },
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActivateArchivedCompleteRouteNamespaceInspection {
    before: NamespaceSnapshot,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActivateArchivedCompleteRouteNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
}

impl UsrRollbackActivateArchivedCompleteRouteNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackActivateArchivedCompleteRouteNamespaceError> {
        require_exact_journal(journal, expected)?;
        let before = capture_snapshot(installation, expected)?;
        require_exact_activate_archived_candidate_preserved_topology(expected, &before)?;
        Ok(Self { before })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<
        UsrRollbackActivateArchivedCompleteRouteNamespaceProof,
        UsrRollbackActivateArchivedCompleteRouteNamespaceError,
    > {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_activate_archived_candidate_preserved_topology(expected, &self.before)?;
        require_exact_activate_archived_candidate_preserved_topology(expected, &after)?;
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackActivateArchivedCompleteRouteNamespaceProof {
            before: self.before,
            after,
        })
    }
}

impl UsrRollbackActivateArchivedCompleteRouteNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackActivateArchivedCompleteRouteNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_activate_archived_candidate_preserved_topology(expected, &self.before)?;
        require_exact_activate_archived_candidate_preserved_topology(expected, &self.after)?;
        require_exact_journal(journal, expected)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_activate_archived_candidate_preserved_topology(expected, &fresh)?;

        require_exact_journal(journal, expected)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackActivateArchivedCompleteRouteNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackActivateArchivedCompleteRouteNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackActivateArchivedCompleteRouteNamespaceError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackActivateArchivedCompleteRouteNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackActivateArchivedCompleteRouteNamespaceError {
    #[error("capture or revalidate the exact ActivateArchived rollback-completion namespace")]
    Capture(#[from] CaptureError),
    #[error("prove the exact preserved ActivateArchived canonical-slot topology")]
    Topology(#[from] UsrRollbackCandidatePreserveNamespaceError),
    #[error("read the retained canonical ActivateArchived transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical ActivateArchived transition journal changed during rollback-completion proof")]
    JournalChanged,
    #[error("the ActivateArchived rollback-completion namespace changed during proof")]
    NamespaceChanged,
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FRESH_NAMESPACE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_activate_archived_complete_route_fresh_namespace_capture(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FRESH_NAMESPACE_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_fresh_namespace_capture() {
    BEFORE_FRESH_NAMESPACE_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_fresh_namespace_capture() {}
