//! Descriptor-rooted, bounded startup inventory of the activation namespace.
//!
//! Diagnostic inventory and admission are read-only. The independent
//! rollback-decision and rollback-routing proofs expose no effects. A private
//! rollback-reverse child may consume already sealed effect evidence through
//! one exchange attempt and the exact ordered parent-durability suffix. It
//! exposes no general namespace-mutation API. Separate candidate-preservation
//! children may consume exact NewState target prefixes through one creation,
//! normalization, or movement attempt. Normalization privately completes its
//! exact target and quarantine-parent barriers before reporting a restart.
//! Every movement lease separately completes its candidate, target, and
//! quarantine-parent pre-move barriers before rename. Freshly applied and
//! already-preserved NewState evidence then converge on the same ordered
//! post-move candidate-and-parent durability suffix. Production dispatch may
//! consume only those phase-specific paths and their journal persistence
//! boundaries; no path exposes general cleanup or trigger authority.

mod candidate_preserve_proof;
mod capture;
mod decision_proof;
mod fresh_db_invalidation_proof;
mod fresh_db_invalidation_route_proof;
mod parent_durability;
mod policy;
mod resume_route_proof;
mod rollback_complete_route_proof;
mod rollback_reverse_proof;

#[cfg(test)]
mod tests;

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

pub(super) use candidate_preserve_proof::UsrRollbackCandidatePreserveTopology;
pub(super) use candidate_preserve_proof::UsrRollbackNewStateTargetNormalizeNamespaceReconciliation;
#[cfg(test)]
pub(in crate::client) use candidate_preserve_proof::arm_before_usr_rollback_candidate_preserve_fresh_namespace_capture;
#[cfg(test)]
pub(in crate::client) use candidate_preserve_proof::{
    NewStateCandidatePreservePostMoveDurabilityEvent, NewStateCandidatePreservePostMoveDurabilityFaultPoint,
    NewStateCandidatePreserveTargetDurabilityEvent, NewStateCandidatePreserveTargetDurabilityFaultPoint,
    NewStateTargetNormalizeDurabilityEvent, NewStateTargetNormalizeDurabilityFaultPoint,
    arm_before_new_state_candidate_preserve_candidate_sync,
    arm_before_new_state_candidate_preserve_durable_post_revalidation_capture,
    arm_before_new_state_candidate_preserve_post_move_candidate_sync,
    arm_before_new_state_candidate_preserve_post_move_final_post_capture,
    arm_before_new_state_candidate_preserve_post_move_quarantine_parent_sync,
    arm_before_new_state_candidate_preserve_post_move_staging_parent_sync,
    arm_before_new_state_candidate_preserve_post_move_target_parent_sync,
    arm_before_new_state_candidate_preserve_quarantine_parent_sync,
    arm_before_new_state_candidate_preserve_target_durability_final_pre_capture,
    arm_before_new_state_candidate_preserve_target_durability_pre_move_revalidation,
    arm_before_new_state_candidate_preserve_target_sync, arm_before_new_state_target_normalize_final_canonical_capture,
    arm_before_new_state_target_normalize_quarantine_parent_sync, arm_before_new_state_target_normalize_target_sync,
    arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture,
    arm_before_usr_rollback_new_state_target_create_final_pre_capture,
    arm_before_usr_rollback_new_state_target_normalize_final_pre_capture,
    arm_new_state_candidate_preserve_post_move_durability_fault,
    arm_new_state_candidate_preserve_target_durability_fault, arm_new_state_target_normalize_durability_fault,
    reset_new_state_candidate_preserve_post_move_durability_events,
    reset_new_state_candidate_preserve_target_durability_events, reset_new_state_target_normalize_durability_events,
    take_new_state_candidate_preserve_post_move_durability_events,
    take_new_state_candidate_preserve_target_durability_events, take_new_state_target_normalize_durability_events,
};
pub(super) use candidate_preserve_proof::{
    UsrRollbackCandidatePreserveNamespaceError, UsrRollbackCandidatePreserveNamespaceInspection,
    UsrRollbackCandidatePreserveNamespaceProof, UsrRollbackNewStateCandidatePreserveAlreadySatisfiedNamespace,
    UsrRollbackNewStateCandidatePreserveAppliedNamespace, UsrRollbackNewStateCandidatePreserveDurableNamespace,
    UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation,
    UsrRollbackNewStateCandidatePreserveNamespaceEffectEvidence,
    UsrRollbackNewStateTargetCreateNamespaceReconciliation,
};
#[cfg(test)]
pub(in crate::client) use capture::arm_before_reverse_exchange_reconciliation_capture;
use capture::{CaptureError, NamespaceSnapshot, capture_snapshot};
#[cfg(test)]
pub(in crate::client) use capture::{
    NewStateCandidatePreserveMoveFault, NewStateTargetCreateFault, NewStateTargetNormalizeFault,
    arm_before_new_state_candidate_preserve_move_reconciliation_capture, arm_before_new_state_target_create_attempt,
    arm_before_new_state_target_create_reconciliation_capture, arm_before_new_state_target_normalize_attempt,
    arm_before_new_state_target_normalize_reconciliation_capture, arm_new_state_candidate_preserve_move_fault,
    arm_new_state_target_create_fault, arm_new_state_target_normalize_fault,
    new_state_candidate_preserve_move_attempt_count, new_state_target_create_attempt_count,
    new_state_target_normalize_attempt_count, reset_new_state_candidate_preserve_move_attempt_count,
    reset_new_state_target_create_attempt_count, reset_new_state_target_normalize_attempt_count,
};
pub(super) use capture::{
    UsrRollbackNewStateTargetCreateNamespaceEvidence, UsrRollbackNewStateTargetNormalizeNamespaceEvidence,
};
#[cfg(test)]
pub(in crate::client) use decision_proof::arm_before_usr_rollback_decision_fresh_namespace_capture;
pub(super) use decision_proof::{
    UsrRollbackDecisionNamespaceError, UsrRollbackDecisionNamespaceInspection, UsrRollbackDecisionNamespaceProof,
};
#[cfg(test)]
pub(in crate::client) use fresh_db_invalidation_proof::arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture;
pub(super) use fresh_db_invalidation_proof::{
    UsrRollbackFreshDbInvalidationNamespaceError, UsrRollbackFreshDbInvalidationNamespaceInspection,
    UsrRollbackFreshDbInvalidationNamespaceProof,
};
#[cfg(test)]
pub(in crate::client) use fresh_db_invalidation_route_proof::arm_before_usr_rollback_fresh_db_invalidation_route_fresh_namespace_capture;
pub(super) use fresh_db_invalidation_route_proof::{
    UsrRollbackFreshDbInvalidationRouteNamespaceError, UsrRollbackFreshDbInvalidationRouteNamespaceInspection,
    UsrRollbackFreshDbInvalidationRouteNamespaceProof,
};
pub(super) use policy::UsrExchangeLayout;
use policy::{LayoutAlternative, NamespacePolicyConflict, assess_snapshot_layout};
#[cfg(test)]
pub(in crate::client) use resume_route_proof::arm_before_usr_rollback_resume_route_fresh_namespace_capture;
pub(super) use resume_route_proof::{
    UsrRollbackResumeRouteNamespaceError, UsrRollbackResumeRouteNamespaceInspection,
    UsrRollbackResumeRouteNamespaceProof,
};
#[cfg(test)]
pub(in crate::client) use rollback_complete_route_proof::arm_before_usr_rollback_complete_route_fresh_namespace_capture;
pub(super) use rollback_complete_route_proof::{
    UsrRollbackCompleteRouteNamespaceError, UsrRollbackCompleteRouteNamespaceInspection,
    UsrRollbackCompleteRouteNamespaceProof,
};
pub(super) use rollback_reverse_proof::{
    UsrRollbackReverseAlreadySatisfiedNamespace, UsrRollbackReverseAppliedNamespace,
    UsrRollbackReverseDurableNamespace, UsrRollbackReverseNamespaceApplyReconciliation,
    UsrRollbackReverseNamespaceEffectEvidence, UsrRollbackReverseNamespaceError, UsrRollbackReverseNamespaceInspection,
    UsrRollbackReverseNamespaceProof,
};
#[cfg(test)]
pub(in crate::client) use rollback_reverse_proof::{
    UsrRollbackReverseNamespaceDurabilityEvent, UsrRollbackReverseNamespaceDurabilityFaultPoint,
    arm_before_usr_rollback_reverse_durable_namespace_capture,
    arm_before_usr_rollback_reverse_effect_final_namespace_capture,
    arm_before_usr_rollback_reverse_fresh_namespace_capture,
    arm_before_usr_rollback_reverse_namespace_final_pre_capture,
    arm_before_usr_rollback_reverse_namespace_installation_root_sync,
    arm_usr_rollback_reverse_namespace_durability_fault, reset_usr_rollback_reverse_namespace_durability_events,
    take_usr_rollback_reverse_namespace_durability_events,
};

/// Complete read-only evidence collected around one startup assessment.
///
/// Both snapshots retain descriptors for every accepted directory, tree,
/// marker, state-ID, state-slot link, and root-ABI link.  Keeping both sides
/// prevents a matching-looking replacement after the first walk from being
/// mistaken for stable evidence.
#[derive(Debug)]
#[allow(dead_code)] // retained by PendingSystemTransition for structured diagnostics
pub(super) struct ActivationNamespaceEvidence {
    before: Result<NamespaceSnapshot, CaptureError>,
    after: Result<NamespaceSnapshot, CaptureError>,
    journal_before: JournalObservation,
    journal_after: JournalObservation,
    retained_revalidation: Result<(), CaptureError>,
    stability: ActivationNamespaceStability,
    policy: NamespacePolicyAssessment,
}

/// First half of the startup namespace sandwich.
///
/// This value deliberately cannot assess policy.  The final inventory and
/// retained/public-name revalidation must run only after every other startup
/// evidence source, including the second database inspection, has completed.
#[derive(Debug)]
pub(super) struct ActivationNamespaceInspection {
    before: Result<NamespaceSnapshot, CaptureError>,
    journal_before: JournalObservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ActivationNamespaceStability {
    Stable,
    Changed,
    Rejected,
}

#[derive(Debug)]
#[allow(dead_code)] // exact storage errors are part of the diagnostic snapshot
enum JournalObservation {
    Exact,
    Missing,
    Different(Box<TransitionRecord>),
    Rejected(StorageError),
}

#[derive(Debug)]
#[allow(dead_code)] // retains exact conflict/unavailability for structured diagnostics
enum NamespacePolicyAssessment {
    Exact(LayoutAlternative),
    Conflict(NamespacePolicyConflict),
    Unavailable(ActivationNamespaceStability),
}

impl ActivationNamespaceInspection {
    pub(super) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Self {
        let journal_before = observe_journal(journal, expected);
        let before = capture_snapshot(installation, expected);
        Self { before, journal_before }
    }

    pub(super) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> ActivationNamespaceEvidence {
        let after = capture_snapshot(installation, expected);
        run_before_final_namespace_revalidation();
        let retained_revalidation = match (&self.before, &after) {
            (Ok(before), Ok(after)) => before.revalidate_retained().and_then(|()| after.revalidate_retained()),
            (Err(_), _) | (_, Err(_)) => Ok(()),
        };
        // This is intentionally the last journal read.  No later startup
        // evidence collection can race ahead of the namespace sandwich.
        let journal_after = observe_journal(journal, expected);

        let stability = match (&self.before, &after, &retained_revalidation) {
            (Ok(before), Ok(after), Ok(())) if before.fingerprint() == after.fingerprint() => {
                ActivationNamespaceStability::Stable
            }
            (Ok(_), Ok(_), _) => ActivationNamespaceStability::Changed,
            (Err(_), _, _) | (_, Err(_), _) => ActivationNamespaceStability::Rejected,
        };
        let policy = match (&after, stability) {
            (Ok(snapshot), ActivationNamespaceStability::Stable) => match assess_snapshot_layout(expected, snapshot) {
                Ok(layout) => NamespacePolicyAssessment::Exact(layout),
                Err(conflict) => NamespacePolicyAssessment::Conflict(conflict),
            },
            (_, unavailable) => NamespacePolicyAssessment::Unavailable(unavailable),
        };

        ActivationNamespaceEvidence {
            before: self.before,
            after,
            journal_before: self.journal_before,
            journal_after,
            retained_revalidation,
            stability,
            policy,
        }
    }
}

impl ActivationNamespaceEvidence {
    pub(super) fn stability(&self) -> ActivationNamespaceStability {
        self.stability
    }

    pub(super) fn journal_is_exact(&self) -> bool {
        matches!(self.journal_before, JournalObservation::Exact)
            && matches!(self.journal_after, JournalObservation::Exact)
    }

    pub(super) fn phase_layout_is_exact(&self) -> bool {
        self.stability == ActivationNamespaceStability::Stable
            && self.journal_is_exact()
            && matches!(self.policy, NamespacePolicyAssessment::Exact(_))
    }

    pub(super) fn usr_exchange_layout(&self) -> Option<UsrExchangeLayout> {
        if self.stability != ActivationNamespaceStability::Stable || !self.journal_is_exact() {
            return None;
        }
        match self.policy {
            NamespacePolicyAssessment::Exact(layout) => layout.usr_exchange_layout(),
            NamespacePolicyAssessment::Conflict(_) | NamespacePolicyAssessment::Unavailable(_) => None,
        }
    }

    #[cfg(test)]
    pub(super) fn policy_was_assessed(&self) -> bool {
        !matches!(self.policy, NamespacePolicyAssessment::Unavailable(_))
    }
}

fn observe_journal(journal: &TransitionJournalStore, expected: &TransitionRecord) -> JournalObservation {
    match journal.load() {
        Ok(Some(actual)) if actual == *expected => JournalObservation::Exact,
        Ok(Some(actual)) => JournalObservation::Different(Box::new(actual)),
        Ok(None) => JournalObservation::Missing,
        Err(source) => JournalObservation::Rejected(source),
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_NAMESPACE_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_before_final_namespace_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_NAMESPACE_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_final_namespace_revalidation() {
    BEFORE_FINAL_NAMESPACE_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_final_namespace_revalidation() {}
