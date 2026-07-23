//! One-entry production dispatch for the ActivateArchived rollback suffix.
//!
//! Only the exact ActivateArchived checkpoint observed at startup entry can
//! enter its consuming leaf. A successful candidate preservation or
//! journal-only completion route returns immediately; the successor is never
//! redispatched by this module in the same startup entry. In particular,
//! `RollbackComplete` is consumed only when it was already present at entry;
//! completion never falls through to finalization. Every other operation or
//! phase is returned unchanged to the remaining startup gate.

use thiserror::Error;

use crate::{
    Installation, db,
    transition_journal::{Operation, Phase, TransitionJournalStore, TransitionRecord},
};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_gate::UsrRollbackCandidatePreserveSeal,
    startup_reconciliation::{
        UsrRollbackActivateArchivedCompleteRouteAdmission, UsrRollbackActivateArchivedCompleteRouteAuthority,
        UsrRollbackActivateArchivedFinalizationAdmission, UsrRollbackActivateArchivedFinalizationAuthority,
        UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveAuthority,
    },
    startup_recovery::{
        UsrRollbackCandidatePreserveReady, dispatch_usr_rollback_candidate_preserve_and_reopen,
        finalize_usr_rollback_activate_archived, persist_usr_rollback_activate_archived_complete_route_and_reopen,
    },
};

/// Unforgeable safe-code token limiting ActivateArchived completion
/// routing to its operation-specific writer-first startup child.
pub(in crate::client) struct UsrRollbackActivateArchivedCompleteRouteSeal {
    _private: (),
}

impl UsrRollbackActivateArchivedCompleteRouteSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable safe-code token limiting ActivateArchived terminal
/// finalization to this operation-specific writer-first startup child.
pub(in crate::client) struct UsrRollbackActivateArchivedFinalizationSeal {
    _private: (),
}

impl UsrRollbackActivateArchivedFinalizationSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
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
    Finalized {
        journal: TransitionJournalStore,
    },
}

/// Dispatch at most the one ActivateArchived rollback checkpoint present at
/// startup entry.
pub(super) fn dispatch<'reservation>(
    installation: &Installation,
    state_db: &db::state::Database,
    active_state_reservation: &'reservation ActiveStateReservation,
    journal: TransitionJournalStore,
    record: TransitionRecord,
    initial_in_flight: Option<db::state::InFlightTransition>,
) -> Result<Dispatch, Error> {
    if record.operation != Operation::ActivateArchived {
        return Ok(Dispatch::Unhandled { journal, record });
    }

    match record.phase {
        Phase::CandidatePreserveIntent => {
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
                UsrRollbackCandidatePreserveAdmission::Apply(authority) => {
                    UsrRollbackCandidatePreserveReady::Apply(authority)
                }
                UsrRollbackCandidatePreserveAdmission::Finish(authority) => {
                    UsrRollbackCandidatePreserveReady::Finish(authority)
                }
                UsrRollbackCandidatePreserveAdmission::NotApplicable
                | UsrRollbackCandidatePreserveAdmission::Deferred => {
                    return Ok(Dispatch::Unhandled { journal, record });
                }
            };
            let (journal, record) = dispatch_usr_rollback_candidate_preserve_and_reopen(journal, record, ready)?;
            Ok(Dispatch::Handled { journal, record })
        }
        Phase::CandidatePreserved => {
            let seal = UsrRollbackActivateArchivedCompleteRouteSeal::new();
            let admission = UsrRollbackActivateArchivedCompleteRouteAuthority::capture(
                &seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
            )?;
            let UsrRollbackActivateArchivedCompleteRouteAdmission::Ready(authority) = admission else {
                return Ok(Dispatch::Unhandled { journal, record });
            };
            let (journal, record) =
                persist_usr_rollback_activate_archived_complete_route_and_reopen(journal, authority)?;
            Ok(Dispatch::Handled { journal, record })
        }
        Phase::RollbackComplete => {
            let seal = UsrRollbackActivateArchivedFinalizationSeal::new();
            let admission = UsrRollbackActivateArchivedFinalizationAuthority::capture(
                &seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
            )?;
            let UsrRollbackActivateArchivedFinalizationAdmission::Ready(authority) = admission else {
                return Ok(Dispatch::Unhandled { journal, record });
            };
            let journal = finalize_usr_rollback_activate_archived(journal, authority)?;
            Ok(Dispatch::Finalized { journal })
        }
        _ => Ok(Dispatch::Unhandled { journal, record }),
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum Error {
    #[error("capture exact startup ActivateArchived candidate-preservation authority")]
    CandidatePreserveAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackCandidatePreserveAuthorityError,
    ),
    #[error("dispatch exact startup ActivateArchived candidate preservation")]
    CandidatePreserveDispatch(#[from] crate::client::startup_recovery::UsrRollbackCandidatePreserveDispatchError),
    #[error("capture exact startup ActivateArchived rollback-completion route authority")]
    CompleteRouteAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackActivateArchivedCompleteRouteAuthorityError,
    ),
    #[error("persist exact startup ActivateArchived rollback-completion route")]
    CompleteRoutePersistence(
        #[from] crate::client::startup_recovery::UsrRollbackActivateArchivedCompleteRoutePersistenceError,
    ),
    #[error("capture exact startup ActivateArchived terminal rollback-finalization authority")]
    RollbackFinalizationAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackActivateArchivedFinalizationAuthorityError,
    ),
    #[error("finalize exact startup ActivateArchived terminal rollback journal")]
    RollbackFinalization(#[from] crate::client::startup_recovery::UsrRollbackActivateArchivedFinalizationError),
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
