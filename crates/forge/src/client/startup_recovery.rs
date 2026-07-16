//! Narrow, phase-specific mutable startup recovery effects.
//!
//! This module is separate from diagnostic startup reconciliation. Its
//! executors consume exact mutation authority. Success may return only a
//! freshly reopened lock-bearing journal store for uninterrupted diagnostic
//! inspection; failure returns neither a store nor reusable authority.

mod usr_rollback_decision;

#[allow(unused_imports)] // unwired until the startup recovery dispatcher lands
pub(super) use usr_rollback_decision::{
    DurableUsrRollbackDecisionRecord, UsrRollbackDecisionPersistenceError, UsrRollbackDecisionReopenError,
    persist_usr_rollback_decision_and_reopen,
};

#[cfg(test)]
#[allow(unused_imports)] // consumed by focused startup recovery race tests
pub(crate) use usr_rollback_decision::arm_before_usr_rollback_decision_final_revalidation;
