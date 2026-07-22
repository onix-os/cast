//! One-entry production recovery admission for ActiveReblit receipt promotion
//! at `BootSyncStarted`.
//!
//! Exact pending and legacy records remain available to conservative rollback.
//! Exact promoted evidence is captured and revalidated without mutation, then
//! deliberately discarded so it cannot fall through to rollback before the
//! later cleanup executor is wired.

use crate::{
    Installation, db,
    transition_journal::{CodecError, Operation, Phase, TransitionJournalStore, TransitionRecord},
};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_gate::ActiveReblitBootSyncStartedCleanupSeal,
    startup_reconciliation::{
        ActiveReblitBootSyncStartedRecoveryAdmission,
        ActiveReblitBootSyncStartedRecoveryAuthority,
        ActiveReblitBootSyncStartedRecoveryAuthorityError,
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

pub(super) fn dispatch<'reservation>(
    installation: &Installation,
    state_db: &db::state::Database,
    active_state_reservation: &'reservation ActiveStateReservation,
    journal: TransitionJournalStore,
    record: TransitionRecord,
) -> Result<Dispatch, Error> {
    if record.operation != Operation::ActiveReblit
        || record.phase != Phase::BootSyncStarted
    {
        return Ok(Dispatch::Unhandled { journal, record });
    }
    let Some(receipt_pair) = record
        .boot_publication_receipt_correlation()
        .map_err(Error::Record)?
    else {
        return Ok(Dispatch::Unhandled { journal, record });
    };
    let cleanup_seal =
        ActiveReblitBootSyncStartedCleanupSeal::new(receipt_pair.pending);
    match ActiveReblitBootSyncStartedRecoveryAuthority::capture(
        cleanup_seal,
        installation,
        &journal,
        state_db,
        active_state_reservation,
        &record,
    )? {
        ActiveReblitBootSyncStartedRecoveryAdmission::NotApplicable => {
            Err(Error::ExactCheckpointRejectedAsNotApplicable)
        }
        ActiveReblitBootSyncStartedRecoveryAdmission::RollbackEligible => {
            Ok(Dispatch::Unhandled { journal, record })
        }
        ActiveReblitBootSyncStartedRecoveryAdmission::Deferred => {
            Ok(Dispatch::Handled { journal, record })
        }
        ActiveReblitBootSyncStartedRecoveryAdmission::Ready(authority) => {
            let cleanup_plan = authority.cleanup_plan(&journal)?;
            drop(cleanup_plan);
            drop(authority);
            Ok(Dispatch::Handled { journal, record })
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum Error {
    #[error("decode the exact ActiveReblit BootSyncStarted receipt pair")]
    Record(#[source] CodecError),
    #[error("capture exact promoted ActiveReblit BootSyncStarted recovery authority")]
    Authority(#[from] ActiveReblitBootSyncStartedRecoveryAuthorityError),
    #[error("exact ActiveReblit BootSyncStarted record was rejected as not applicable")]
    ExactCheckpointRejectedAsNotApplicable,
}
