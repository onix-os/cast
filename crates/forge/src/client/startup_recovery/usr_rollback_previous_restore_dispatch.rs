//! Consuming dispatcher for one exact `PreviousRestoreIntent` phase.
//!
//! Only sealed `Archived` or `Restored` admission can enter this boundary.
//! `Archived` consumes exactly one compensating move and `Restored` consumes
//! none; both converge on the shared exact `PreviousRestoredToStaging`
//! persistence boundary once. A semantic non-application or ambiguity is
//! terminal for this startup entry and returns no reusable store or authority.

use thiserror::Error;

use crate::transition_journal::{TransitionJournalStore, TransitionRecord};

use super::super::startup_reconciliation::{
    UsrRollbackPreviousRestoreApplyAuthority, UsrRollbackPreviousRestoreApplyReconciliation,
    UsrRollbackPreviousRestoreAuthorityError, UsrRollbackPreviousRestoreFinishAuthority,
};
use super::{
    UsrRollbackPreviousRestoreEffectSeal, UsrRollbackPreviousRestorePersistenceError,
    persist_usr_rollback_previous_restore_and_reopen,
};

/// Exact read-only previous-restore admission ready for one consuming effect.
pub(in crate::client) enum UsrRollbackPreviousRestoreReady<'reservation> {
    Apply(UsrRollbackPreviousRestoreApplyAuthority<'reservation>),
    Finish(UsrRollbackPreviousRestoreFinishAuthority<'reservation>),
}

/// Consume one admitted previous-restore phase through effect and exact
/// journal persistence.
pub(in crate::client) fn dispatch_usr_rollback_previous_restore_and_reopen<'reservation>(
    journal: TransitionJournalStore,
    ready: UsrRollbackPreviousRestoreReady<'reservation>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackPreviousRestoreDispatchError> {
    let effect_seal = UsrRollbackPreviousRestoreEffectSeal::new();
    let durable = match ready {
        UsrRollbackPreviousRestoreReady::Apply(authority) => {
            let lease = authority.into_effect_lease(&effect_seal, &journal)?;
            match lease.reconcile(&effect_seal, &journal)? {
                UsrRollbackPreviousRestoreApplyReconciliation::Applied(authority) => authority,
                UsrRollbackPreviousRestoreApplyReconciliation::NotApplied => {
                    drop(journal);
                    return Err(UsrRollbackPreviousRestoreDispatchError::NotApplied);
                }
                UsrRollbackPreviousRestoreApplyReconciliation::Ambiguous => {
                    drop(journal);
                    return Err(UsrRollbackPreviousRestoreDispatchError::Ambiguous);
                }
            }
        }
        UsrRollbackPreviousRestoreReady::Finish(authority) => {
            let lease = authority.into_effect_lease(&effect_seal, &journal)?;
            lease.reconcile(&effect_seal, &journal)?
        }
    };

    persist_usr_rollback_previous_restore_and_reopen(journal, durable)
        .map_err(UsrRollbackPreviousRestoreDispatchError::from)
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackPreviousRestoreDispatchError {
    #[error("consume and reconcile exact startup previous-restore authority")]
    Authority(#[from] UsrRollbackPreviousRestoreAuthorityError),
    #[error("persist exact startup previous-restore outcome")]
    Persistence(#[from] UsrRollbackPreviousRestorePersistenceError),
    #[error("one-shot startup previous-restore attempt was not applied")]
    NotApplied,
    #[error("one-shot startup previous-restore attempt has ambiguous namespace evidence")]
    Ambiguous,
}
