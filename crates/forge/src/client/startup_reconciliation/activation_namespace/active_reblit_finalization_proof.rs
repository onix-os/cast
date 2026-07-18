//! Independent terminal namespace proof for ActiveReblit rollback
//! finalization.
//!
//! This read-only proof is phase and operation specific. It retains both
//! sides of its admission sandwich, the exact replacement-wrapper index, and
//! a fresh matching capture on every revalidation. After the one terminal
//! journal deletion, it consumes itself while accepting only authenticated
//! public journal absence around the unchanged preserved whole-wrapper
//! topology.

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    candidate_preserve_proof::{
        UsrRollbackCandidatePreserveNamespaceError, require_exact_active_reblit_rollback_complete_topology,
    },
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitFinalizationNamespaceInspection {
    before: NamespaceSnapshot,
    wrapper_index: usize,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitFinalizationNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    wrapper_index: usize,
}

impl UsrRollbackActiveReblitFinalizationNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackActiveReblitFinalizationNamespaceError> {
        require_exact_journal(journal, expected)?;
        let before = capture_snapshot(installation, expected)?;
        let wrapper_index = require_exact_active_reblit_rollback_complete_topology(expected, &before)?;
        Ok(Self { before, wrapper_index })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<UsrRollbackActiveReblitFinalizationNamespaceProof, UsrRollbackActiveReblitFinalizationNamespaceError>
    {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_wrapper_index(expected, &self.before, self.wrapper_index)?;
        require_exact_wrapper_index(expected, &after, self.wrapper_index)?;
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackActiveReblitFinalizationNamespaceProof {
            before: self.before,
            after,
            wrapper_index: self.wrapper_index,
        })
    }
}

impl UsrRollbackActiveReblitFinalizationNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackActiveReblitFinalizationNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_wrapper_index(expected, &self.before, self.wrapper_index)?;
        require_exact_wrapper_index(expected, &self.after, self.wrapper_index)?;
        require_exact_journal(journal, expected)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_wrapper_index(expected, &fresh, self.wrapper_index)?;

        require_exact_journal(journal, expected)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    /// Consume terminal namespace authority after the exact journal record
    /// has been deleted.
    ///
    /// The source record remains the topology policy input, while public
    /// journal absence is required before and after a fresh capture through
    /// the same retained store. No namespace or journal entry is created or
    /// repaired here.
    pub(in crate::client::startup_reconciliation) fn revalidate_after_journal_delete(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackActiveReblitFinalizationNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        require_exact_public_journal_absence(installation, journal)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_wrapper_index(expected, &self.before, self.wrapper_index)?;
        require_exact_wrapper_index(expected, &self.after, self.wrapper_index)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_wrapper_index(expected, &fresh, self.wrapper_index)?;

        require_exact_public_journal_absence(installation, journal)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::client) fn wrapper_index(&self) -> usize {
        self.wrapper_index
    }
}

fn require_exact_public_journal_absence(
    installation: &Installation,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackActiveReblitFinalizationNamespaceError> {
    let cast = installation.retained_mutable_cast_directory()?;
    match journal.load_revalidated_retained_cast(cast)? {
        None => Ok(()),
        Some(_) => Err(UsrRollbackActiveReblitFinalizationNamespaceError::JournalChanged),
    }
}

fn require_exact_wrapper_index(
    expected: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    wrapper_index: usize,
) -> Result<(), UsrRollbackActiveReblitFinalizationNamespaceError> {
    let actual = require_exact_active_reblit_rollback_complete_topology(expected, snapshot)?;
    if actual == wrapper_index {
        Ok(())
    } else {
        Err(UsrRollbackActiveReblitFinalizationNamespaceError::WrapperIndexChanged {
            expected: wrapper_index,
            actual,
        })
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackActiveReblitFinalizationNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackActiveReblitFinalizationNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackActiveReblitFinalizationNamespaceError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackActiveReblitFinalizationNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackActiveReblitFinalizationNamespaceError {
    #[error("capture or revalidate the exact ActiveReblit rollback-finalization namespace")]
    Capture(#[from] CaptureError),
    #[error("prove the exact terminal ActiveReblit whole-wrapper topology")]
    Topology(#[from] UsrRollbackCandidatePreserveNamespaceError),
    #[error("read the retained canonical ActiveReblit transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical ActiveReblit transition journal changed during rollback finalization")]
    JournalChanged,
    #[error("the ActiveReblit rollback-finalization namespace changed during proof")]
    NamespaceChanged,
    #[error("the ActiveReblit replacement-wrapper index changed from {expected} to {actual}")]
    WrapperIndexChanged { expected: usize, actual: usize },
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FRESH_NAMESPACE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_active_reblit_finalization_fresh_namespace_capture(
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
