//! Consuming leaf for one exact fresh-database invalidation checkpoint.
//!
//! Present evidence permits at most one exact removal call. Joint absence
//! permits none. Only those two proved success origins reach the existing
//! single-advance persistence boundary, and no result can continue into the
//! following rollback-completion route in this invocation.

use thiserror::Error;

use crate::transition_journal::{TransitionJournalStore, TransitionRecord};

use super::super::startup_reconciliation::{
    UsrRollbackFreshDbInvalidationApplyAuthority, UsrRollbackFreshDbInvalidationApplyReconciliation,
    UsrRollbackFreshDbInvalidationAuthorityError, UsrRollbackFreshDbInvalidationFinishAuthority,
};
use super::{UsrRollbackFreshDbInvalidationPersistenceError, persist_usr_rollback_fresh_db_invalidation_and_reopen};

/// Unforgeable permission to consume exact `FreshDbInvalidationIntent`
/// admission through the one-attempt database invalidation effect. The
/// private field is rooted in this exact consuming leaf.
pub(in crate::client) struct UsrRollbackFreshDbInvalidationEffectSeal {
    _private: (),
}

impl UsrRollbackFreshDbInvalidationEffectSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Exact read-only fresh-database invalidation admission ready for one
/// consuming startup checkpoint.
pub(in crate::client) enum UsrRollbackFreshDbInvalidationReady<'reservation> {
    Apply(UsrRollbackFreshDbInvalidationApplyAuthority<'reservation>),
    Finish(UsrRollbackFreshDbInvalidationFinishAuthority<'reservation>),
}

/// Consume at most one exact removal and persist `FreshDbInvalidated` once.
pub(in crate::client) fn dispatch_usr_rollback_fresh_db_invalidation_and_reopen<'reservation>(
    journal: TransitionJournalStore,
    ready: UsrRollbackFreshDbInvalidationReady<'reservation>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackFreshDbInvalidationDispatchError> {
    // This is the sole production constructor and its capability never leaves
    // the consuming invalidation leaf.
    let effect_seal = UsrRollbackFreshDbInvalidationEffectSeal::new();
    let authority = match ready {
        UsrRollbackFreshDbInvalidationReady::Apply(authority) => match authority.reconcile(&effect_seal, &journal)? {
            UsrRollbackFreshDbInvalidationApplyReconciliation::Applied(authority) => authority,
            UsrRollbackFreshDbInvalidationApplyReconciliation::NotApplied => {
                drop(journal);
                return Err(UsrRollbackFreshDbInvalidationDispatchError::NotApplied);
            }
            UsrRollbackFreshDbInvalidationApplyReconciliation::Ambiguous => {
                drop(journal);
                return Err(UsrRollbackFreshDbInvalidationDispatchError::Ambiguous);
            }
        },
        UsrRollbackFreshDbInvalidationReady::Finish(authority) => authority.reconcile(&effect_seal, &journal)?,
    };

    persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority)
        .map_err(UsrRollbackFreshDbInvalidationDispatchError::from)
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackFreshDbInvalidationDispatchError {
    #[error("consume and reconcile exact fresh-database invalidation authority")]
    Authority(#[from] UsrRollbackFreshDbInvalidationAuthorityError),
    #[error("persist exact fresh-database invalidation outcome")]
    Persistence(#[from] UsrRollbackFreshDbInvalidationPersistenceError),
    #[error("one-shot fresh-database invalidation attempt was not applied")]
    NotApplied,
    #[error("one-shot fresh-database invalidation attempt has ambiguous database evidence")]
    Ambiguous,
}
