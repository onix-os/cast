//! One-entry production dispatch for the ActiveReblit rollback suffix.
//!
//! Only the exact ActiveReblit checkpoint observed at startup entry can enter
//! its consuming leaf. A successful candidate preservation or journal-only
//! completion route returns immediately; the successor is never redispatched
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
    startup_reconciliation::{
        UsrRollbackActiveReblitBootRepairRequiredAdmission, UsrRollbackActiveReblitBootRepairRequiredAuthority,
        UsrRollbackActiveReblitCompleteRouteAdmission, UsrRollbackActiveReblitCompleteRouteAuthority,
        UsrRollbackActiveReblitFinalizationAdmission, UsrRollbackActiveReblitFinalizationAuthority,
        UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveAuthority,
    },
    startup_recovery::{
        UsrRollbackCandidatePreserveReady, dispatch_usr_rollback_candidate_preserve_and_reopen,
        finalize_usr_rollback_active_reblit, persist_usr_rollback_active_reblit_boot_repair_required_and_reopen,
        persist_usr_rollback_active_reblit_complete_route_and_reopen,
    },
};

/// Unforgeable safe-code token limiting the ActiveReblit boot-repair-required
/// route to this operation-specific writer-first startup child.
pub(in crate::client) struct UsrRollbackActiveReblitBootRepairRequiredSeal {
    _private: (),
}

impl UsrRollbackActiveReblitBootRepairRequiredSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable safe-code token limiting the ActiveReblit completion route to
/// this operation-specific writer-first startup child.
pub(in crate::client) struct UsrRollbackActiveReblitCompleteRouteSeal {
    _private: (),
}

impl UsrRollbackActiveReblitCompleteRouteSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable safe-code token limiting ActiveReblit terminal journal
/// finalization to this operation-specific writer-first startup child.
pub(in crate::client) struct UsrRollbackActiveReblitFinalizationSeal {
    _private: (),
}

impl UsrRollbackActiveReblitFinalizationSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

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
    /// The exact terminal record was deleted and the same lock-bearing store
    /// proved public canonical absence. No record remains which could be
    /// redispatched by this startup entry.
    Finalized { journal: TransitionJournalStore },
}

/// Dispatch at most the one ActiveReblit rollback checkpoint present at entry.
pub(super) fn dispatch<'reservation>(
    installation: &Installation,
    state_db: &db::state::Database,
    active_state_reservation: &'reservation ActiveStateReservation,
    journal: TransitionJournalStore,
    record: TransitionRecord,
    initial_in_flight: Option<db::state::InFlightTransition>,
) -> Result<Dispatch, Error> {
    if record.operation != Operation::ActiveReblit {
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
            let boot_repair_seal = UsrRollbackActiveReblitBootRepairRequiredSeal::new();
            let boot_repair = UsrRollbackActiveReblitBootRepairRequiredAuthority::capture(
                &boot_repair_seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
            )?;
            if let UsrRollbackActiveReblitBootRepairRequiredAdmission::Ready(authority) = boot_repair {
                let (journal, record) =
                    persist_usr_rollback_active_reblit_boot_repair_required_and_reopen(journal, authority)?;
                return Ok(Dispatch::Handled { journal, record });
            }

            let seal = UsrRollbackActiveReblitCompleteRouteSeal::new();
            let admission = UsrRollbackActiveReblitCompleteRouteAuthority::capture(
                &seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
            )?;
            let UsrRollbackActiveReblitCompleteRouteAdmission::Ready(authority) = admission else {
                return Ok(Dispatch::Unhandled { journal, record });
            };
            let (journal, record) = persist_usr_rollback_active_reblit_complete_route_and_reopen(journal, authority)?;
            Ok(Dispatch::Handled { journal, record })
        }
        Phase::RollbackComplete => {
            let seal = UsrRollbackActiveReblitFinalizationSeal::new();
            let admission = UsrRollbackActiveReblitFinalizationAuthority::capture(
                &seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
            )?;
            let UsrRollbackActiveReblitFinalizationAdmission::Ready(authority) = admission else {
                return Ok(Dispatch::Unhandled { journal, record });
            };
            let journal = finalize_usr_rollback_active_reblit(journal, authority)?;
            Ok(Dispatch::Finalized { journal })
        }
        _ => Ok(Dispatch::Unhandled { journal, record }),
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum Error {
    #[error("capture exact startup ActiveReblit candidate-preservation authority")]
    CandidatePreserveAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackCandidatePreserveAuthorityError,
    ),
    #[error("dispatch exact startup ActiveReblit candidate preservation")]
    CandidatePreserveDispatch(#[from] crate::client::startup_recovery::UsrRollbackCandidatePreserveDispatchError),
    #[error("capture exact startup ActiveReblit rollback-completion route authority")]
    CompleteRouteAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackActiveReblitCompleteRouteAuthorityError,
    ),
    #[error("capture exact startup ActiveReblit boot-repair-required route authority")]
    BootRepairRequiredAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackActiveReblitBootRepairRequiredAuthorityError,
    ),
    #[error("persist exact startup ActiveReblit boot-repair-required route")]
    BootRepairRequiredPersistence(
        #[from] crate::client::startup_recovery::UsrRollbackActiveReblitBootRepairRequiredPersistenceError,
    ),
    #[error("persist exact startup ActiveReblit rollback-completion route")]
    CompleteRoutePersistence(
        #[from] crate::client::startup_recovery::UsrRollbackActiveReblitCompleteRoutePersistenceError,
    ),
    #[error("capture exact startup ActiveReblit terminal rollback-finalization authority")]
    RollbackFinalizationAuthority(
        #[from] crate::client::startup_reconciliation::UsrRollbackActiveReblitFinalizationAuthorityError,
    ),
    #[error("finalize exact startup ActiveReblit terminal rollback journal")]
    RollbackFinalization(#[from] crate::client::startup_recovery::UsrRollbackActiveReblitFinalizationError),
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
