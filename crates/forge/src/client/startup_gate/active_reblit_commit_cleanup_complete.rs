//! One-entry production dispatch for exact completed ActiveReblit cleanup.
//!
//! `CommitCleanupComplete` admission may retire the one exact promoted
//! receipt head, or resume after that retirement is already durable, and
//! persist one exact `Complete` successor. Every handled source or successor
//! returns immediately.

use thiserror::Error;

use crate::{
    Installation, db,
    transition_journal::{Operation, Phase, TransitionJournalStore, TransitionRecord},
};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_gate::ActiveReblitCommitCleanupCompleteSeal,
    startup_reconciliation::{
        ActiveReblitCommitCleanupCompleteAdmission,
        ActiveReblitCommitCleanupCompleteAuthority,
        ActiveReblitCommitCleanupCompleteAuthorityError,
        ActiveReblitCommitCleanupCompleteRetiredAuthority,
    },
    startup_recovery::{
        ActiveReblitCommitCleanupCompletePersistenceError,
        persist_active_reblit_commit_cleanup_complete_to_complete_and_reopen,
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

/// Dispatch at most the exact completed-cleanup checkpoint present at entry.
pub(super) fn dispatch<'reservation>(
    installation: &Installation,
    state_db: &db::state::Database,
    active_state_reservation: &'reservation ActiveStateReservation,
    journal: TransitionJournalStore,
    record: TransitionRecord,
) -> Result<Dispatch, Error> {
    if record.operation != Operation::ActiveReblit
        || record.phase != Phase::CommitCleanupComplete
    {
        return Ok(Dispatch::Unhandled { journal, record });
    }

    let seal = ActiveReblitCommitCleanupCompleteSeal::new();
    let admission = ActiveReblitCommitCleanupCompleteAuthority::capture(
        &seal,
        installation,
        &journal,
        state_db,
        active_state_reservation,
        &record,
    )?;
    let retired = match admission {
        ActiveReblitCommitCleanupCompleteAdmission::NotApplicable => {
            return Err(Error::ExactCheckpointRejectedAsNotApplicable);
        }
        ActiveReblitCommitCleanupCompleteAdmission::Deferred => {
            return Ok(Dispatch::Handled { journal, record });
        }
        ActiveReblitCommitCleanupCompleteAdmission::Apply(authority) => {
            match authority.retire(&journal) {
                Ok(retired) => retired,
                Err(_) => return Ok(Dispatch::Handled { journal, record }),
            }
        }
        ActiveReblitCommitCleanupCompleteAdmission::Finish(authority) => {
            match authority.into_retired(&journal) {
                Ok(retired) => retired,
                Err(_) => return Ok(Dispatch::Handled { journal, record }),
            }
        }
    };
    persist_complete(journal, retired)
}

fn persist_complete(
    journal: TransitionJournalStore,
    retired: ActiveReblitCommitCleanupCompleteRetiredAuthority<'_>,
) -> Result<Dispatch, Error> {
    let (journal, record) =
        persist_active_reblit_commit_cleanup_complete_to_complete_and_reopen(journal, retired)?;
    Ok(Dispatch::Handled { journal, record })
}

#[derive(Debug, Error)]
pub(in crate::client) enum Error {
    #[error("capture exact forward ActiveReblit CommitCleanupComplete authority")]
    Authority(#[from] ActiveReblitCommitCleanupCompleteAuthorityError),
    #[error("exact ActiveReblit CommitCleanupComplete record was rejected as not applicable")]
    ExactCheckpointRejectedAsNotApplicable,
    #[error("persist exact ActiveReblit Complete successor")]
    Persistence(#[from] ActiveReblitCommitCleanupCompletePersistenceError),
}
