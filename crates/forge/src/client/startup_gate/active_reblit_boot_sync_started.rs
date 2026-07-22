//! One-entry production guard for ActiveReblit receipt promotion at
//! `BootSyncStarted`.
//!
//! Exact pending and legacy records remain available to conservative rollback.
//! An exact promoted receipt is handled without mutation so it cannot fall
//! through to rollback before durable cleanup recovery is implemented.

use crate::{
    db,
    transition_journal::{TransitionJournalStore, TransitionRecord},
};

use crate::client::startup_reconciliation::{
    ActiveReblitBootSyncStartedGuard, ActiveReblitBootSyncStartedGuardAdmission,
    ActiveReblitBootSyncStartedGuardError,
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

pub(super) fn dispatch(
    state_db: &db::state::Database,
    journal: TransitionJournalStore,
    record: TransitionRecord,
) -> Result<Dispatch, Error> {
    match ActiveReblitBootSyncStartedGuard::inspect(state_db, &record)? {
        ActiveReblitBootSyncStartedGuardAdmission::NotApplicable
        | ActiveReblitBootSyncStartedGuardAdmission::RollbackEligible => {
            Ok(Dispatch::Unhandled { journal, record })
        }
        ActiveReblitBootSyncStartedGuardAdmission::Promoted => {
            Ok(Dispatch::Handled { journal, record })
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum Error {
    #[error("classify the ActiveReblit BootSyncStarted receipt-promotion boundary")]
    Guard(#[from] ActiveReblitBootSyncStartedGuardError),
}
