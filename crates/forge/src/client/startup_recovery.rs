//! Narrow, phase-specific mutable startup recovery effects.
//!
//! This module is separate from diagnostic startup reconciliation. Its
//! executors consume exact mutation authority. Success may return only a
//! freshly reopened lock-bearing journal store for uninterrupted diagnostic
//! inspection; failure returns neither a store nor reusable authority.

mod canonical_journal_reopen;
mod usr_exchange_parent_durability;
#[allow(dead_code)] // candidate persistence remains sealed from production dispatch
mod usr_rollback_candidate_preserve_persistence;
mod usr_rollback_decision;
#[allow(dead_code)] // fresh-database invalidation routing remains sealed from production dispatch
mod usr_rollback_fresh_db_invalidation_route;
mod usr_rollback_resume_route;
mod usr_rollback_reverse_dispatch;
mod usr_rollback_reverse_durability;
mod usr_rollback_reverse_persistence;

/// Unforgeable permission to consume read-only candidate-preservation
/// admission into test-only NewState target-creation, normalization, or move
/// checkpoints.
///
/// Production deliberately has no constructor until the complete effect,
/// durability, persistence, and dispatcher boundaries are ready together.
#[allow(dead_code)] // the checkpoint remains unreachable from production
pub(in crate::client) struct UsrRollbackCandidatePreserveEffectSeal {
    _private: (),
}

impl UsrRollbackCandidatePreserveEffectSeal {
    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self { _private: () }
    }
}

/// Unforgeable permission to consume exact NewState POST authority through
/// the candidate-preservation durability suffix.
///
/// Production deliberately has no constructor until persistence and the full
/// candidate-preservation dispatcher are ready as one recovery boundary.
#[allow(dead_code)] // the checkpoint remains unreachable from production
pub(in crate::client) struct UsrRollbackCandidatePreserveDurabilitySeal {
    _private: (),
}

impl UsrRollbackCandidatePreserveDurabilitySeal {
    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self { _private: () }
    }
}

/// Unforgeable permission to consume exact `FreshDbInvalidationIntent`
/// admission through the one-attempt database invalidation effect.
///
/// Production deliberately has no constructor until persistence and the full
/// fresh-database invalidation dispatcher are ready as one recovery boundary.
#[allow(dead_code)] // invalidation remains intentionally unreachable from production
pub(in crate::client) struct UsrRollbackFreshDbInvalidationEffectSeal {
    _private: (),
}

impl UsrRollbackFreshDbInvalidationEffectSeal {
    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self { _private: () }
    }
}

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

#[allow(unused_imports)] // candidate persistence remains sealed from production dispatch
pub(super) use usr_rollback_candidate_preserve_persistence::{
    UsrRollbackCandidatePreservePersistenceError, persist_usr_rollback_candidate_preserve_and_reopen,
};

#[allow(unused_imports)] // fresh-database invalidation routing remains sealed from production dispatch
pub(super) use usr_rollback_fresh_db_invalidation_route::{
    UsrRollbackFreshDbInvalidationRoutePersistenceError, UsrRollbackFreshDbInvalidationRouteReopenError,
    persist_usr_rollback_fresh_db_invalidation_route_and_reopen,
};

#[cfg(test)]
pub(in crate::client) use usr_rollback_candidate_preserve_persistence::DurableUsrRollbackCandidatePreserveRecord;

#[cfg(test)]
pub(in crate::client) use usr_rollback_fresh_db_invalidation_route::DurableUsrRollbackFreshDbInvalidationRouteRecord;

#[cfg(test)]
pub(in crate::client) use usr_rollback_reverse_persistence::DurableUsrRollbackReverseRecord;

#[cfg(test)]
pub(crate) use usr_rollback_resume_route::arm_before_usr_rollback_resume_route_final_revalidation;

#[cfg(test)]
pub(crate) use usr_rollback_reverse_persistence::arm_before_usr_rollback_reverse_persistence_final_revalidation;

#[cfg(test)]
pub(crate) use usr_rollback_candidate_preserve_persistence::arm_before_usr_rollback_candidate_preserve_persistence_final_revalidation;

#[cfg(test)]
pub(crate) use usr_rollback_fresh_db_invalidation_route::arm_before_usr_rollback_fresh_db_invalidation_route_final_revalidation;
