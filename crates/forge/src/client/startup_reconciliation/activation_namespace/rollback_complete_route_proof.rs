//! Independent namespace proof for routing one invalidated fresh database to
//! rollback completion.
//!
//! This proof is read-only. It retains both sides of its admission sandwich
//! and requires a fresh matching capture whenever the enclosing authority is
//! revalidated. The accepted namespace is the exact NewState
//! `FreshDbInvalidated` preserved-candidate topology: the candidate is alone
//! in its private transition quarantine, staging is empty, and no target
//! residue, parking, or current-transition state wrapper exists.

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    candidate_preserve_proof::{
        UsrRollbackCandidatePreserveNamespaceError, require_exact_new_state_fresh_db_invalidated_topology,
    },
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackCompleteRouteNamespaceInspection {
    before: NamespaceSnapshot,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackCompleteRouteNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
}

impl UsrRollbackCompleteRouteNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackCompleteRouteNamespaceError> {
        require_exact_journal(journal, expected)?;
        let before = capture_snapshot(installation, expected)?;
        require_exact_new_state_fresh_db_invalidated_topology(expected, &before)?;
        Ok(Self { before })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<UsrRollbackCompleteRouteNamespaceProof, UsrRollbackCompleteRouteNamespaceError> {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_new_state_fresh_db_invalidated_topology(expected, &self.before)?;
        require_exact_new_state_fresh_db_invalidated_topology(expected, &after)?;
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackCompleteRouteNamespaceProof {
            before: self.before,
            after,
        })
    }
}

impl UsrRollbackCompleteRouteNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackCompleteRouteNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_new_state_fresh_db_invalidated_topology(expected, &self.before)?;
        require_exact_new_state_fresh_db_invalidated_topology(expected, &self.after)?;
        require_exact_journal(journal, expected)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_new_state_fresh_db_invalidated_topology(expected, &fresh)?;

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
) -> Result<(), UsrRollbackCompleteRouteNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackCompleteRouteNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackCompleteRouteNamespaceError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackCompleteRouteNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackCompleteRouteNamespaceError {
    #[error("capture or revalidate the exact rollback-completion route namespace")]
    Capture(#[from] CaptureError),
    #[error("prove the exact NewState FreshDbInvalidated namespace topology")]
    Topology(#[from] UsrRollbackCandidatePreserveNamespaceError),
    #[error("read the retained canonical transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical transition journal changed during rollback-completion route proof")]
    JournalChanged,
    #[error("the rollback-completion route namespace changed during proof")]
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
pub(in crate::client) fn arm_before_usr_rollback_complete_route_fresh_namespace_capture(hook: impl FnOnce() + 'static) {
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
