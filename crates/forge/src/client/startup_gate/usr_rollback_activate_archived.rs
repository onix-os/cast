//! One-entry production dispatch for ActivateArchived candidate preservation.
//!
//! Only exact `CandidatePreserveIntent` evidence can enter the consuming leaf.
//! A handled source returns its reopened record immediately; the resulting
//! `CandidatePreserved` successor is never routed to rollback completion in
//! the same startup entry. Every other phase is returned unchanged.

use thiserror::Error;

use crate::{
    Installation, db,
    transition_journal::{Operation, Phase, TransitionJournalStore, TransitionRecord},
};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_gate::UsrRollbackCandidatePreserveSeal,
    startup_reconciliation::{UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveAuthority},
    startup_recovery::{UsrRollbackCandidatePreserveReady, dispatch_usr_rollback_candidate_preserve_and_reopen},
};

/// Unforgeable safe-code token limiting future ActivateArchived completion
/// routing to its operation-specific writer-first startup child.
pub(in crate::client) struct UsrRollbackActivateArchivedCompleteRouteSeal {
    _private: (),
}

impl UsrRollbackActivateArchivedCompleteRouteSeal {
    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self { _private: () }
    }
}

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

/// Dispatch at most the ActivateArchived candidate-preservation checkpoint
/// present at startup entry.
pub(super) fn dispatch<'reservation>(
    installation: &Installation,
    state_db: &db::state::Database,
    active_state_reservation: &'reservation ActiveStateReservation,
    journal: TransitionJournalStore,
    record: TransitionRecord,
    initial_in_flight: Option<db::state::InFlightTransition>,
) -> Result<Dispatch, Error> {
    if record.operation != Operation::ActivateArchived || record.phase != Phase::CandidatePreserveIntent {
        return Ok(Dispatch::Unhandled { journal, record });
    }

    let seal = UsrRollbackCandidatePreserveSeal::new();
    let admission = UsrRollbackCandidatePreserveAuthority::capture(
        &seal,
        installation,
        &journal,
        state_db,
        active_state_reservation,
        &record,
        initial_in_flight,
    )?;
    let ready = match admission {
        UsrRollbackCandidatePreserveAdmission::Apply(authority) => UsrRollbackCandidatePreserveReady::Apply(authority),
        UsrRollbackCandidatePreserveAdmission::Finish(authority) => {
            UsrRollbackCandidatePreserveReady::Finish(authority)
        }
        UsrRollbackCandidatePreserveAdmission::NotApplicable | UsrRollbackCandidatePreserveAdmission::Deferred => {
            return Ok(Dispatch::Unhandled { journal, record });
        }
    };
    let (journal, record) = dispatch_usr_rollback_candidate_preserve_and_reopen(journal, record, ready)?;
    Ok(Dispatch::Handled { journal, record })
}

#[derive(Debug, Error)]
pub(in crate::client) enum Error {
    #[error("capture exact startup ActivateArchived candidate-preservation authority")]
    CandidatePreserveAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackCandidatePreserveAuthorityError,
    ),
    #[error("dispatch exact startup ActivateArchived candidate preservation")]
    CandidatePreserveDispatch(#[from] crate::client::startup_recovery::UsrRollbackCandidatePreserveDispatchError),
}

#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider candidate-authority helpers
#[path = "../startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod candidate_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider recovery construction helpers
#[path = "../startup_recovery/test_support.rs"]
mod test_fixture;
#[cfg(test)]
mod tests;
