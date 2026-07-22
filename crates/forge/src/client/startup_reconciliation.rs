//! Classification and sealed semantic reconciliation of one durable startup
//! transition.
//!
//! Admission remains read-only. Consumed rollback-reverse typestates cross the
//! production one-shot exchange and ordered parent-durability boundaries. A
//! separate production leaf consumes exact NewState target prefixes,
//! ActivateArchived child-move evidence, and ActiveReblit exchange evidence
//! into disjoint capabilities. Each operation retains its own one-attempt
//! reconciliation and durability boundaries. Applied and exact
//! already-preserved evidence converge only within their operation family
//! before separate journal-persistence boundaries. The complete suffix remains
//! phase-specific and has no cleanup or trigger authority.

use std::{fmt, path::PathBuf};

use crate::{
    Installation, db,
    state::{self, TransitionId},
    transition_journal::{
        Phase, RecoveryDisposition, RuntimeEpoch, RuntimeEvidenceError, RuntimeTreeIdentity, TransitionJournalStore,
        TransitionRecord,
    },
    tree_marker::{RetainedTreeMarker, TreeMarkerError, TreeMarkerStore},
};

mod activation_namespace;
mod active_reblit_boot_sync_started_guard;
mod active_reblit_commit_cleanup_authority;
mod active_reblit_commit_cleanup_complete_authority;
mod active_reblit_complete_finalization_authority;
#[allow(dead_code)] // read-only startup boundary; live dispatch is deliberately a later slice
mod active_reblit_boot_sync_complete_authority;
mod active_reblit_boot_repair_evidence;
mod database_evidence;
#[cfg(test)]
mod focused_test_exports;
mod metadata_provenance;
mod replacement_mutation_authority;
mod usr_rollback_activate_archived_complete_route_authority;
mod usr_rollback_activate_archived_finalization_authority;
mod usr_rollback_active_reblit_boot_repair_complete_authority;
mod usr_rollback_active_reblit_boot_repair_required_authority;
mod usr_rollback_active_reblit_boot_repair_unverified_authority;
mod usr_rollback_active_reblit_complete_route_authority;
mod usr_rollback_active_reblit_finalization_authority;
mod usr_rollback_candidate_preserve_authority;
mod usr_rollback_complete_route_authority;
mod usr_rollback_decision_authority;
mod usr_rollback_finalization_authority;
mod usr_rollback_fresh_db_invalidation_authority;
mod usr_rollback_fresh_db_invalidation_route_authority;
mod usr_rollback_resume_route_authority;
mod usr_rollback_reverse_authority;
mod usr_exchanged_root_abi_authority;

#[cfg(test)]
pub(in crate::client) use focused_test_exports::*;

pub(in crate::client) use active_reblit_boot_sync_started_guard::{
    ActiveReblitBootSyncStartedGuard, ActiveReblitBootSyncStartedGuardAdmission,
    ActiveReblitBootSyncStartedGuardError,
};

#[allow(unused_imports)] // exported for focused startup adoption and the later persistence leaf
pub(in crate::client) use active_reblit_boot_sync_complete_authority::{
    ActiveReblitBootSyncCompleteAdmission, ActiveReblitBootSyncCompleteAuthority,
    ActiveReblitBootSyncCompleteAuthorityError, ActiveReblitBootSyncCompletePostAdvanceAuthority,
    ActiveReblitBootSyncCompleteRecordAdvanceError,
};
#[cfg(test)]
pub(in crate::client) use active_reblit_boot_sync_complete_authority::arm_between_active_reblit_boot_sync_complete_database_captures;
#[cfg(test)]
pub(in crate::client) use activation_namespace::arm_before_active_reblit_boot_sync_complete_fresh_namespace_capture;
#[allow(unused_imports)] // exported for the specialized cleanup path and focused contracts
pub(in crate::client) use active_reblit_commit_cleanup_authority::{
    ActiveReblitCommitCleanupAdmission, ActiveReblitCommitCleanupApplyAuthority,
    ActiveReblitCommitCleanupApplyEffectAuthority, ActiveReblitCommitCleanupApplyReconciliation,
    ActiveReblitCommitCleanupAuthority, ActiveReblitCommitCleanupAuthorityError,
    ActiveReblitCommitCleanupDurableAuthority, ActiveReblitCommitCleanupEffectError,
    ActiveReblitCommitCleanupFinishAuthority, ActiveReblitCommitCleanupFinishEffectAuthority,
    ActiveReblitCommitCleanupPendingDurabilityAuthority,
    ActiveReblitCommitCleanupPostAdvanceAuthority, ActiveReblitCommitCleanupRecordAdvanceError,
};
pub(in crate::client) use active_reblit_commit_cleanup_complete_authority::{
    ActiveReblitCommitCleanupCompleteAdmission,
    ActiveReblitCommitCleanupCompleteAuthority,
    ActiveReblitCommitCleanupCompleteAuthorityError,
    ActiveReblitCommitCleanupCompletePostAdvanceAuthority,
    ActiveReblitCommitCleanupCompleteRecordAdvanceError,
};
pub(in crate::client) use active_reblit_complete_finalization_authority::{
    ActiveReblitCompleteFinalizationAdmission,
    ActiveReblitCompleteFinalizationAfterDeleteAuthority,
    ActiveReblitCompleteFinalizationAuthority,
    ActiveReblitCompleteFinalizationAuthorityError,
};
#[cfg(test)]
pub(in crate::client) use active_reblit_complete_finalization_authority::{
    arm_between_active_reblit_complete_finalization_database_captures,
    arm_between_active_reblit_complete_finalization_post_delete_database_captures,
};
#[cfg(test)]
pub(in crate::client) use active_reblit_commit_cleanup_complete_authority::arm_between_active_reblit_commit_cleanup_complete_database_captures;
#[cfg(test)]
pub(in crate::client) use active_reblit_commit_cleanup_authority::arm_between_active_reblit_commit_cleanup_database_captures;
#[cfg(test)]
pub(in crate::client) use activation_namespace::arm_before_active_reblit_commit_cleanup_fresh_namespace_capture;
#[cfg(test)]
pub(in crate::client) use activation_namespace::{
    ActiveReblitCommitCleanupDurabilityEvent, ActiveReblitCommitCleanupDurabilityFaultPoint,
    ActiveReblitCommitCleanupExchangeFault, active_reblit_commit_cleanup_exchange_attempt_count,
    arm_active_reblit_commit_cleanup_durability_fault,
    arm_active_reblit_commit_cleanup_exchange_fault,
    arm_before_active_reblit_commit_cleanup_reconciliation_capture,
    reset_active_reblit_commit_cleanup_durability_events,
    reset_active_reblit_commit_cleanup_exchange_attempt_count,
    take_active_reblit_commit_cleanup_durability_events,
};
pub(crate) use replacement_mutation_authority::ActiveReblitReplacementMutationAuthorityProvider;
pub(in crate::client) use usr_rollback_activate_archived_complete_route_authority::{
    UsrRollbackActivateArchivedCompleteRouteAdmission, UsrRollbackActivateArchivedCompleteRouteAuthority,
    UsrRollbackActivateArchivedCompleteRouteAuthorityError,
    UsrRollbackActivateArchivedCompleteRouteRecordAdvanceError,
};
pub(in crate::client) use usr_rollback_activate_archived_finalization_authority::{
    UsrRollbackActivateArchivedFinalizationAdmission,
    UsrRollbackActivateArchivedFinalizationAfterDeleteAuthority,
    UsrRollbackActivateArchivedFinalizationAuthority, UsrRollbackActivateArchivedFinalizationAuthorityError,
};
pub(in crate::client) use usr_rollback_active_reblit_boot_repair_complete_authority::{
    UsrRollbackActiveReblitBootRepairCompleteAdmission, UsrRollbackActiveReblitBootRepairCompleteAuthority,
    UsrRollbackActiveReblitBootRepairCompleteAuthorityError,
};
pub(in crate::client) use usr_rollback_active_reblit_boot_repair_required_authority::{
    UsrRollbackActiveReblitBootRepairRequiredAdmission, UsrRollbackActiveReblitBootRepairRequiredAuthority,
    UsrRollbackActiveReblitBootRepairRequiredAuthorityError,
};
pub(in crate::client) use usr_rollback_active_reblit_boot_repair_unverified_authority::{
    UsrRollbackActiveReblitBootRepairUnverifiedAdmission, UsrRollbackActiveReblitBootRepairUnverifiedAuthority,
    UsrRollbackActiveReblitBootRepairUnverifiedAuthorityError,
};
pub(in crate::client) use usr_rollback_active_reblit_complete_route_authority::{
    UsrRollbackActiveReblitCompleteRouteAdmission, UsrRollbackActiveReblitCompleteRouteAuthority,
    UsrRollbackActiveReblitCompleteRouteAuthorityError, UsrRollbackActiveReblitCompleteRouteRecordAdvanceError,
};
pub(in crate::client) use usr_rollback_active_reblit_finalization_authority::{
    UsrRollbackActiveReblitFinalizationAdmission,
    UsrRollbackActiveReblitFinalizationAfterDeleteAuthority, UsrRollbackActiveReblitFinalizationAuthority,
    UsrRollbackActiveReblitFinalizationAuthorityError,
};
pub(in crate::client) use usr_rollback_candidate_preserve_authority::{
    UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority,
    UsrRollbackActiveReblitCandidatePreserveAppliedEffectAuthority,
    UsrRollbackActiveReblitCandidatePreserveApplyReconciliation,
    UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority,
    UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError,
    UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority,
    UsrRollbackArchivedCandidatePreserveAppliedEffectAuthority,
    UsrRollbackArchivedCandidatePreserveApplyReconciliation,
    UsrRollbackArchivedCandidatePreserveDurableEffectAuthority,
    UsrRollbackArchivedCandidatePreserveRecordAdvanceError,
};
pub(in crate::client) use usr_rollback_candidate_preserve_authority::{
    UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveApplyAuthority,
    UsrRollbackCandidatePreserveApplyEffectSelection, UsrRollbackCandidatePreserveAuthority,
    UsrRollbackCandidatePreserveAuthorityError, UsrRollbackCandidatePreserveFinishAuthority,
    UsrRollbackCandidatePreserveFinishDurabilitySelection, UsrRollbackCandidatePreserveRestartAuthority,
    UsrRollbackCandidatePreserveRecordAdvanceError,
    UsrRollbackNewStateCandidatePreserveAlreadySatisfiedEffectAuthority,
    UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority,
    UsrRollbackNewStateCandidatePreserveApplyReconciliation,
    UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation,
    UsrRollbackNewStateCandidatePreserveDurableEffectAuthority,
    UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation,
};
pub(in crate::client) use usr_rollback_complete_route_authority::{
    UsrRollbackCompleteRouteAdmission, UsrRollbackCompleteRouteAuthority, UsrRollbackCompleteRouteAuthorityError,
    UsrRollbackCompleteRouteRecordAdvanceError,
};
#[allow(unused_imports)] // retained for structured startup diagnostics and focused contracts
pub(in crate::client) use usr_rollback_decision_authority::UsrRollbackDecisionDeferral;
pub(in crate::client) use usr_rollback_decision_authority::{
    UsrExchangeParentDurabilityAuthority, UsrRollbackDecisionAdmission, UsrRollbackDecisionAuthority,
    UsrRollbackDecisionAuthorityError, UsrRollbackDecisionRecordAdvanceError,
};
pub(in crate::client) use usr_rollback_finalization_authority::{
    UsrRollbackFinalizationAdmission, UsrRollbackFinalizationAfterDeleteAuthority,
    UsrRollbackFinalizationAuthority, UsrRollbackFinalizationAuthorityError,
};
pub(in crate::client) use usr_rollback_fresh_db_invalidation_authority::{
    UsrRollbackFreshDbInvalidationAdmission, UsrRollbackFreshDbInvalidationApplyAuthority,
    UsrRollbackFreshDbInvalidationApplyReconciliation, UsrRollbackFreshDbInvalidationAuthority,
    UsrRollbackFreshDbInvalidationAuthorityError, UsrRollbackFreshDbInvalidationEffectAuthority,
    UsrRollbackFreshDbInvalidationFinishAuthority, UsrRollbackFreshDbInvalidationRecordAdvanceError,
};
pub(in crate::client) use usr_rollback_fresh_db_invalidation_route_authority::{
    UsrRollbackFreshDbInvalidationRouteAdmission, UsrRollbackFreshDbInvalidationRouteAuthority,
    UsrRollbackFreshDbInvalidationRouteAuthorityError, UsrRollbackFreshDbInvalidationRouteRecordAdvanceError,
};
pub(in crate::client) use usr_rollback_resume_route_authority::{
    UsrRollbackResumeRouteAdmission, UsrRollbackResumeRouteAuthority, UsrRollbackResumeRouteAuthorityError,
    UsrRollbackResumeRouteRecordAdvanceError,
};

pub(in crate::client) use usr_rollback_reverse_authority::{
    UsrRollbackReverseAdmission, UsrRollbackReverseAlreadySatisfiedEffectAuthority,
    UsrRollbackReverseAppliedEffectAuthority, UsrRollbackReverseApplyAuthority, UsrRollbackReverseApplyReconciliation,
    UsrRollbackReverseAuthority, UsrRollbackReverseAuthorityError, UsrRollbackReverseDurableEffectAuthority,
    UsrRollbackReverseFinishAuthority, UsrRollbackReverseRecordAdvanceError,
};
pub(in crate::client) use usr_exchanged_root_abi_authority::{
    UsrExchangedRootAbiDurabilityAuthority, UsrExchangedRootAbiNormalizationAdmission,
    UsrExchangedRootAbiNormalizationAuthority, UsrExchangedRootAbiNormalizationAuthorityError,
};

use activation_namespace::{
    ActivationNamespaceEvidence, ActivationNamespaceInspection, ActivationNamespaceStability, UsrExchangeLayout,
    UsrRollbackActivateArchivedCompleteRouteNamespaceError,
    UsrRollbackActivateArchivedCompleteRouteNamespaceInspection,
    UsrRollbackActivateArchivedCompleteRouteNamespaceProof, UsrRollbackActivateArchivedFinalizationNamespaceError,
    UsrRollbackActivateArchivedFinalizationNamespaceInspection, UsrRollbackActivateArchivedFinalizationNamespaceProof,
    UsrRollbackActiveReblitBootRepairCompleteNamespaceError,
    UsrRollbackActiveReblitBootRepairCompleteNamespaceInspection,
    UsrRollbackActiveReblitBootRepairCompleteNamespaceProof, UsrRollbackActiveReblitBootRepairRequiredNamespaceError,
    UsrRollbackActiveReblitBootRepairRequiredNamespaceInspection,
    UsrRollbackActiveReblitBootRepairRequiredNamespaceProof, UsrRollbackActiveReblitBootRepairStartedNamespaceError,
    UsrRollbackActiveReblitBootRepairStartedNamespaceInspection,
    UsrRollbackActiveReblitBootRepairStartedNamespaceProof, UsrRollbackActiveReblitCompleteRouteNamespaceError,
    UsrRollbackActiveReblitCompleteRouteNamespaceInspection, UsrRollbackActiveReblitCompleteRouteNamespaceProof,
    UsrRollbackActiveReblitFinalizationNamespaceError, UsrRollbackActiveReblitFinalizationNamespaceInspection,
    UsrRollbackActiveReblitFinalizationNamespaceProof, UsrRollbackCandidatePreserveNamespaceError,
    UsrRollbackCandidatePreserveNamespaceInspection, UsrRollbackCandidatePreserveNamespaceProof,
    UsrRollbackCandidatePreserveTopology, UsrRollbackCompleteRouteNamespaceError,
    UsrRollbackCompleteRouteNamespaceInspection, UsrRollbackCompleteRouteNamespaceProof,
    UsrRollbackDecisionNamespaceError, UsrRollbackDecisionNamespaceInspection, UsrRollbackDecisionNamespaceProof,
    UsrRollbackFinalizationNamespaceError, UsrRollbackFinalizationNamespaceInspection,
    UsrRollbackFinalizationNamespaceProof, UsrRollbackFreshDbInvalidationNamespaceError,
    UsrRollbackFreshDbInvalidationNamespaceInspection, UsrRollbackFreshDbInvalidationNamespaceProof,
    UsrRollbackFreshDbInvalidationRouteNamespaceError, UsrRollbackFreshDbInvalidationRouteNamespaceInspection,
    UsrRollbackFreshDbInvalidationRouteNamespaceProof, UsrRollbackNewStateCandidatePreserveNamespaceEffectEvidence,
    UsrRollbackNewStateTargetCreateNamespaceEvidence, UsrRollbackNewStateTargetNormalizeNamespaceEvidence,
    UsrRollbackResumeRouteNamespaceError, UsrRollbackResumeRouteNamespaceInspection,
    UsrRollbackResumeRouteNamespaceProof, UsrRollbackReverseNamespaceEffectEvidence, UsrRollbackReverseNamespaceError,
    UsrRollbackReverseNamespaceInspection, UsrRollbackReverseNamespaceProof,
    UsrExchangedRootAbiNamespaceAdmission, UsrExchangedRootAbiNamespaceError,
    UsrExchangedRootAbiNamespaceInspection, UsrExchangedRootAbiNamespaceProof,
};
use activation_namespace::{
    UsrRollbackArchivedCandidatePreserveAlreadySatisfiedNamespace,
    UsrRollbackArchivedCandidatePreserveAppliedNamespace, UsrRollbackArchivedCandidatePreserveDurableNamespace,
    UsrRollbackArchivedCandidatePreserveNamespaceApplyReconciliation,
    UsrRollbackArchivedCandidatePreserveNamespaceEffectEvidence, complete_namespace_error_is_structural,
    started_namespace_error_is_structural,
};
use active_reblit_boot_repair_evidence::{
    ActiveReblitBootRepairDatabaseEvidence, ActiveReblitBootRepairDatabaseInspection,
    ActiveReblitBootRepairEvidenceError, active_reblit_completed_boot_repair_plan_is_exact,
    active_reblit_pending_boot_repair_plan_is_exact, capture_active_reblit_boot_repair_active_state,
    inspect_active_reblit_boot_repair_database, require_exact_active_reblit_boot_repair_active_state,
    require_exact_active_reblit_boot_repair_database,
};
#[cfg(test)]
use database_evidence::{
    DatabaseConflict, ExistingStateEvidence, FreshDatabaseExpectation, database_evidence_compatible,
    fresh_database_expectation,
};
use database_evidence::{
    DatabaseEvidence, DatabaseInspectionStability, database_ownership_evidence_compatible, inspect_database,
};
use metadata_provenance::metadata_provenance_evidence_compatible;

const MAX_KNOWN_TREE_LOCATIONS: usize = 5;

/// The installation/global-lock, journal, and state-database capabilities
/// retained while startup proves that no interrupted transition exists.
#[derive(Debug)]
pub(super) struct StartupRecoveryAuthority {
    _installation: Installation,
    journal: TransitionJournalStore,
    state_db: db::state::Database,
}

impl StartupRecoveryAuthority {
    pub(super) fn new(
        installation: &Installation,
        journal: TransitionJournalStore,
        state_db: &db::state::Database,
    ) -> Self {
        let retained = state_db.clone();
        debug_assert!(retained.same_instance(state_db));
        Self {
            _installation: installation.clone(),
            journal,
            state_db: retained,
        }
    }

    pub(super) fn journal(&self) -> &TransitionJournalStore {
        &self.journal
    }
}

/// The fixed, bounded names which can be authenticated without inventing a
/// new recovery namespace API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum KnownTreeRole {
    Live,
    Staging,
    CandidateState(state::Id),
    PreviousState(state::Id),
    Quarantine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // retained for structured diagnostic classification
pub(super) enum DurableTreeRole {
    Candidate,
    Previous,
    Foreign,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // retained for structured diagnostic classification
pub(super) enum RuntimeTreeRole {
    Candidate,
    Previous,
    Foreign,
    NotComparable,
    Unavailable,
}

#[derive(Clone, Debug)]
pub(super) struct KnownTreeLocation {
    path: PathBuf,
    roles: Vec<KnownTreeRole>,
}

#[derive(Debug)]
#[allow(dead_code)] // every field preserves the immutable diagnostic snapshot
pub(super) struct RetainedTreeEvidence {
    location: KnownTreeLocation,
    store: TreeMarkerStore,
    marker: RetainedTreeMarker,
    runtime: Result<RuntimeTreeIdentity, RuntimeEvidenceError>,
}

impl RetainedTreeEvidence {
    #[allow(dead_code)] // consumed by fail-closed blocker classification
    fn durable_role(&self, record: &TransitionRecord) -> DurableTreeRole {
        if self.marker.token() == &record.candidate.tree_token {
            DurableTreeRole::Candidate
        } else if self.marker.token() == &record.previous.tree_token {
            DurableTreeRole::Previous
        } else {
            DurableTreeRole::Foreign
        }
    }

    #[allow(dead_code)] // consumed by fail-closed blocker classification
    fn runtime_role(&self, record: &TransitionRecord, epoch: &RuntimeEpochEvidence) -> RuntimeTreeRole {
        if epoch.comparability(record) != RuntimeEpochComparability::Current {
            return RuntimeTreeRole::NotComparable;
        }
        let Ok(runtime) = &self.runtime else {
            return RuntimeTreeRole::Unavailable;
        };
        if *runtime == record.candidate.usr_runtime_identity {
            RuntimeTreeRole::Candidate
        } else if *runtime == record.previous.usr_runtime_identity {
            RuntimeTreeRole::Previous
        } else {
            RuntimeTreeRole::Foreign
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // variants retain exact rejected/absent diagnostic evidence
pub(super) enum KnownTreeEvidence {
    Retained(RetainedTreeEvidence),
    Unresolved {
        location: KnownTreeLocation,
        retained: Option<RetainedTreeEvidence>,
        reason: UnresolvedTreeReason,
    },
}

#[derive(Debug)]
#[allow(dead_code)] // preserves typed reasons in the diagnostic snapshot
pub(super) enum UnresolvedTreeReason {
    Absent,
    Rejected(TreeMarkerError),
    StateSlotLinkUnauthenticated,
}

/// Runtime witnesses are comparable only when both authenticated epoch
/// captures agree with the journal's creation epoch.
#[derive(Debug)]
pub(super) struct RuntimeEpochEvidence {
    before: Result<RuntimeEpoch, RuntimeEvidenceError>,
    after: Result<RuntimeEpoch, RuntimeEvidenceError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RuntimeEpochComparability {
    Current,
    RecordedEpochChanged,
    ChangedDuringInspection,
    Unavailable,
}

impl RuntimeEpochEvidence {
    fn comparability(&self, record: &TransitionRecord) -> RuntimeEpochComparability {
        let (Ok(before), Ok(after)) = (&self.before, &self.after) else {
            return RuntimeEpochComparability::Unavailable;
        };
        if before != after {
            RuntimeEpochComparability::ChangedDuringInspection
        } else if before == &record.creation_epoch {
            RuntimeEpochComparability::Current
        } else {
            RuntimeEpochComparability::RecordedEpochChanged
        }
    }
}

/// Why this first read-only foundation still refuses to execute effects.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum RecoveryBlocker {
    DatabaseConflict,
    MetadataProvenanceConflict,
    DatabaseChangedDuringInspection,
    RuntimeEpochUnavailable,
    RuntimeEpochChangedDuringInspection,
    RuntimeTreeEvidenceUnavailable,
    RuntimeTreeIdentityConflict,
    TreeEvidenceRejected,
    DurableTreeIdentityConflict,
    UnresolvedStateSlotLink,
    ActivationNamespaceRejected,
    ActivationNamespaceChangedDuringInspection,
    JournalChangedDuringInspection,
    PhaseNamespaceConflict,
    ForwardExchangeDurabilityUnproven,
    ExactNamespaceInventoryRequired,
    ManualBootRepair,
}

/// A pending transition owns the exact database capability and retained tree
/// evidence used by its read-only assessment. Inspection keeps the mutable
/// installation/global-lock and exclusive journal authority through the final
/// revalidation, then deliberately releases both before returning this
/// diagnostic. It therefore exposes and pins no recovery effects. A future
/// executor must run internally before the startup reservation is released and
/// reload the exact canonical journal generation immediately before each
/// conditional mutation.
#[derive(Debug)]
#[allow(dead_code)] // preserves the complete structured snapshot until the diagnostic is dropped
pub(super) struct PendingSystemTransition {
    state_db: db::state::Database,
    record: TransitionRecord,
    disposition: RecoveryDisposition,
    database: DatabaseEvidence,
    database_stability: DatabaseInspectionStability,
    epoch: RuntimeEpochEvidence,
    trees: Vec<KnownTreeEvidence>,
    namespace: ActivationNamespaceEvidence,
    blockers: Vec<RecoveryBlocker>,
}

impl PendingSystemTransition {
    pub(super) fn inspect(
        installation: &Installation,
        state_db: &db::state::Database,
        journal: TransitionJournalStore,
        record: TransitionRecord,
        in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<Self, InspectionError> {
        let authority = StartupRecoveryAuthority::new(installation, journal, state_db);
        let disposition = record.recovery_disposition();
        let database = inspect_database(&record, authority.state_db(), in_flight)?;

        installation.revalidate_mutable_namespace()?;
        let namespace = ActivationNamespaceInspection::begin(installation, authority.journal(), &record);
        let before = RuntimeEpoch::capture();
        let trees = inspect_known_trees(installation, &record);
        let after = RuntimeEpoch::capture();
        installation.revalidate_mutable_namespace()?;
        let epoch = RuntimeEpochEvidence { before, after };
        run_between_database_inspections();
        let in_flight_after = authority.state_db().audit_in_flight_transition()?;
        let database_after = inspect_database(&record, authority.state_db(), in_flight_after)?;
        installation.revalidate_mutable_namespace()?;
        let namespace = namespace.finish(installation, authority.journal(), &record);
        installation.revalidate_mutable_namespace()?;
        let database_stability = if database == database_after {
            DatabaseInspectionStability::Stable
        } else {
            DatabaseInspectionStability::Changed { after: database_after }
        };

        let mut blockers = Vec::with_capacity(14);
        if !database_ownership_evidence_compatible(&record, &database) {
            blockers.push(RecoveryBlocker::DatabaseConflict);
        }
        if !metadata_provenance_evidence_compatible(&record, &database) {
            blockers.push(RecoveryBlocker::MetadataProvenanceConflict);
        }
        if database_stability != DatabaseInspectionStability::Stable {
            blockers.push(RecoveryBlocker::DatabaseChangedDuringInspection);
        }
        // The legacy fixed-name lane intentionally cannot authorize nlink=2
        // state-slot markers and may observe a transient name before the
        // final database/namespace sandwich.  Once the bounded inventory is
        // exact, its retained descriptor proof supersedes those diagnostic
        // limitations; otherwise retain every legacy blocker as additional
        // fail-closed evidence.
        if !namespace.phase_layout_is_exact() {
            match epoch.comparability(&record) {
                RuntimeEpochComparability::Unavailable => blockers.push(RecoveryBlocker::RuntimeEpochUnavailable),
                RuntimeEpochComparability::ChangedDuringInspection => {
                    blockers.push(RecoveryBlocker::RuntimeEpochChangedDuringInspection);
                }
                RuntimeEpochComparability::Current | RuntimeEpochComparability::RecordedEpochChanged => {}
            }
            if trees.iter().any(|tree| {
                matches!(
                    tree,
                    KnownTreeEvidence::Unresolved {
                        reason: UnresolvedTreeReason::Rejected(_),
                        ..
                    }
                )
            }) {
                blockers.push(RecoveryBlocker::TreeEvidenceRejected);
            }
            if trees.iter().any(|tree| {
                matches!(
                    tree,
                    KnownTreeEvidence::Unresolved {
                        reason: UnresolvedTreeReason::StateSlotLinkUnauthenticated,
                        ..
                    }
                )
            }) {
                blockers.push(RecoveryBlocker::UnresolvedStateSlotLink);
            }
            assess_tree_roles(&record, &epoch, &trees, &mut blockers);
        }
        match namespace.stability() {
            ActivationNamespaceStability::Stable => {}
            ActivationNamespaceStability::Changed => {
                blockers.push(RecoveryBlocker::ActivationNamespaceChangedDuringInspection);
            }
            ActivationNamespaceStability::Rejected => blockers.push(RecoveryBlocker::ActivationNamespaceRejected),
        }
        if !namespace.journal_is_exact() {
            blockers.push(RecoveryBlocker::JournalChangedDuringInspection);
        }
        if !namespace.phase_layout_is_exact() {
            blockers.push(RecoveryBlocker::PhaseNamespaceConflict);
        }
        if record.phase == Phase::UsrExchangeIntent && namespace.usr_exchange_layout() == Some(UsrExchangeLayout::Post)
        {
            blockers.push(RecoveryBlocker::ForwardExchangeDurabilityUnproven);
        }
        if namespace.stability() != ActivationNamespaceStability::Stable || !namespace.journal_is_exact() {
            blockers.push(RecoveryBlocker::ExactNamespaceInventoryRequired);
        }
        if disposition == RecoveryDisposition::ManualBootRepair {
            blockers.push(RecoveryBlocker::ManualBootRepair);
        }
        blockers.sort_unstable();
        blockers.dedup();

        // Retain the exact database connection used for the snapshot, but
        // release the mutable installation/global-lock and exclusive journal
        // authority before returning a diagnostic.  The diagnostic exposes
        // no recovery effects, and keeping the journal here would permit a
        // coordinator -> journal / diagnostic -> coordinator ABBA deadlock.
        let state_db = authority.state_db().clone();
        debug_assert!(state_db.same_instance(authority.state_db()));
        drop(authority);

        Ok(Self {
            state_db,
            record,
            disposition,
            database,
            database_stability,
            epoch,
            trees,
            namespace,
            blockers,
        })
    }

    pub(super) fn transition_id(&self) -> &TransitionId {
        &self.record.transition_id
    }

    pub(super) fn phase(&self) -> Phase {
        self.record.phase
    }

    #[allow(dead_code)] // available to structured client diagnostics
    pub(super) fn disposition(&self) -> RecoveryDisposition {
        self.disposition
    }

    #[cfg(test)]
    fn database_evidence(&self) -> &DatabaseEvidence {
        &self.database
    }

    #[cfg(test)]
    fn database_stability(&self) -> &DatabaseInspectionStability {
        &self.database_stability
    }

    #[cfg(test)]
    pub(super) fn blockers(&self) -> &[RecoveryBlocker] {
        &self.blockers
    }

    #[cfg(test)]
    pub(super) fn retains_database(&self, database: &db::state::Database) -> bool {
        self.state_db.same_instance(database)
    }
}

impl fmt::Display for PendingSystemTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "state transition {} at {:?} requires {:?}; recovery effects remain blocked by {:?}",
            self.transition_id(),
            self.phase(),
            self.disposition,
            self.blockers,
        )
    }
}

impl std::error::Error for PendingSystemTransition {}

impl StartupRecoveryAuthority {
    fn state_db(&self) -> &db::state::Database {
        &self.state_db
    }
}

fn inspect_known_trees(installation: &Installation, record: &TransitionRecord) -> Vec<KnownTreeEvidence> {
    let mut locations = known_tree_locations(installation, record);
    debug_assert!(locations.len() <= MAX_KNOWN_TREE_LOCATIONS);
    locations.drain(..).map(inspect_known_tree).collect()
}

fn inspect_known_tree(location: KnownTreeLocation) -> KnownTreeEvidence {
    let store = match TreeMarkerStore::open_path(location.path.clone()) {
        Err(TreeMarkerError::Io { source, .. }) if source.raw_os_error() == Some(nix::libc::ENOENT) => {
            return unresolved_tree(location, None, UnresolvedTreeReason::Absent);
        }
        Err(source) => return unresolved_tree(location, None, UnresolvedTreeReason::Rejected(source)),
        Ok(store) => store,
    };
    let marker = match store.read_for_transition_recovery() {
        Ok(marker) => marker,
        Err(source) => return unresolved_tree(location, None, UnresolvedTreeReason::Rejected(source)),
    };
    let runtime = RuntimeTreeIdentity::capture_directory(store.retained_directory());
    let reopened = match TreeMarkerStore::open_path(location.path.clone()) {
        Ok(reopened) => reopened,
        Err(source) => {
            let retained = RetainedTreeEvidence {
                location: location.clone(),
                store,
                marker,
                runtime,
            };
            return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
        }
    };
    if let Err(source) = store.require_same_directory(&reopened) {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store,
            marker,
            runtime,
        };
        return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
    }
    let runtime = reconcile_runtime(
        runtime,
        RuntimeTreeIdentity::capture_directory(reopened.retained_directory()),
    );
    if marker.needs_slot_link_authorization() {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store: reopened,
            marker,
            runtime,
        };
        return unresolved_tree(
            location,
            Some(retained),
            UnresolvedTreeReason::StateSlotLinkUnauthenticated,
        );
    }

    let named_marker = match marker.read_named_for_transition(&reopened) {
        Ok(named_marker) => named_marker,
        Err(source) => {
            let retained = RetainedTreeEvidence {
                location: location.clone(),
                store,
                marker,
                runtime,
            };
            return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
        }
    };
    if let Err(source) = marker.require_same_marker(&named_marker) {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store,
            marker,
            runtime,
        };
        return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
    }
    if let Err(source) = named_marker.revalidate(&reopened) {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store,
            marker,
            runtime,
        };
        return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
    }

    run_before_final_tree_reopen();
    let final_store = match TreeMarkerStore::open_path(location.path.clone()) {
        Ok(final_store) => final_store,
        Err(source) => {
            let retained = RetainedTreeEvidence {
                location: location.clone(),
                store: reopened,
                marker: named_marker,
                runtime,
            };
            return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
        }
    };
    if let Err(source) = reopened.require_same_directory(&final_store) {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store: reopened,
            marker: named_marker,
            runtime,
        };
        return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
    }
    let runtime = reconcile_runtime(
        runtime,
        RuntimeTreeIdentity::capture_directory(final_store.retained_directory()),
    );
    let final_marker = match named_marker.read_named_for_transition(&final_store) {
        Ok(final_marker) => final_marker,
        Err(source) => {
            let retained = RetainedTreeEvidence {
                location: location.clone(),
                store: reopened,
                marker: named_marker,
                runtime,
            };
            return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
        }
    };
    if let Err(source) = named_marker.require_same_marker(&final_marker) {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store: reopened,
            marker: named_marker,
            runtime,
        };
        return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
    }
    if let Err(source) = final_marker.revalidate(&final_store) {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store: reopened,
            marker: named_marker,
            runtime,
        };
        return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
    }
    KnownTreeEvidence::Retained(RetainedTreeEvidence {
        location,
        store: final_store,
        marker: final_marker,
        runtime,
    })
}

fn reconcile_runtime(
    before: Result<RuntimeTreeIdentity, RuntimeEvidenceError>,
    after: Result<RuntimeTreeIdentity, RuntimeEvidenceError>,
) -> Result<RuntimeTreeIdentity, RuntimeEvidenceError> {
    match (before, after) {
        (Ok(before), Ok(after)) if before == after => Ok(after),
        (Ok(_), Ok(_)) => Err(RuntimeEvidenceError::TreeChanged),
        (Err(source), _) | (_, Err(source)) => Err(source),
    }
}

fn unresolved_tree(
    location: KnownTreeLocation,
    retained: Option<RetainedTreeEvidence>,
    reason: UnresolvedTreeReason,
) -> KnownTreeEvidence {
    KnownTreeEvidence::Unresolved {
        location,
        retained,
        reason,
    }
}

fn assess_tree_roles(
    record: &TransitionRecord,
    epoch: &RuntimeEpochEvidence,
    trees: &[KnownTreeEvidence],
    blockers: &mut Vec<RecoveryBlocker>,
) {
    let mut candidate_count = 0usize;
    let mut previous_count = 0usize;
    for retained in trees.iter().filter_map(retained_tree) {
        let durable = retained.durable_role(record);
        match durable {
            DurableTreeRole::Candidate => candidate_count += 1,
            DurableTreeRole::Previous => previous_count += 1,
            DurableTreeRole::Foreign => blockers.push(RecoveryBlocker::DurableTreeIdentityConflict),
        }
        if epoch.comparability(record) != RuntimeEpochComparability::Current {
            continue;
        }
        match (durable, retained.runtime_role(record, epoch)) {
            (_, RuntimeTreeRole::Unavailable) => blockers.push(RecoveryBlocker::RuntimeTreeEvidenceUnavailable),
            (DurableTreeRole::Candidate, RuntimeTreeRole::Candidate)
            | (DurableTreeRole::Previous, RuntimeTreeRole::Previous) => {}
            (_, RuntimeTreeRole::NotComparable) => {
                blockers.push(RecoveryBlocker::RuntimeEpochChangedDuringInspection);
            }
            _ => blockers.push(RecoveryBlocker::RuntimeTreeIdentityConflict),
        }
    }
    if candidate_count > 1 || previous_count > 1 {
        blockers.push(RecoveryBlocker::DurableTreeIdentityConflict);
    }
}

fn retained_tree(tree: &KnownTreeEvidence) -> Option<&RetainedTreeEvidence> {
    match tree {
        KnownTreeEvidence::Retained(retained) => Some(retained),
        KnownTreeEvidence::Unresolved { retained, .. } => retained.as_ref(),
    }
}

fn known_tree_locations(installation: &Installation, record: &TransitionRecord) -> Vec<KnownTreeLocation> {
    let mut locations = Vec::with_capacity(MAX_KNOWN_TREE_LOCATIONS);
    add_location(&mut locations, installation.root.join("usr"), KnownTreeRole::Live);
    add_location(&mut locations, installation.staging_path("usr"), KnownTreeRole::Staging);
    if let Some(candidate) = record.candidate.id.map(state::Id::from) {
        add_location(
            &mut locations,
            installation.root_path(i32::from(candidate).to_string()).join("usr"),
            KnownTreeRole::CandidateState(candidate),
        );
    }
    if let Some(previous) = record.previous.id.map(state::Id::from) {
        add_location(
            &mut locations,
            installation.root_path(i32::from(previous).to_string()).join("usr"),
            KnownTreeRole::PreviousState(previous),
        );
    }
    add_location(
        &mut locations,
        installation
            .state_quarantine_dir()
            .join(record.quarantine_name.as_str())
            .join("usr"),
        KnownTreeRole::Quarantine,
    );
    locations
}

fn add_location(locations: &mut Vec<KnownTreeLocation>, path: PathBuf, role: KnownTreeRole) {
    if let Some(existing) = locations.iter_mut().find(|existing| existing.path == path) {
        existing.roles.push(role);
    } else {
        locations.push(KnownTreeLocation {
            path,
            roles: vec![role],
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum InspectionError {
    #[error("inspect exact state-transition database ownership")]
    Database(#[from] db::state::TransitionEvidenceError),
    #[error("inspect exact generated-metadata provenance")]
    MetadataProvenance(#[from] db::state::MetadataProvenanceError),
    #[error("revalidate retained mutable installation namespace around recovery evidence inspection")]
    Installation(#[from] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_INSPECTIONS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_FINAL_TREE_REOPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_between_database_inspections(hook: impl FnOnce() + 'static) {
    BETWEEN_DATABASE_INSPECTIONS.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_between_database_inspections() {
    BETWEEN_DATABASE_INSPECTIONS.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_between_database_inspections() {}

#[cfg(test)]
fn arm_before_final_tree_reopen(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_TREE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_final_tree_reopen() {
    BEFORE_FINAL_TREE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_final_tree_reopen() {}

#[cfg(test)]
mod tests;
