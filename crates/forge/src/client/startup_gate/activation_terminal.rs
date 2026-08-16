//! One-entry startup dispatch of the activation terminal tail.
//!
//! A NewState or ActivateArchived transition that crashes after its commit
//! decision leaves a live journal record with no route: the ActiveReblit arms
//! beside this one are built around a staging-wrapper exchange these operations
//! never perform, so they decline it and startup reports `RecoveryPending`
//! forever.
//!
//! This drives the same terminal route the forward path uses, which resumes
//! from whichever step the record actually reached and ends with the record
//! deleted.

use thiserror::Error;

use crate::{
    Installation, db,
    transition_journal::{Operation, Phase, TransitionJournalStore, TransitionRecord},
};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_recovery::{ActivationCommitCleanupPersistenceError, finish_activation_after_commit},
};

/// Whether this entry finished the transition.
pub(super) enum Dispatch {
    Unhandled {
        journal: TransitionJournalStore,
        record: TransitionRecord,
    },
    /// The terminal record was deleted; there is deliberately no record left
    /// that could fall through into another dispatch in this startup entry.
    Finalized { journal: TransitionJournalStore },
}

pub(super) fn dispatch(
    installation: &Installation,
    state_db: &db::state::Database,
    active_state_reservation: &ActiveStateReservation,
    journal: TransitionJournalStore,
    record: TransitionRecord,
) -> Result<Dispatch, Error> {
    if !matches!(record.operation, Operation::NewState | Operation::ActivateArchived)
        || !matches!(
            record.phase,
            Phase::CommitDecided | Phase::CommitCleanupComplete | Phase::Complete
        )
        || record.rollback.is_some()
    {
        return Ok(Dispatch::Unhandled { journal, record });
    }

    let journal = finish_activation_after_commit(journal, state_db, installation, record, active_state_reservation)?;
    Ok(Dispatch::Finalized { journal })
}

#[derive(Debug, Error)]
pub(in crate::client) enum Error {
    #[error("finish the committed activation transition from its recovered phase")]
    Terminal(#[from] ActivationCommitCleanupPersistenceError),
}
