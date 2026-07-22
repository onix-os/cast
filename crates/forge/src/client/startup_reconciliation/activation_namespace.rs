//! Descriptor-rooted, bounded startup inventory of the activation namespace.
//!
//! Diagnostic inventory and admission are read-only. The independent
//! rollback-decision and rollback-routing proofs expose no effects. A private
//! rollback-reverse child may consume already sealed effect evidence through
//! one exchange attempt and the exact ordered parent-durability suffix. It
//! exposes no general namespace-mutation API. Separate candidate-preservation
//! children may consume exact NewState target prefixes, an ActivateArchived
//! child move, or an ActiveReblit wrapper exchange through one attempt.
//! Operation-specific preparation and durability barriers remain disjoint;
//! freshly applied and already-preserved evidence converge only inside their
//! own operation family. Production dispatch may consume only those
//! phase-specific paths and their journal-persistence boundaries; no path
//! exposes general cleanup or trigger authority.

mod activate_archived_complete_route_proof;
mod activate_archived_finalization_proof;
mod active_reblit_boot_repair_complete_proof;
mod active_reblit_boot_repair_required_proof;
mod active_reblit_boot_repair_started_error_classification;
mod active_reblit_boot_repair_started_proof;
mod active_reblit_boot_sync_complete_proof;
mod active_reblit_complete_route_proof;
mod active_reblit_finalization_proof;
mod candidate_preserve_proof;
mod capture;
mod decision_proof;
mod fresh_db_invalidation_proof;
mod fresh_db_invalidation_route_proof;
mod parent_durability;
mod policy;
mod resume_route_proof;
mod rollback_complete_route_proof;
#[allow(dead_code)] // checkpoint A remains sealed from production dispatch
mod rollback_finalization_proof;
mod rollback_reverse_proof;
mod usr_exchanged_root_abi_proof;

#[cfg(test)]
mod tests;

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

#[cfg(test)]
pub(in crate::client) use activate_archived_complete_route_proof::arm_before_usr_rollback_activate_archived_complete_route_fresh_namespace_capture;
pub(super) use activate_archived_complete_route_proof::{
    UsrRollbackActivateArchivedCompleteRouteNamespaceError,
    UsrRollbackActivateArchivedCompleteRouteNamespaceInspection,
    UsrRollbackActivateArchivedCompleteRouteNamespaceProof,
};
#[cfg(test)]
pub(in crate::client) use activate_archived_finalization_proof::arm_before_usr_rollback_activate_archived_finalization_fresh_namespace_capture;
pub(super) use activate_archived_finalization_proof::{
    UsrRollbackActivateArchivedFinalizationNamespaceError, UsrRollbackActivateArchivedFinalizationNamespaceInspection,
    UsrRollbackActivateArchivedFinalizationNamespaceProof,
};
#[cfg(test)]
pub(in crate::client) use active_reblit_boot_repair_complete_proof::{
    ActiveReblitBootRepairCompleteCaptureFault, arm_active_reblit_boot_repair_complete_capture_fault,
    arm_before_usr_rollback_active_reblit_boot_repair_complete_fresh_namespace_capture,
};
pub(super) use active_reblit_boot_repair_complete_proof::{
    UsrRollbackActiveReblitBootRepairCompleteNamespaceError,
    UsrRollbackActiveReblitBootRepairCompleteNamespaceInspection,
    UsrRollbackActiveReblitBootRepairCompleteNamespaceProof,
};
#[cfg(test)]
pub(in crate::client) use active_reblit_boot_repair_required_proof::arm_before_usr_rollback_active_reblit_boot_repair_required_fresh_namespace_capture;
pub(super) use active_reblit_boot_repair_required_proof::{
    UsrRollbackActiveReblitBootRepairRequiredNamespaceError,
    UsrRollbackActiveReblitBootRepairRequiredNamespaceInspection,
    UsrRollbackActiveReblitBootRepairRequiredNamespaceProof,
};
pub(super) use active_reblit_boot_repair_started_error_classification::{
    complete_namespace_error_is_structural, started_namespace_error_is_structural,
};
#[cfg(test)]
pub(in crate::client) use active_reblit_boot_repair_started_proof::{
    ActiveReblitBootRepairStartedCaptureFault, arm_active_reblit_boot_repair_started_capture_fault,
};
pub(super) use active_reblit_boot_repair_started_proof::{
    UsrRollbackActiveReblitBootRepairStartedNamespaceError,
    UsrRollbackActiveReblitBootRepairStartedNamespaceInspection,
    UsrRollbackActiveReblitBootRepairStartedNamespaceProof,
};
#[cfg(test)]
pub(in crate::client) use active_reblit_boot_sync_complete_proof::arm_before_active_reblit_boot_sync_complete_fresh_namespace_capture;
pub(super) use active_reblit_boot_sync_complete_proof::{
    ActiveReblitBootSyncCompleteNamespaceError, ActiveReblitBootSyncCompleteNamespaceInspection,
    ActiveReblitBootSyncCompleteNamespaceProof, active_reblit_boot_sync_complete_namespace_error_is_mismatch,
};
#[cfg(test)]
pub(in crate::client) use active_reblit_complete_route_proof::arm_before_usr_rollback_active_reblit_complete_route_fresh_namespace_capture;
pub(super) use active_reblit_complete_route_proof::{
    UsrRollbackActiveReblitCompleteRouteNamespaceError, UsrRollbackActiveReblitCompleteRouteNamespaceInspection,
    UsrRollbackActiveReblitCompleteRouteNamespaceProof,
};
#[cfg(test)]
pub(in crate::client) use active_reblit_finalization_proof::arm_before_usr_rollback_active_reblit_finalization_fresh_namespace_capture;
pub(super) use active_reblit_finalization_proof::{
    UsrRollbackActiveReblitFinalizationNamespaceError, UsrRollbackActiveReblitFinalizationNamespaceInspection,
    UsrRollbackActiveReblitFinalizationNamespaceProof,
};

pub(super) use candidate_preserve_proof::UsrRollbackCandidatePreserveTopology;
pub(super) use candidate_preserve_proof::UsrRollbackNewStateTargetNormalizeNamespaceReconciliation;
#[cfg(test)]
pub(in crate::client) use candidate_preserve_proof::arm_before_usr_rollback_candidate_preserve_fresh_namespace_capture;
#[cfg(test)]
pub(in crate::client) use candidate_preserve_proof::{
    ArchivedCandidatePreserveMoveFault, ArchivedCandidatePreservePostMoveDurabilityEvent,
    ArchivedCandidatePreservePostMoveDurabilityFaultPoint, ArchivedCandidatePreserveTargetDurabilityEvent,
    ArchivedCandidatePreserveTargetDurabilityFaultPoint, NewStateCandidatePreservePostMoveDurabilityEvent,
    NewStateCandidatePreservePostMoveDurabilityFaultPoint, NewStateCandidatePreserveTargetDurabilityEvent,
    NewStateCandidatePreserveTargetDurabilityFaultPoint, NewStateTargetNormalizeDurabilityEvent,
    NewStateTargetNormalizeDurabilityFaultPoint, archived_candidate_preserve_move_attempt_count,
    arm_archived_candidate_preserve_move_fault, arm_archived_candidate_preserve_post_move_durability_fault,
    arm_archived_candidate_preserve_target_durability_fault,
    arm_before_archived_candidate_preserve_durable_post_revalidation_capture,
    arm_before_archived_candidate_preserve_move_reconciliation_capture,
    arm_before_archived_candidate_preserve_move_reconciliation_closing,
    arm_before_archived_candidate_preserve_post_candidate_sync,
    arm_before_archived_candidate_preserve_post_final_capture,
    arm_before_archived_candidate_preserve_post_roots_parent_sync,
    arm_before_archived_candidate_preserve_post_staging_parent_sync,
    arm_before_archived_candidate_preserve_post_target_parent_sync,
    arm_before_archived_candidate_preserve_pre_candidate_sync,
    arm_before_archived_candidate_preserve_pre_final_capture,
    arm_before_archived_candidate_preserve_pre_move_revalidation,
    arm_before_archived_candidate_preserve_pre_roots_parent_sync,
    arm_before_archived_candidate_preserve_pre_staging_parent_sync,
    arm_before_archived_candidate_preserve_pre_target_parent_sync,
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
    reset_archived_candidate_preserve_move_attempt_count,
    reset_archived_candidate_preserve_post_move_durability_events,
    reset_archived_candidate_preserve_target_durability_events,
    reset_new_state_candidate_preserve_post_move_durability_events,
    reset_new_state_candidate_preserve_target_durability_events, reset_new_state_target_normalize_durability_events,
    take_archived_candidate_preserve_post_move_durability_events,
    take_archived_candidate_preserve_target_durability_events,
    take_new_state_candidate_preserve_post_move_durability_events,
    take_new_state_candidate_preserve_target_durability_events, take_new_state_target_normalize_durability_events,
};
pub(in crate::client::startup_reconciliation) use candidate_preserve_proof::{
    UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedNamespace,
    UsrRollbackActiveReblitCandidatePreserveAppliedNamespace, UsrRollbackActiveReblitCandidatePreserveDurableNamespace,
    UsrRollbackActiveReblitCandidatePreserveNamespaceApplyReconciliation,
    UsrRollbackActiveReblitCandidatePreserveNamespaceEffectEvidence,
};
pub(super) use candidate_preserve_proof::{
    UsrRollbackArchivedCandidatePreserveAlreadySatisfiedNamespace,
    UsrRollbackArchivedCandidatePreserveAppliedNamespace, UsrRollbackArchivedCandidatePreserveDurableNamespace,
    UsrRollbackArchivedCandidatePreserveNamespaceApplyReconciliation,
    UsrRollbackArchivedCandidatePreserveNamespaceEffectEvidence,
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
#[cfg(test)]
pub(in crate::client) use capture::{
    ActiveReblitCandidatePreserveExchangeFault, ActiveReblitCandidatePreservePostExchangeDurabilityEvent,
    ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint, NewStateCandidatePreserveMoveFault,
    NewStateTargetCreateFault, NewStateTargetNormalizeFault, active_reblit_candidate_preserve_exchange_attempt_count,
    arm_active_reblit_candidate_preserve_exchange_fault,
    arm_active_reblit_candidate_preserve_post_exchange_durability_fault,
    arm_before_active_reblit_candidate_preserve_durable_post_revalidation_capture,
    arm_before_active_reblit_candidate_preserve_post_exchange_candidate_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_candidate_wrapper_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_final_post_capture,
    arm_before_active_reblit_candidate_preserve_post_exchange_quarantine_parent_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_reservation_wrapper_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_roots_parent_sync,
    arm_before_active_reblit_candidate_preserve_reconciliation_capture,
    arm_before_new_state_candidate_preserve_move_reconciliation_capture, arm_before_new_state_target_create_attempt,
    arm_before_new_state_target_create_reconciliation_capture, arm_before_new_state_target_normalize_attempt,
    arm_before_new_state_target_normalize_reconciliation_capture, arm_new_state_candidate_preserve_move_fault,
    arm_new_state_target_create_fault, arm_new_state_target_normalize_fault,
    new_state_candidate_preserve_move_attempt_count, new_state_target_create_attempt_count,
    new_state_target_normalize_attempt_count, reset_active_reblit_candidate_preserve_exchange_attempt_count,
    reset_active_reblit_candidate_preserve_post_exchange_durability_events,
    reset_new_state_candidate_preserve_move_attempt_count, reset_new_state_target_create_attempt_count,
    reset_new_state_target_normalize_attempt_count,
    take_active_reblit_candidate_preserve_post_exchange_durability_events,
};
use capture::{CaptureError, NamespaceSnapshot, capture_snapshot};
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
#[cfg(test)]
pub(in crate::client) use rollback_finalization_proof::arm_before_usr_rollback_finalization_fresh_namespace_capture;
pub(super) use rollback_finalization_proof::{
    UsrRollbackFinalizationNamespaceError, UsrRollbackFinalizationNamespaceInspection,
    UsrRollbackFinalizationNamespaceProof,
};
pub(super) use rollback_reverse_proof::{
    UsrRollbackReverseAlreadySatisfiedNamespace, UsrRollbackReverseAppliedNamespace,
    UsrRollbackReverseDurableNamespace, UsrRollbackReverseNamespaceApplyReconciliation,
    UsrRollbackReverseNamespaceEffectEvidence, UsrRollbackReverseNamespaceError, UsrRollbackReverseNamespaceInspection,
    UsrRollbackReverseNamespaceProof,
};
#[cfg(test)]
pub(in crate::client) use usr_exchanged_root_abi_proof::{
    arm_after_usr_exchanged_root_abi_complete_sync, arm_after_usr_exchanged_root_abi_publication,
    arm_before_usr_exchanged_root_abi_complete_sync, arm_before_usr_exchanged_root_abi_publication,
    arm_usr_exchanged_root_abi_complete_sync_fault,
    reset_usr_exchanged_root_abi_effect_counts, usr_exchanged_root_abi_complete_sync_attempts,
    usr_exchanged_root_abi_publication_attempts,
};
pub(super) use usr_exchanged_root_abi_proof::{
    UsrExchangedRootAbiNamespaceAdmission, UsrExchangedRootAbiNamespaceError,
    UsrExchangedRootAbiNamespaceInspection, UsrExchangedRootAbiNamespaceProof,
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
