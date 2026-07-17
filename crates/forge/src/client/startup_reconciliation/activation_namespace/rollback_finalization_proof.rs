//! Independent terminal namespace proof for NewState rollback finalization.
//!
//! This proof is read-only and phase-specific. It retains both sides of its
//! admission sandwich and requires a fresh matching capture whenever the
//! enclosing finalization authority is revalidated. It cannot be constructed
//! from `FreshDbInvalidated` routing authority.

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    candidate_preserve_proof::{
        UsrRollbackCandidatePreserveNamespaceError, require_exact_new_state_rollback_complete_topology,
    },
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackFinalizationNamespaceInspection {
    before: NamespaceSnapshot,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackFinalizationNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
}

impl UsrRollbackFinalizationNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackFinalizationNamespaceError> {
        require_exact_journal(journal, expected)?;
        let before = capture_snapshot(installation, expected)?;
        require_exact_new_state_rollback_complete_topology(expected, &before)?;
        Ok(Self { before })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<UsrRollbackFinalizationNamespaceProof, UsrRollbackFinalizationNamespaceError> {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_new_state_rollback_complete_topology(expected, &self.before)?;
        require_exact_new_state_rollback_complete_topology(expected, &after)?;
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackFinalizationNamespaceProof {
            before: self.before,
            after,
        })
    }
}

impl UsrRollbackFinalizationNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackFinalizationNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_new_state_rollback_complete_topology(expected, &self.before)?;
        require_exact_new_state_rollback_complete_topology(expected, &self.after)?;
        require_exact_journal(journal, expected)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_new_state_rollback_complete_topology(expected, &fresh)?;

        require_exact_journal(journal, expected)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    /// Consume the terminal namespace proof after the exact journal record
    /// has been deleted.
    ///
    /// The record payload remains the policy input, but absence is now the
    /// only accepted public journal state.  The same retained store must still
    /// own both the public journal directory and its public lock name around a
    /// fresh terminal namespace capture.  No journal directory, lock, record,
    /// or namespace entry is created or repaired here.
    pub(in crate::client::startup_reconciliation) fn revalidate_after_journal_delete(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackFinalizationNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        require_exact_public_journal_absence(installation, journal)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_new_state_rollback_complete_topology(expected, &self.before)?;
        require_exact_new_state_rollback_complete_topology(expected, &self.after)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_new_state_rollback_complete_topology(expected, &fresh)?;

        require_exact_public_journal_absence(installation, journal)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn require_exact_public_journal_absence(
    installation: &Installation,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackFinalizationNamespaceError> {
    let cast = installation.retained_mutable_cast_directory()?;
    match journal.load_revalidated_retained_cast(cast)? {
        None => Ok(()),
        Some(_) => Err(UsrRollbackFinalizationNamespaceError::JournalChanged),
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackFinalizationNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackFinalizationNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackFinalizationNamespaceError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackFinalizationNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackFinalizationNamespaceError {
    #[error("capture or revalidate the exact rollback-finalization namespace")]
    Capture(#[from] CaptureError),
    #[error("prove the exact NewState RollbackComplete namespace topology")]
    Topology(#[from] UsrRollbackCandidatePreserveNamespaceError),
    #[error("read the retained canonical transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical transition journal changed during rollback-finalization proof")]
    JournalChanged,
    #[error("the rollback-finalization namespace changed during proof")]
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
pub(in crate::client) fn arm_before_usr_rollback_finalization_fresh_namespace_capture(hook: impl FnOnce() + 'static) {
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
