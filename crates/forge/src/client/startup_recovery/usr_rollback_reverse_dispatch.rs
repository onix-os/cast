//! Consuming dispatcher for one exact startup `/usr` rollback-reverse phase.
//!
//! Only sealed POST or PRE admission can enter this boundary. POST consumes
//! exactly one exchange attempt and PRE consumes none. Both successful paths
//! converge only after parent durability, then use the shared exact
//! `UsrRestored` persistence boundary once. A semantic non-application or
//! ambiguity is terminal for this startup entry and returns no reusable store
//! or mutation authority.

use thiserror::Error;

use crate::transition_journal::{TransitionJournalStore, TransitionRecord};

use super::super::startup_reconciliation::{
    UsrRollbackReverseApplyAuthority, UsrRollbackReverseApplyReconciliation, UsrRollbackReverseAuthorityError,
    UsrRollbackReverseFinishAuthority,
};
use super::{
    UsrRollbackReverseDurabilityError, UsrRollbackReverseEffectSeal, UsrRollbackReversePersistenceError,
    complete_already_satisfied_usr_rollback_reverse_durability, complete_applied_usr_rollback_reverse_durability,
    persist_usr_rollback_reverse_and_reopen,
};

#[cfg(test)]
#[allow(dead_code)] // shared reverse fixture contains wider reconciliation helpers
#[path = "../startup_reconciliation/usr_rollback_reverse_authority/tests/support.rs"]
mod reverse_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider startup-recovery helpers
#[path = "test_support.rs"]
mod test_fixture;
#[cfg(test)]
mod tests;

/// Exact read-only reverse admission ready for one consuming startup effect.
pub(in crate::client) enum UsrRollbackReverseReady<'reservation> {
    Apply(UsrRollbackReverseApplyAuthority<'reservation>),
    Finish(UsrRollbackReverseFinishAuthority<'reservation>),
}

/// Consume one admitted reverse phase through effect, durability, and exact
/// journal persistence.
pub(in crate::client) fn dispatch_usr_rollback_reverse_and_reopen<'reservation>(
    journal: TransitionJournalStore,
    ready: UsrRollbackReverseReady<'reservation>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackReverseDispatchError> {
    let effect_seal = UsrRollbackReverseEffectSeal::new();
    let durable = match ready {
        UsrRollbackReverseReady::Apply(authority) => {
            let lease = authority.into_effect_lease(&effect_seal, &journal)?;
            match lease.reconcile(&effect_seal, &journal)? {
                UsrRollbackReverseApplyReconciliation::Applied(authority) => {
                    complete_applied_usr_rollback_reverse_durability(&journal, authority)?
                }
                UsrRollbackReverseApplyReconciliation::NotApplied => {
                    drop(journal);
                    return Err(UsrRollbackReverseDispatchError::NotApplied);
                }
                UsrRollbackReverseApplyReconciliation::Ambiguous => {
                    drop(journal);
                    return Err(UsrRollbackReverseDispatchError::Ambiguous);
                }
            }
        }
        UsrRollbackReverseReady::Finish(authority) => {
            let lease = authority.into_effect_lease(&effect_seal, &journal)?;
            let authority = lease.reconcile(&effect_seal, &journal)?;
            complete_already_satisfied_usr_rollback_reverse_durability(&journal, authority)?
        }
    };

    persist_usr_rollback_reverse_and_reopen(journal, durable).map_err(UsrRollbackReverseDispatchError::from)
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackReverseDispatchError {
    #[error("consume and reconcile exact startup /usr rollback-reverse authority")]
    Authority(#[from] UsrRollbackReverseAuthorityError),
    #[error("complete exact startup /usr rollback-reverse parent durability")]
    Durability(#[from] UsrRollbackReverseDurabilityError),
    #[error("persist exact startup /usr rollback-reverse outcome")]
    Persistence(#[from] UsrRollbackReversePersistenceError),
    #[error("one-shot startup /usr rollback-reverse attempt was not applied")]
    NotApplied,
    #[error("one-shot startup /usr rollback-reverse attempt has ambiguous namespace evidence")]
    Ambiguous,
}
