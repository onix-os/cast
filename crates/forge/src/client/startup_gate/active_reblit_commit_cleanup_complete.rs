//! One-entry production dispatch for exact completed ActiveReblit cleanup.
//!
//! `CommitCleanupComplete` admission authenticates the installed promoted
//! receipt without mutating it, then persists one exact `Complete` successor.
//! Every handled source or successor returns immediately.

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
    let ready = match admission {
        ActiveReblitCommitCleanupCompleteAdmission::NotApplicable => {
            return Err(Error::ExactCheckpointRejectedAsNotApplicable);
        }
        ActiveReblitCommitCleanupCompleteAdmission::Deferred => {
            return Ok(Dispatch::Handled { journal, record });
        }
        ActiveReblitCommitCleanupCompleteAdmission::Ready(authority) => authority,
    };
    persist_complete(journal, ready)
}

fn persist_complete(
    journal: TransitionJournalStore,
    authority: ActiveReblitCommitCleanupCompleteAuthority<'_>,
) -> Result<Dispatch, Error> {
    let (journal, record) =
        persist_active_reblit_commit_cleanup_complete_to_complete_and_reopen(journal, authority)?;
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
