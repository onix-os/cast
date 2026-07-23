//! One-entry production dispatch of the completed NewState rollback suffix.
//!
//! This orchestrator handles at most the checkpoint observed at startup entry.
//! A successful leaf returns immediately, whether it advances the phase or
//! safely retains it; its resulting record is never redispatched in the same
//! startup entry. Everything outside the exact NewState suffix is returned
//! untouched to diagnostic reconciliation.

use thiserror::Error;

use crate::{
    Installation, db,
    transition_journal::{Operation, Phase, TransitionJournalStore, TransitionRecord},
};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_gate::UsrRollbackCandidatePreserveSeal,
    startup_reconciliation::{
        UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveAuthority,
        UsrRollbackCompleteRouteAdmission, UsrRollbackCompleteRouteAuthority, UsrRollbackFinalizationAdmission,
        UsrRollbackFinalizationAuthority, UsrRollbackFreshDbInvalidationAdmission,
        UsrRollbackFreshDbInvalidationAuthority, UsrRollbackFreshDbInvalidationRouteAdmission,
        UsrRollbackFreshDbInvalidationRouteAuthority,
    },
    startup_recovery::{
        UsrRollbackCandidatePreserveReady, UsrRollbackFreshDbInvalidationReady,
        dispatch_usr_rollback_candidate_preserve_and_reopen, dispatch_usr_rollback_fresh_db_invalidation_and_reopen,
        finalize_usr_rollback, persist_usr_rollback_complete_route_and_reopen,
        persist_usr_rollback_fresh_db_invalidation_route_and_reopen,
    },
};

/// Unforgeable safe-code token limiting the post-preservation journal route
/// to this exact writer-first NewState suffix orchestrator.
pub(in crate::client) struct UsrRollbackFreshDbInvalidationRouteSeal {
    _private: (),
}

impl UsrRollbackFreshDbInvalidationRouteSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable safe-code token limiting fresh-database invalidation authority
/// capture to this exact writer-first NewState suffix orchestrator.
pub(in crate::client) struct UsrRollbackFreshDbInvalidationSeal {
    _private: (),
}

impl UsrRollbackFreshDbInvalidationSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable safe-code token limiting the journal-only route from
/// `FreshDbInvalidated` to rollback completion to this exact orchestrator.
pub(in crate::client) struct UsrRollbackCompleteRouteSeal {
    _private: (),
}

impl UsrRollbackCompleteRouteSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable safe-code token limiting terminal rollback-finalization
/// authority capture to this exact writer-first NewState suffix orchestrator.
pub(in crate::client) struct UsrRollbackFinalizationSeal {
    _private: (),
}

impl UsrRollbackFinalizationSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Whether this entry handled one exact suffix checkpoint.
pub(super) enum Dispatch {
    Unhandled {
        journal: TransitionJournalStore,
        record: TransitionRecord,
    },
    Handled {
        journal: TransitionJournalStore,
        record: TransitionRecord,
    },
    /// The exact terminal record was deleted and the same lock-bearing store
    /// proved public canonical absence. There is deliberately no record which
    /// could fall through into another phase dispatch in this startup entry.
    Finalized { journal: TransitionJournalStore },
}

/// Dispatch at most the one NewState rollback phase present at entry.
pub(super) fn dispatch<'reservation>(
    installation: &Installation,
    state_db: &db::state::Database,
    active_state_reservation: &'reservation ActiveStateReservation,
    journal: TransitionJournalStore,
    record: TransitionRecord,
    initial_in_flight: Option<db::state::InFlightTransition>,
) -> Result<Dispatch, Error> {
    if record.operation != Operation::NewState {
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
            let seal = UsrRollbackFreshDbInvalidationRouteSeal::new();
            let admission = UsrRollbackFreshDbInvalidationRouteAuthority::capture(
                &seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
                initial_in_flight,
            )?;
            let UsrRollbackFreshDbInvalidationRouteAdmission::Ready(authority) = admission else {
                return Ok(Dispatch::Unhandled { journal, record });
            };
            let (journal, record) = persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority)?;
            Ok(Dispatch::Handled { journal, record })
        }
        Phase::FreshDbInvalidationIntent => {
            let seal = UsrRollbackFreshDbInvalidationSeal::new();
            let admission = UsrRollbackFreshDbInvalidationAuthority::capture(
                &seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
            )?;
            let ready = match admission {
                UsrRollbackFreshDbInvalidationAdmission::Apply(authority) => {
                    UsrRollbackFreshDbInvalidationReady::Apply(authority)
                }
                UsrRollbackFreshDbInvalidationAdmission::Finish(authority) => {
                    UsrRollbackFreshDbInvalidationReady::Finish(authority)
                }
                UsrRollbackFreshDbInvalidationAdmission::NotApplicable
                | UsrRollbackFreshDbInvalidationAdmission::Deferred => {
                    return Ok(Dispatch::Unhandled { journal, record });
                }
            };
            let (journal, record) = dispatch_usr_rollback_fresh_db_invalidation_and_reopen(journal, ready)?;
            Ok(Dispatch::Handled { journal, record })
        }
        Phase::FreshDbInvalidated => {
            let seal = UsrRollbackCompleteRouteSeal::new();
            let admission = UsrRollbackCompleteRouteAuthority::capture(
                &seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
            )?;
            let UsrRollbackCompleteRouteAdmission::Ready(authority) = admission else {
                return Ok(Dispatch::Unhandled { journal, record });
            };
            let (journal, record) = persist_usr_rollback_complete_route_and_reopen(journal, authority)?;
            Ok(Dispatch::Handled { journal, record })
        }
        Phase::RollbackComplete => {
            let seal = UsrRollbackFinalizationSeal::new();
            let admission = UsrRollbackFinalizationAuthority::capture(
                &seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
            )?;
            let UsrRollbackFinalizationAdmission::Ready(authority) = admission else {
                return Ok(Dispatch::Unhandled { journal, record });
            };
            let journal = finalize_usr_rollback(journal, authority)?;
            Ok(Dispatch::Finalized { journal })
        }
        _ => Ok(Dispatch::Unhandled { journal, record }),
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum Error {
    #[error("capture exact startup NewState candidate-preservation authority")]
    CandidatePreserveAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackCandidatePreserveAuthorityError,
    ),
    #[error("dispatch exact startup NewState candidate preservation")]
    CandidatePreserveDispatch(#[from] crate::client::startup_recovery::UsrRollbackCandidatePreserveDispatchError),
    #[error("capture exact startup NewState fresh-database invalidation route authority")]
    FreshDbInvalidationRouteAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackFreshDbInvalidationRouteAuthorityError,
    ),
    #[error("persist exact startup NewState fresh-database invalidation route")]
    FreshDbInvalidationRoutePersistence(
        #[from] crate::client::startup_recovery::UsrRollbackFreshDbInvalidationRoutePersistenceError,
    ),
    #[error("capture exact startup NewState fresh-database invalidation authority")]
    FreshDbInvalidationAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackFreshDbInvalidationAuthorityError,
    ),
    #[error("dispatch exact startup NewState fresh-database invalidation")]
    FreshDbInvalidationDispatch(#[from] crate::client::startup_recovery::UsrRollbackFreshDbInvalidationDispatchError),
    #[error("capture exact startup NewState rollback-completion route authority")]
    RollbackCompleteRouteAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackCompleteRouteAuthorityError,
    ),
    #[error("persist exact startup NewState rollback-completion route")]
    RollbackCompleteRoutePersistence(#[from] crate::client::startup_recovery::UsrRollbackCompleteRoutePersistenceError),
    #[error("capture exact startup NewState terminal rollback-finalization authority")]
    RollbackFinalizationAuthority(#[from] crate::client::startup_reconciliation::UsrRollbackFinalizationAuthorityError),
    #[error("finalize exact startup NewState terminal rollback journal")]
    RollbackFinalization(#[from] crate::client::startup_recovery::UsrRollbackFinalizationError),
}

#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider candidate-authority helpers
#[path = "../startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod candidate_test_support;
#[cfg(test)]
#[allow(dead_code, unused_imports)] // shared fixture contains wider invalidation-authority helpers
#[path = "../startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/tests/support.rs"]
mod invalidation_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider recovery construction helpers
#[path = "../startup_recovery/test_support.rs"]
mod test_fixture;
#[cfg(test)]
mod tests;
