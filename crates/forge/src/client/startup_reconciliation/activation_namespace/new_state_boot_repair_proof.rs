//! Independent namespace proof for routing a preserved NewState candidate
//! through the boot-repair tail.
//!
//! This proof is read-only. It retains both sides of its admission sandwich and
//! takes a fresh matching capture on every revalidation. One proof serves every
//! boot-repair edge: the candidate is quarantined and the predecessor restored
//! before the tail begins, so the namespace is the same preserved topology at
//! each phase and only the journal advances. The phase-specific authorities
//! layered above are solely responsible for distinguishing the plans.

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    candidate_preserve_proof::{
        UsrRollbackCandidatePreserveNamespaceError, require_exact_new_state_boot_repair_topology,
    },
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackNewStateBootRepairNamespaceInspection {
    before: NamespaceSnapshot,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackNewStateBootRepairNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
}

impl UsrRollbackNewStateBootRepairNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackNewStateBootRepairNamespaceError> {
        require_exact_journal(journal, expected)?;
        let before = capture_snapshot(installation, expected)?;
        require_exact_new_state_boot_repair_topology(expected, &before)?;
        Ok(Self { before })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<UsrRollbackNewStateBootRepairNamespaceProof, UsrRollbackNewStateBootRepairNamespaceError> {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_new_state_boot_repair_topology(expected, &after)?;
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackNewStateBootRepairNamespaceProof {
            before: self.before,
            after,
        })
    }
}

impl UsrRollbackNewStateBootRepairNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackNewStateBootRepairNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_new_state_boot_repair_topology(expected, &self.before)?;
        require_exact_new_state_boot_repair_topology(expected, &self.after)?;
        require_exact_journal(journal, expected)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_new_state_boot_repair_topology(expected, &fresh)?;

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
) -> Result<(), UsrRollbackNewStateBootRepairNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackNewStateBootRepairNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackNewStateBootRepairNamespaceError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackNewStateBootRepairNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackNewStateBootRepairNamespaceError {
    #[error("capture or revalidate the exact NewState boot-repair namespace")]
    Capture(#[from] CaptureError),
    #[error("prove the exact preserved NewState topology")]
    Topology(#[from] UsrRollbackCandidatePreserveNamespaceError),
    #[error("read the retained canonical NewState transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical NewState transition journal changed during boot-repair proof")]
    JournalChanged,
    #[error("the NewState boot-repair namespace changed during proof")]
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
pub(in crate::client) fn arm_before_usr_rollback_new_state_boot_repair_fresh_namespace_capture(
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
