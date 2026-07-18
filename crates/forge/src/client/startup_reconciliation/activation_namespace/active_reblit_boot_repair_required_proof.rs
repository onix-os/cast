//! Independent namespace proof for routing one preserved ActiveReblit
//! candidate to the boot-repair boundary.
//!
//! This proof is read-only. It retains both sides of its admission sandwich,
//! the exact derived replacement-wrapper index, and a fresh matching capture
//! on every revalidation. The phase-specific authority layered above this
//! proof is solely responsible for distinguishing a boot-repair plan from the
//! ordinary rollback-completion plan.

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    candidate_preserve_proof::{
        UsrRollbackCandidatePreserveNamespaceError, require_exact_active_reblit_candidate_preserved_topology,
    },
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitBootRepairRequiredNamespaceInspection {
    before: NamespaceSnapshot,
    wrapper_index: usize,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitBootRepairRequiredNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    wrapper_index: usize,
}

impl UsrRollbackActiveReblitBootRepairRequiredNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackActiveReblitBootRepairRequiredNamespaceError> {
        require_exact_journal(journal, expected)?;
        let before = capture_snapshot(installation, expected)?;
        let wrapper_index = require_exact_active_reblit_candidate_preserved_topology(expected, &before)?;
        Ok(Self { before, wrapper_index })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitBootRepairRequiredNamespaceProof,
        UsrRollbackActiveReblitBootRepairRequiredNamespaceError,
    > {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_wrapper_index(expected, &self.before, self.wrapper_index)?;
        require_exact_wrapper_index(expected, &after, self.wrapper_index)?;
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackActiveReblitBootRepairRequiredNamespaceProof {
            before: self.before,
            after,
            wrapper_index: self.wrapper_index,
        })
    }
}

impl UsrRollbackActiveReblitBootRepairRequiredNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackActiveReblitBootRepairRequiredNamespaceError> {
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

    #[cfg(test)]
    pub(in crate::client) fn wrapper_index(&self) -> usize {
        self.wrapper_index
    }
}

fn require_exact_wrapper_index(
    expected: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    wrapper_index: usize,
) -> Result<(), UsrRollbackActiveReblitBootRepairRequiredNamespaceError> {
    let actual = require_exact_active_reblit_candidate_preserved_topology(expected, snapshot)?;
    if actual == wrapper_index {
        Ok(())
    } else {
        Err(
            UsrRollbackActiveReblitBootRepairRequiredNamespaceError::WrapperIndexChanged {
                expected: wrapper_index,
                actual,
            },
        )
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackActiveReblitBootRepairRequiredNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackActiveReblitBootRepairRequiredNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackActiveReblitBootRepairRequiredNamespaceError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackActiveReblitBootRepairRequiredNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackActiveReblitBootRepairRequiredNamespaceError {
    #[error("capture or revalidate the exact ActiveReblit boot-repair-required namespace")]
    Capture(#[from] CaptureError),
    #[error("prove the exact preserved ActiveReblit whole-wrapper topology")]
    Topology(#[from] UsrRollbackCandidatePreserveNamespaceError),
    #[error("read the retained canonical ActiveReblit transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical ActiveReblit transition journal changed during boot-repair-required proof")]
    JournalChanged,
    #[error("the ActiveReblit boot-repair-required namespace changed during proof")]
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
pub(in crate::client) fn arm_before_usr_rollback_active_reblit_boot_repair_required_fresh_namespace_capture(
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
