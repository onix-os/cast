//! Independent preserved-wrapper proof for an ActiveReblit
//! `BootRepairRequired` record.
//!
//! The proof is read-only and phase-specific. It authorizes no boot,
//! filesystem, database, or journal mutation; the recovery layer may consume
//! it only to persist the typed `BootRepairStarted` successor.

use crate::{
    Installation,
    transition_journal::{
        StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    candidate_preserve_proof::{
        UsrRollbackCandidatePreserveNamespaceError,
        require_exact_active_reblit_boot_repair_required_topology,
    },
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitBootRepairStartNamespaceInspection {
    before: NamespaceSnapshot,
    wrapper_index: usize,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitBootRepairStartNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    wrapper_index: usize,
}

impl UsrRollbackActiveReblitBootRepairStartNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackActiveReblitBootRepairStartNamespaceError> {
        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        let before = capture_snapshot(installation, expected)?;
        let wrapper_index = require_exact_active_reblit_boot_repair_required_topology(expected, &before)?;
        Ok(Self { before, wrapper_index })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitBootRepairStartNamespaceProof,
        UsrRollbackActiveReblitBootRepairStartNamespaceError,
    > {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_wrapper_index(expected, &self.before, self.wrapper_index)?;
        require_exact_wrapper_index(expected, &after, self.wrapper_index)?;
        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackActiveReblitBootRepairStartNamespaceProof {
            before: self.before,
            after,
            wrapper_index: self.wrapper_index,
        })
    }
}

impl UsrRollbackActiveReblitBootRepairStartNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackActiveReblitBootRepairStartNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_wrapper_index(expected, &self.before, self.wrapper_index)?;
        require_exact_wrapper_index(expected, &self.after, self.wrapper_index)?;
        require_exact_journal(installation, journal, journal_record_binding, expected)?;

        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_wrapper_index(expected, &fresh, self.wrapper_index)?;

        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn require_exact_wrapper_index(
    expected: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    wrapper_index: usize,
) -> Result<(), UsrRollbackActiveReblitBootRepairStartNamespaceError> {
    let actual = require_exact_active_reblit_boot_repair_required_topology(expected, snapshot)?;
    if actual == wrapper_index {
        Ok(())
    } else {
        Err(
            UsrRollbackActiveReblitBootRepairStartNamespaceError::WrapperIndexChanged {
                expected: wrapper_index,
                actual,
            },
        )
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackActiveReblitBootRepairStartNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackActiveReblitBootRepairStartNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    installation: &Installation,
    journal: &TransitionJournalStore,
    journal_record_binding: &TransitionJournalRecordBinding,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackActiveReblitBootRepairStartNamespaceError> {
    if !journal.has_record_store_binding(journal_record_binding) {
        return Err(UsrRollbackActiveReblitBootRepairStartNamespaceError::JournalChanged);
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, journal_record_binding, expected)? {
        Ok(())
    } else {
        Err(UsrRollbackActiveReblitBootRepairStartNamespaceError::JournalChanged)
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackActiveReblitBootRepairStartNamespaceError {
    #[error("capture or revalidate the exact ActiveReblit BootRepairRequired namespace")]
    Capture(#[from] CaptureError),
    #[error("prove the exact preserved ActiveReblit whole-wrapper topology at BootRepairRequired")]
    Topology(#[from] UsrRollbackCandidatePreserveNamespaceError),
    #[error("read the retained canonical ActiveReblit BootRepairRequired journal")]
    Journal(#[from] StorageError),
    #[error("the retained ActiveReblit BootRepairRequired journal changed during namespace proof")]
    JournalChanged,
    #[error("the ActiveReblit BootRepairRequired namespace changed during proof")]
    NamespaceChanged,
    #[error("the ActiveReblit replacement-wrapper index changed from {expected} to {actual}")]
    WrapperIndexChanged { expected: usize, actual: usize },
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}
