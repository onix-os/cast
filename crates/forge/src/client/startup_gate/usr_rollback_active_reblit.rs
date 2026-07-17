//! One-entry production dispatch for ActiveReblit candidate preservation.
//!
//! Only an exact ActiveReblit `CandidatePreserveIntent` observed at startup
//! entry can enter the consuming leaf. A successful Apply or Finish returns
//! immediately with `CandidatePreserved`; the successor is never redispatched
//! by this module in the same startup entry. Every other operation or phase is
//! returned unchanged to the remaining startup gate.

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

/// Whether this startup entry handled the exact ActiveReblit checkpoint.
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

/// Dispatch only ActiveReblit candidate preservation present at startup entry.
pub(super) fn dispatch<'reservation>(
    installation: &Installation,
    state_db: &db::state::Database,
    active_state_reservation: &'reservation ActiveStateReservation,
    journal: TransitionJournalStore,
    record: TransitionRecord,
    initial_in_flight: Option<db::state::InFlightTransition>,
) -> Result<Dispatch, Error> {
    if record.operation != Operation::ActiveReblit || record.phase != Phase::CandidatePreserveIntent {
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
    #[error("capture exact startup ActiveReblit candidate-preservation authority")]
    CandidatePreserveAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackCandidatePreserveAuthorityError,
    ),
    #[error("dispatch exact startup ActiveReblit candidate preservation")]
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
