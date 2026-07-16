//! Narrow, phase-specific mutable startup recovery effects.
//!
//! This module is separate from diagnostic startup reconciliation. Its
//! executors consume exact mutation authority. Success may return only a
//! freshly reopened lock-bearing journal store for uninterrupted diagnostic
//! inspection; failure returns neither a store nor reusable authority.

mod usr_exchange_parent_durability;
mod usr_rollback_decision;
mod usr_rollback_resume_route;

pub(in crate::client) use usr_exchange_parent_durability::{
    UsrExchangeParentDurabilityCompletionSeal, UsrExchangeParentDurabilityError,
    normalize_usr_exchange_parent_durability,
};

#[cfg(test)]
#[allow(unused_imports)] // exported for focused parent-durability contracts
pub(crate) use usr_exchange_parent_durability::{
    UsrExchangeParentDurabilityEvent, UsrExchangeParentDurabilityFaultPoint,
    arm_before_usr_exchange_parent_durability_final_revalidation, arm_usr_exchange_parent_durability_fault,
    reset_usr_exchange_parent_durability_events, take_usr_exchange_parent_durability_events,
};

#[allow(unused_imports)] // unwired until the startup recovery dispatcher lands
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

#[cfg(test)]
pub(crate) use usr_rollback_resume_route::arm_before_usr_rollback_resume_route_final_revalidation;
