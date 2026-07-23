//! One-entry production dispatch for exact forward ActiveReblit finalization.
//!
//! Every exact `Complete` checkpoint is owned here: incompatible evidence is
//! retained as pending, while exact evidence may consume only one bound
//! terminal deletion before handing the same locked store to clean admission.

use thiserror::Error;

use crate::{
    Installation, db,
    transition_journal::{Operation, Phase, TransitionJournalStore, TransitionRecord},
};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_gate::ActiveReblitCompleteFinalizationSeal,
    startup_reconciliation::{
        ActiveReblitCompleteFinalizationAdmission,
        ActiveReblitCompleteFinalizationAuthority,
        ActiveReblitCompleteFinalizationAuthorityError,
    },
    startup_recovery::{
        ActiveReblitCompleteFinalizationError,
        finalize_active_reblit_complete,
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
    Finalized {
        journal: TransitionJournalStore,
    },
}

pub(super) fn dispatch<'reservation>(
    installation: &Installation,
    state_db: &db::state::Database,
    active_state_reservation: &'reservation ActiveStateReservation,
    journal: TransitionJournalStore,
    record: TransitionRecord,
) -> Result<Dispatch, Error> {
    if record.operation != Operation::ActiveReblit || record.phase != Phase::Complete {
        return Ok(Dispatch::Unhandled { journal, record });
    }

    let seal = ActiveReblitCompleteFinalizationSeal::new();
    match ActiveReblitCompleteFinalizationAuthority::capture(
        &seal,
        installation,
        &journal,
        state_db,
        active_state_reservation,
        &record,
    )? {
        ActiveReblitCompleteFinalizationAdmission::NotApplicable => {
            Err(Error::ExactCheckpointRejectedAsNotApplicable)
        }
        ActiveReblitCompleteFinalizationAdmission::Deferred => {
            Ok(Dispatch::Handled { journal, record })
        }
        ActiveReblitCompleteFinalizationAdmission::Ready(authority) => {
            let journal = finalize_active_reblit_complete(journal, authority)?;
            Ok(Dispatch::Finalized { journal })
        }
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum Error {
    #[error("capture exact forward ActiveReblit Complete finalization authority")]
    Authority(#[from] ActiveReblitCompleteFinalizationAuthorityError),
    #[error("exact forward ActiveReblit Complete record was rejected as not applicable")]
    ExactCheckpointRejectedAsNotApplicable,
    #[error("finalize exact forward ActiveReblit Complete journal")]
    Finalization(#[from] ActiveReblitCompleteFinalizationError),
}
