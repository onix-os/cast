//! Narrow, phase-specific mutable startup recovery effects.
//!
//! This module is separate from diagnostic startup reconciliation. Its
//! executors consume exact mutation authority. Success returns a lock-bearing
//! journal store paired with either an exact canonical record or proven
//! terminal absence for uninterrupted startup inspection; failure returns
//! neither a store nor reusable authority.

mod canonical_journal_reopen;
mod usr_exchange_parent_durability;
mod usr_exchanged_root_abi_normalization;
mod usr_rollback_activate_archived_candidate_preserve_persistence;
mod usr_rollback_activate_archived_complete_route;
mod usr_rollback_activate_archived_finalization;
mod usr_rollback_active_reblit_boot_repair_complete;
mod usr_rollback_active_reblit_boot_repair_required;
mod usr_rollback_active_reblit_boot_repair_unverified;
mod usr_rollback_active_reblit_candidate_preserve_persistence;
mod usr_rollback_active_reblit_complete_route;
mod usr_rollback_active_reblit_finalization;
mod usr_rollback_candidate_preserve_dispatch;
mod usr_rollback_candidate_preserve_persistence;
mod usr_rollback_complete_route;
mod usr_rollback_decision;
mod usr_rollback_finalization;
mod usr_rollback_fresh_db_invalidation_dispatch;
mod usr_rollback_fresh_db_invalidation_persistence;
mod usr_rollback_fresh_db_invalidation_route;
mod usr_rollback_resume_route;
mod usr_rollback_reverse_dispatch;
mod usr_rollback_reverse_durability;
mod usr_rollback_reverse_persistence;

/// Unforgeable permission to consume read-only rollback-reverse admission
/// into mutable effect typestate. The production constructor is private to
/// this module and its phase-specific executor descendants.
pub(in crate::client) struct UsrRollbackReverseEffectSeal {
    _private: (),
}

impl UsrRollbackReverseEffectSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self { _private: () }
    }
}

pub(in crate::client) use usr_exchange_parent_durability::{
    UsrExchangeParentDurabilityCompletionSeal, UsrExchangeParentDurabilityError,
    normalize_usr_exchange_parent_durability,
};
pub(super) use usr_exchanged_root_abi_normalization::{
    UsrExchangedRootAbiNormalizationExecutionError, normalize_usr_exchanged_root_abi,
    synchronize_usr_exchanged_root_abi,
};
pub(in crate::client) use usr_rollback_reverse_durability::{
    UsrRollbackReverseDurabilityError, UsrRollbackReverseDurabilitySeal,
    complete_already_satisfied_usr_rollback_reverse_durability, complete_applied_usr_rollback_reverse_durability,
};

#[cfg(test)]
#[allow(unused_imports)] // exported for focused parent-durability contracts
pub(crate) use usr_exchange_parent_durability::{
    UsrExchangeParentDurabilityEvent, UsrExchangeParentDurabilityFaultPoint,
    arm_before_usr_exchange_parent_durability_final_revalidation, arm_usr_exchange_parent_durability_fault,
    reset_usr_exchange_parent_durability_events, take_usr_exchange_parent_durability_events,
};

#[allow(unused_imports)] // detailed outcome types remain available to focused persistence contracts
pub(super) use usr_rollback_decision::{
    DurableUsrRollbackDecisionRecord, UsrRollbackDecisionPersistenceError, UsrRollbackDecisionReopenError,
    persist_usr_rollback_decision_and_reopen,
};

#[cfg(test)]
#[allow(unused_imports)] // consumed by focused startup recovery race tests
pub(crate) use usr_rollback_decision::arm_before_usr_rollback_decision_final_revalidation;

#[allow(unused_imports)] // error details are retained for focused persistence contracts
pub(super) use usr_rollback_resume_route::{
    DurableUsrRollbackResumeRouteRecord, UsrRollbackResumeRoutePersistenceError, UsrRollbackResumeRouteReopenError,
    persist_usr_rollback_resume_route_and_reopen,
};

pub(super) use usr_rollback_reverse_dispatch::{
    UsrRollbackReverseDispatchError, UsrRollbackReverseReady, dispatch_usr_rollback_reverse_and_reopen,
};

pub(super) use usr_rollback_reverse_persistence::{
    UsrRollbackReversePersistenceError, persist_usr_rollback_reverse_and_reopen,
};

pub(super) use usr_rollback_candidate_preserve_dispatch::{
    UsrRollbackActiveReblitCandidatePreserveDurabilitySeal, UsrRollbackArchivedCandidatePreserveDurabilitySeal,
    UsrRollbackCandidatePreserveDispatchError, UsrRollbackCandidatePreserveDurabilitySeal,
    UsrRollbackCandidatePreserveEffectSeal, UsrRollbackCandidatePreserveReady,
    dispatch_usr_rollback_candidate_preserve_and_reopen,
};

pub(super) use usr_rollback_candidate_preserve_persistence::{
    UsrRollbackCandidatePreservePersistenceError, persist_usr_rollback_candidate_preserve_and_reopen,
};

pub(super) use usr_rollback_active_reblit_candidate_preserve_persistence::{
    UsrRollbackActiveReblitCandidatePreservePersistenceError,
    persist_usr_rollback_active_reblit_candidate_preserve_and_reopen,
};

pub(super) use usr_rollback_activate_archived_candidate_preserve_persistence::{
    UsrRollbackArchivedCandidatePreservePersistenceError, persist_usr_rollback_archived_candidate_preserve_and_reopen,
};

pub(super) use usr_rollback_active_reblit_complete_route::{
    UsrRollbackActiveReblitCompleteRoutePersistenceError, persist_usr_rollback_active_reblit_complete_route_and_reopen,
};

pub(super) use usr_rollback_active_reblit_boot_repair_complete::{
    UsrRollbackActiveReblitBootRepairCompletePersistenceError,
    persist_usr_rollback_active_reblit_boot_repair_complete_and_reopen,
};

pub(super) use usr_rollback_active_reblit_boot_repair_required::{
    UsrRollbackActiveReblitBootRepairRequiredPersistenceError,
    persist_usr_rollback_active_reblit_boot_repair_required_and_reopen,
};

pub(super) use usr_rollback_active_reblit_boot_repair_unverified::{
    UsrRollbackActiveReblitBootRepairUnverifiedPersistenceError,
    persist_usr_rollback_active_reblit_boot_repair_unverified_and_reopen,
};

pub(super) use usr_rollback_active_reblit_finalization::{
    UsrRollbackActiveReblitFinalizationError, finalize_usr_rollback_active_reblit,
};

pub(super) use usr_rollback_activate_archived_complete_route::{
    UsrRollbackActivateArchivedCompleteRoutePersistenceError,
    persist_usr_rollback_activate_archived_complete_route_and_reopen,
};

pub(super) use usr_rollback_activate_archived_finalization::{
    UsrRollbackActivateArchivedFinalizationError, finalize_usr_rollback_activate_archived,
};

#[cfg(test)]
pub(in crate::client) use usr_rollback_active_reblit_candidate_preserve_persistence::{
    DurableUsrRollbackActiveReblitCandidatePreserveRecord,
    arm_before_usr_rollback_active_reblit_candidate_preserve_persistence_final_revalidation,
};

#[cfg(test)]
pub(in crate::client) use usr_rollback_activate_archived_candidate_preserve_persistence::{
    DurableUsrRollbackArchivedCandidatePreserveRecord,
    arm_before_usr_rollback_archived_candidate_preserve_persistence_final_revalidation,
};

#[cfg(test)]
pub(in crate::client) use usr_rollback_active_reblit_complete_route::{
    DurableUsrRollbackActiveReblitCompleteRouteRecord,
    arm_before_usr_rollback_active_reblit_complete_route_final_revalidation,
};

#[cfg(test)]
pub(in crate::client) use usr_rollback_active_reblit_boot_repair_complete::{
    DurableUsrRollbackActiveReblitBootRepairCompleteRecord,
    arm_before_usr_rollback_active_reblit_boot_repair_complete_final_revalidation,
};

#[cfg(test)]
pub(in crate::client) use usr_rollback_active_reblit_boot_repair_required::{
    DurableUsrRollbackActiveReblitBootRepairRequiredRecord,
    arm_before_usr_rollback_active_reblit_boot_repair_required_final_revalidation,
};

#[cfg(test)]
pub(in crate::client) use usr_rollback_active_reblit_boot_repair_unverified::DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord;

#[cfg(test)]
pub(in crate::client) use usr_rollback_active_reblit_finalization::{
    DurableUsrRollbackActiveReblitFinalizationRecord, UsrRollbackActiveReblitFinalizationVerificationError,
};

#[cfg(test)]
pub(in crate::client) use usr_rollback_activate_archived_complete_route::{
    DurableUsrRollbackActivateArchivedCompleteRouteRecord,
    arm_before_usr_rollback_activate_archived_complete_route_final_revalidation,
};

#[cfg(test)]
pub(in crate::client) use usr_rollback_activate_archived_finalization::{
    DurableUsrRollbackActivateArchivedFinalizationRecord, UsrRollbackActivateArchivedFinalizationVerificationError,
};

pub(super) use usr_rollback_complete_route::{
    UsrRollbackCompleteRoutePersistenceError, persist_usr_rollback_complete_route_and_reopen,
};

pub(super) use usr_rollback_finalization::{UsrRollbackFinalizationError, finalize_usr_rollback};

pub(super) use usr_rollback_fresh_db_invalidation_dispatch::{
    UsrRollbackFreshDbInvalidationDispatchError, UsrRollbackFreshDbInvalidationEffectSeal,
    UsrRollbackFreshDbInvalidationReady, dispatch_usr_rollback_fresh_db_invalidation_and_reopen,
};

pub(super) use usr_rollback_fresh_db_invalidation_persistence::{
    UsrRollbackFreshDbInvalidationPersistenceError, persist_usr_rollback_fresh_db_invalidation_and_reopen,
};

pub(super) use usr_rollback_fresh_db_invalidation_route::{
    UsrRollbackFreshDbInvalidationRoutePersistenceError, persist_usr_rollback_fresh_db_invalidation_route_and_reopen,
};

#[cfg(test)]
pub(in crate::client) use usr_rollback_candidate_preserve_persistence::DurableUsrRollbackCandidatePreserveRecord;

#[cfg(test)]
pub(in crate::client) use usr_rollback_complete_route::DurableUsrRollbackCompleteRouteRecord;

#[cfg(test)]
pub(in crate::client) use usr_rollback_finalization::{
    DurableUsrRollbackFinalizationRecord, UsrRollbackFinalizationVerificationError,
};

#[cfg(test)]
pub(in crate::client) use usr_rollback_fresh_db_invalidation_persistence::DurableUsrRollbackFreshDbInvalidationRecord;

#[cfg(test)]
pub(in crate::client) use usr_rollback_fresh_db_invalidation_route::{
    DurableUsrRollbackFreshDbInvalidationRouteRecord,
    UsrRollbackFreshDbInvalidationRouteSuccessorBindingError,
    arm_after_usr_rollback_fresh_db_invalidation_route_successor_binding_check_before_reopen,
    arm_before_usr_rollback_fresh_db_invalidation_route_successor_binding_revalidation,
};

#[cfg(test)]
pub(in crate::client) use usr_rollback_reverse_persistence::DurableUsrRollbackReverseRecord;

#[cfg(test)]
pub(crate) use usr_rollback_resume_route::arm_before_usr_rollback_resume_route_final_revalidation;

#[cfg(test)]
pub(crate) use usr_rollback_reverse_persistence::arm_before_usr_rollback_reverse_persistence_final_revalidation;

#[cfg(test)]
pub(crate) use usr_rollback_candidate_preserve_persistence::arm_before_usr_rollback_candidate_preserve_persistence_final_revalidation;

#[cfg(test)]
pub(crate) use usr_rollback_complete_route::arm_before_usr_rollback_complete_route_final_revalidation;

#[cfg(test)]
pub(crate) use usr_rollback_active_reblit_finalization::{
    arm_after_usr_rollback_active_reblit_finalization_delete,
    arm_before_usr_rollback_active_reblit_finalization_final_durable_inspection,
    arm_before_usr_rollback_active_reblit_finalization_final_revalidation,
};

#[cfg(test)]
pub(crate) use usr_rollback_activate_archived_finalization::{
    arm_after_usr_rollback_activate_archived_finalization_delete,
    arm_before_usr_rollback_activate_archived_finalization_final_durable_inspection,
    arm_before_usr_rollback_activate_archived_finalization_final_revalidation,
};

#[cfg(test)]
pub(crate) use usr_rollback_finalization::{
    arm_after_usr_rollback_finalization_delete, arm_before_usr_rollback_finalization_final_durable_inspection,
    arm_before_usr_rollback_finalization_final_revalidation,
};

#[cfg(test)]
pub(crate) use usr_rollback_fresh_db_invalidation_persistence::arm_before_usr_rollback_fresh_db_invalidation_persistence_final_revalidation;

#[cfg(test)]
pub(crate) use usr_rollback_fresh_db_invalidation_route::arm_before_usr_rollback_fresh_db_invalidation_route_final_revalidation;
