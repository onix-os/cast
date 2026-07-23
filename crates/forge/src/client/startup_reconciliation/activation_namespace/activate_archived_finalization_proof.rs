//! Independent terminal namespace proof for ActivateArchived rollback
//! finalization.
//!
//! This proof retains both sides of the exact archived canonical-slot
//! topology sandwich. Revalidation requires a fresh matching capture. After
//! terminal deletion it consumes itself while proving public journal absence
//! before and after that fresh capture through the same retained store.

use crate::{
    Installation,
    transition_journal::{
        StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    candidate_preserve_proof::{
        UsrRollbackCandidatePreserveNamespaceError, require_exact_activate_archived_rollback_complete_topology,
    },
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActivateArchivedFinalizationNamespaceInspection {
    before: NamespaceSnapshot,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActivateArchivedFinalizationNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
}

impl UsrRollbackActivateArchivedFinalizationNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackActivateArchivedFinalizationNamespaceError> {
        require_exact_journal(installation, journal, binding, expected)?;
        let before = capture_snapshot(installation, expected)?;
        require_exact_activate_archived_rollback_complete_topology(expected, &before)?;
        Ok(Self { before })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<
        UsrRollbackActivateArchivedFinalizationNamespaceProof,
        UsrRollbackActivateArchivedFinalizationNamespaceError,
    > {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_activate_archived_rollback_complete_topology(expected, &self.before)?;
        require_exact_activate_archived_rollback_complete_topology(expected, &after)?;
        require_exact_journal(installation, journal, binding, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackActivateArchivedFinalizationNamespaceProof {
            before: self.before,
            after,
        })
    }
}

impl UsrRollbackActivateArchivedFinalizationNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackActivateArchivedFinalizationNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_activate_archived_rollback_complete_topology(expected, &self.before)?;
        require_exact_activate_archived_rollback_complete_topology(expected, &self.after)?;
        require_exact_journal(installation, journal, binding, expected)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_activate_archived_rollback_complete_topology(expected, &fresh)?;

        require_exact_journal(installation, journal, binding, expected)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    pub(in crate::client::startup_reconciliation) fn revalidate_after_journal_delete(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackActivateArchivedFinalizationNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        require_exact_public_journal_absence(installation, journal)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_activate_archived_rollback_complete_topology(expected, &self.before)?;
        require_exact_activate_archived_rollback_complete_topology(expected, &self.after)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_activate_archived_rollback_complete_topology(expected, &fresh)?;

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
) -> Result<(), UsrRollbackActivateArchivedFinalizationNamespaceError> {
    let cast = installation.retained_mutable_cast_directory()?;
    match journal.load_revalidated_retained_cast(cast)? {
        None => Ok(()),
        Some(_) => Err(UsrRollbackActivateArchivedFinalizationNamespaceError::JournalChanged),
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackActivateArchivedFinalizationNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackActivateArchivedFinalizationNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackActivateArchivedFinalizationNamespaceError> {
    if !journal.has_record_store_binding(binding) {
        return Err(UsrRollbackActivateArchivedFinalizationNamespaceError::JournalChanged);
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, binding, expected)? {
        Ok(())
    } else {
        Err(UsrRollbackActivateArchivedFinalizationNamespaceError::JournalChanged)
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackActivateArchivedFinalizationNamespaceError {
    #[error("capture or revalidate the exact ActivateArchived rollback-finalization namespace")]
    Capture(#[from] CaptureError),
    #[error("prove the exact terminal ActivateArchived canonical-slot topology")]
    Topology(#[from] UsrRollbackCandidatePreserveNamespaceError),
    #[error("read the retained canonical ActivateArchived transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical ActivateArchived transition journal changed during rollback finalization")]
    JournalChanged,
    #[error("the ActivateArchived rollback-finalization namespace changed during proof")]
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
pub(in crate::client) fn arm_before_usr_rollback_activate_archived_finalization_fresh_namespace_capture(
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
