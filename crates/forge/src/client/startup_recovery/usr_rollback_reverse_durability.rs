//! Sealed, unwired executor for reverse `/usr` parent durability.
//!
//! The namespace capability owns the descriptor-bound staging-parent and
//! installation-root sync sequence. This executor owns the production seal
//! which permits the already reconciled Applied or AlreadySatisfied authority
//! to enter that sequence. It performs no journal persistence or other
//! recovery action.

use thiserror::Error;

use crate::transition_journal::TransitionJournalStore;

use super::super::startup_reconciliation::{
    UsrRollbackReverseAlreadySatisfiedEffectAuthority, UsrRollbackReverseAppliedEffectAuthority,
    UsrRollbackReverseAuthorityError, UsrRollbackReverseDurableEffectAuthority,
};

/// Unforgeable permission to enter the reverse parent-durability suffix.
pub(in crate::client) struct UsrRollbackReverseDurabilitySeal {
    _private: (),
}

impl UsrRollbackReverseDurabilitySeal {
    fn new() -> Self {
        Self { _private: () }
    }
}

/// Complete durability after this invocation applied POST-to-PRE.
#[allow(dead_code)] // intentionally unwired until the reverse dispatcher lands
pub(in crate::client) fn complete_applied_usr_rollback_reverse_durability<'reservation>(
    journal: &TransitionJournalStore,
    authority: UsrRollbackReverseAppliedEffectAuthority<'reservation>,
) -> Result<UsrRollbackReverseDurableEffectAuthority<'reservation>, UsrRollbackReverseDurabilityError> {
    authority
        .complete_parent_durability(&UsrRollbackReverseDurabilitySeal::new(), journal)
        .map_err(UsrRollbackReverseDurabilityError::from)
}

/// Complete durability after this invocation admitted exact PRE without an
/// exchange attempt.
#[allow(dead_code)] // intentionally unwired until the reverse dispatcher lands
pub(in crate::client) fn complete_already_satisfied_usr_rollback_reverse_durability<'reservation>(
    journal: &TransitionJournalStore,
    authority: UsrRollbackReverseAlreadySatisfiedEffectAuthority<'reservation>,
) -> Result<UsrRollbackReverseDurableEffectAuthority<'reservation>, UsrRollbackReverseDurabilityError> {
    authority
        .complete_parent_durability(&UsrRollbackReverseDurabilitySeal::new(), journal)
        .map_err(UsrRollbackReverseDurabilityError::from)
}

#[derive(Debug, Error)]
#[error("complete exact reverse /usr parent durability")]
pub(in crate::client) struct UsrRollbackReverseDurabilityError(#[from] UsrRollbackReverseAuthorityError);
