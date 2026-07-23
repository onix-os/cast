//! One-entry production dispatch for exact forward ActiveReblit boot
//! completion.
//!
//! Only an ActiveReblit `BootSyncComplete` record can enter this child. A
//! stable admission mismatch remains pending without falling through to any
//! rollback route. Exact authority permits one bound journal advance to
//! `CommitDecided`; the reopened successor returns immediately and is never
//! redispatched during the same startup entry.

use thiserror::Error;

use crate::{
    Installation, db,
    transition_journal::{Operation, Phase, TransitionJournalStore, TransitionRecord},
};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_gate::ActiveReblitBootSyncCompleteSeal,
    startup_reconciliation::{
        ActiveReblitBootSyncCompleteAdmission, ActiveReblitBootSyncCompleteAuthority,
        ActiveReblitBootSyncCompleteAuthorityError,
    },
    startup_recovery::{
        ActiveReblitBootSyncCommitDecisionPersistenceError,
        persist_active_reblit_boot_sync_commit_decision_and_reopen,
    },
};

/// Whether this startup entry handled the exact forward boot-completion
/// checkpoint. `Handled` includes deliberately deferred evidence so the
/// checkpoint can never fall through to rollback dispatch.
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

/// Dispatch at most the one exact ActiveReblit `BootSyncComplete` checkpoint
/// present at startup entry.
pub(super) fn dispatch<'reservation>(
    installation: &Installation,
    state_db: &db::state::Database,
    active_state_reservation: &'reservation ActiveStateReservation,
    journal: TransitionJournalStore,
    record: TransitionRecord,
) -> Result<Dispatch, Error> {
    if record.operation != Operation::ActiveReblit || record.phase != Phase::BootSyncComplete {
        return Ok(Dispatch::Unhandled { journal, record });
    }

    let seal = ActiveReblitBootSyncCompleteSeal::new();
    let admission = ActiveReblitBootSyncCompleteAuthority::capture(
        &seal,
        installation,
        &journal,
        state_db,
        active_state_reservation,
        &record,
    )?;
    match admission {
        ActiveReblitBootSyncCompleteAdmission::NotApplicable => {
            Err(Error::ExactCheckpointRejectedAsNotApplicable)
        }
        ActiveReblitBootSyncCompleteAdmission::Deferred => Ok(Dispatch::Handled { journal, record }),
        ActiveReblitBootSyncCompleteAdmission::Ready(authority) => {
            let (journal, record) =
                persist_active_reblit_boot_sync_commit_decision_and_reopen(journal, authority)?;
            Ok(Dispatch::Handled { journal, record })
        }
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum Error {
    #[error("capture exact forward ActiveReblit BootSyncComplete startup authority")]
    Authority(#[from] ActiveReblitBootSyncCompleteAuthorityError),
    #[error("exact ActiveReblit BootSyncComplete record was rejected as not applicable")]
    ExactCheckpointRejectedAsNotApplicable,
    #[error("persist the exact ActiveReblit BootSyncComplete to CommitDecided startup edge")]
    Persistence(#[from] ActiveReblitBootSyncCommitDecisionPersistenceError),
}
