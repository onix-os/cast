//! Final restrictive-residue PRE proof, one-attempt reconciliation, and
//! ordered durability completion.
//!
//! Namespace preparation ends before the descriptor-bound chmod so enclosing
//! authority can later repeat its binding-first non-namespace checks. A fresh
//! canonical semantic result stays private until both descriptor barriers and
//! final canonical capture complete. Every namespace result is then fieldless
//! and forces a caller boundary.

use crate::{Installation, transition_journal::TransitionRecord};

use super::{
    UsrRollbackCandidatePreserveNamespaceError, UsrRollbackCandidatePreserveTopology, require_matching_fingerprints,
    require_topology,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    NamespaceSnapshot, NewStateTargetNormalizeLayout, NewStateTargetNormalizeReconciliation,
    ProjectedNewStateTargetNormalizeNamespace, UsrRollbackNewStateTargetNormalizeNamespaceEvidence, capture_snapshot,
};

/// Fieldless namespace result of consuming one exact residue capability.
#[must_use = "a consumed NewState target-normalization namespace result must be handled"]
pub(in crate::client::startup_reconciliation) enum UsrRollbackNewStateTargetNormalizeNamespaceReconciliation {
    RestartRequired,
    NotApplied,
    Ambiguous,
}

/// Opaque final PRE authority for exactly one descriptor-bound chmod attempt.
#[must_use = "prepared NewState target-normalization namespace authority must be consumed"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackNewStateTargetNormalizePreparedNamespace {
    baseline: NamespaceSnapshot,
    projection: ProjectedNewStateTargetNormalizeNamespace,
}

impl UsrRollbackNewStateTargetNormalizeNamespaceEvidence {
    /// Prove one final exact restrictive-residue PRE without performing an effect.
    pub(in crate::client::startup_reconciliation) fn prepare_target_normalization(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackNewStateTargetNormalizePreparedNamespace, UsrRollbackCandidatePreserveNamespaceError> {
        let Self { baseline, projection } = self;
        if projection.layout() != NewStateTargetNormalizeLayout::RestrictiveResidue {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }

        installation.revalidate_mutable_namespace()?;
        baseline.revalidate_retained()?;
        require_projection(record, &baseline, &projection)?;
        require_topology(
            record,
            &baseline,
            UsrRollbackCandidatePreserveTopology::NewStateStagedWithTargetResidue,
        )?;

        run_before_final_pre_capture();
        let fresh = capture_snapshot(installation, record)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&baseline, &fresh)?;
        require_projection(record, &fresh, &projection)?;
        require_topology(
            record,
            &fresh,
            UsrRollbackCandidatePreserveTopology::NewStateStagedWithTargetResidue,
        )?;
        installation.revalidate_mutable_namespace()?;

        Ok(UsrRollbackNewStateTargetNormalizePreparedNamespace {
            baseline: fresh,
            projection,
        })
    }
}

impl UsrRollbackNewStateTargetNormalizePreparedNamespace {
    /// Consume exact final PRE authority through one attempt, fresh semantic
    /// capture, and ordered target-then-parent durability.
    pub(in crate::client::startup_reconciliation) fn reconcile_target_normalization(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> UsrRollbackNewStateTargetNormalizeNamespaceReconciliation {
        let Self { baseline, projection } = self;
        match baseline
            .attempt_new_state_target_normalize_once(projection)
            .reconcile(installation, record)
        {
            NewStateTargetNormalizeReconciliation::Canonical(canonical) => {
                match canonical.complete_durability(installation, record) {
                    Ok(()) => UsrRollbackNewStateTargetNormalizeNamespaceReconciliation::RestartRequired,
                    Err(_) => UsrRollbackNewStateTargetNormalizeNamespaceReconciliation::Ambiguous,
                }
            }
            NewStateTargetNormalizeReconciliation::NotApplied => {
                UsrRollbackNewStateTargetNormalizeNamespaceReconciliation::NotApplied
            }
            NewStateTargetNormalizeReconciliation::Ambiguous => {
                UsrRollbackNewStateTargetNormalizeNamespaceReconciliation::Ambiguous
            }
        }
    }
}

fn require_projection(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: &ProjectedNewStateTargetNormalizeNamespace,
) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
    if ProjectedNewStateTargetNormalizeNamespace::capture(snapshot, record)? == *expected {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged)
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_PRE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_new_state_target_normalize_final_pre_capture(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_PRE_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_final_pre_capture() {
    BEFORE_FINAL_PRE_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_final_pre_capture() {}
