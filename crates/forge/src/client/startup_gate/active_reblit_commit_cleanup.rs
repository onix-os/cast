//! One-entry production dispatch for exact forward ActiveReblit cleanup.
//!
//! `CommitDecided` admission may attempt one cleanup exchange, complete the
//! shared durability suffix, and persist one exact `CommitCleanupComplete`
//! successor. Every handled source or successor returns immediately.

use thiserror::Error;

use crate::{
    Installation, db,
    transition_journal::{Operation, Phase, TransitionJournalStore, TransitionRecord},
};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::{
        ActiveReblitCommitCleanupAdmission, ActiveReblitCommitCleanupApplyReconciliation,
        ActiveReblitCommitCleanupAuthority, ActiveReblitCommitCleanupAuthorityError,
        ActiveReblitCommitCleanupPendingDurabilityAuthority,
    },
    startup_recovery::{
        ActiveReblitCommitCleanupPersistenceError,
        persist_active_reblit_commit_cleanup_complete_and_reopen,
    },
};

pub(super) enum Dispatch {
    Unhandled {
        journal: TransitionJournalStore,
        record: TransitionRecord,
    },
    Handled {
        journal: TransitionJournalStore,
        record: TransitionRecord,
    },
}

/// Dispatch at most the exact cleanup checkpoint present at startup entry.
pub(super) fn dispatch<'reservation>(
    installation: &Installation,
    state_db: &db::state::Database,
    active_state_reservation: &'reservation ActiveStateReservation,
    journal: TransitionJournalStore,
    record: TransitionRecord,
) -> Result<Dispatch, Error> {
    if record.operation != Operation::ActiveReblit || record.phase != Phase::CommitDecided {
        return Ok(Dispatch::Unhandled { journal, record });
    }

    let admission = ActiveReblitCommitCleanupAuthority::capture(
        installation,
        &journal,
        state_db,
        active_state_reservation,
        &record,
    )?;
    let pending = match admission {
        ActiveReblitCommitCleanupAdmission::NotApplicable => {
            return Err(Error::ExactCheckpointRejectedAsNotApplicable);
        }
        ActiveReblitCommitCleanupAdmission::Deferred => {
            return Ok(Dispatch::Handled { journal, record });
        }
        ActiveReblitCommitCleanupAdmission::Apply(authority) => {
            let effect = match authority.into_effect_authority(&journal) {
                Ok(effect) => effect,
                Err(_) => return Ok(Dispatch::Handled { journal, record }),
            };
            match effect.reconcile(&journal) {
                Ok(ActiveReblitCommitCleanupApplyReconciliation::Applied(pending)) => pending,
                Ok(
                    ActiveReblitCommitCleanupApplyReconciliation::NotApplied
                    | ActiveReblitCommitCleanupApplyReconciliation::Ambiguous,
                )
                | Err(_) => return Ok(Dispatch::Handled { journal, record }),
            }
        }
        ActiveReblitCommitCleanupAdmission::Finish(authority) => {
            let effect = match authority.into_effect_authority(&journal) {
                Ok(effect) => effect,
                Err(_) => return Ok(Dispatch::Handled { journal, record }),
            };
            match effect.into_durability(&journal) {
                Ok(pending) => pending,
                Err(_) => return Ok(Dispatch::Handled { journal, record }),
            }
        }
    };
    complete_and_persist(journal, record, pending)
}

fn complete_and_persist(
    journal: TransitionJournalStore,
    record: TransitionRecord,
    pending: ActiveReblitCommitCleanupPendingDurabilityAuthority<'_>,
) -> Result<Dispatch, Error> {
    let durable = match pending.complete(&journal) {
        Ok(durable) => durable,
        Err(_) => return Ok(Dispatch::Handled { journal, record }),
    };
    let (journal, record) =
        persist_active_reblit_commit_cleanup_complete_and_reopen(journal, durable)?;
    Ok(Dispatch::Handled { journal, record })
}

#[derive(Debug, Error)]
pub(in crate::client) enum Error {
    #[error("capture exact forward ActiveReblit CommitDecided cleanup authority")]
    Authority(#[from] ActiveReblitCommitCleanupAuthorityError),
    #[error("exact ActiveReblit CommitDecided cleanup record was rejected as not applicable")]
    ExactCheckpointRejectedAsNotApplicable,
    #[error("persist exact ActiveReblit CommitCleanupComplete cleanup successor")]
    Persistence(#[from] ActiveReblitCommitCleanupPersistenceError),
}
