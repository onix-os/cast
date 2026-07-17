//! Persist the journal-only ActiveReblit route from `CandidatePreserved` to
//! `RollbackComplete`.
//!
//! The supplied authority retains exact cleared existing-state provenance,
//! preserved whole-wrapper namespace, journal, plan, installation, and
//! active-state-reservation evidence. This boundary revalidates that authority
//! twice, derives the sole `RollbackComplete` successor, performs exactly one
//! conditional journal advance, and drops both the authority and old store
//! before canonical reopen. It performs no database, namespace, trigger,
//! cleanup, retry, finalizer, or journal-delete effect.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{CodecError, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{
    UsrRollbackActiveReblitCompleteRouteAuthority, UsrRollbackActiveReblitCompleteRouteAuthorityError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackActiveReblitCompleteRouteRecord {
    CandidatePreserved,
    RollbackComplete,
}

/// Persist the sole ActiveReblit rollback-completion successor, then
/// independently reopen and compare the complete canonical record.
pub(in crate::client) fn persist_usr_rollback_active_reblit_complete_route_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackActiveReblitCompleteRouteAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackActiveReblitCompleteRoutePersistenceError> {
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::Authority(source));
    }
    let source_record = authority.record().clone();
    let successor = match source_record.rollback_successor(None) {
        Ok(successor) if successor.phase == Phase::RollbackComplete => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(
                UsrRollbackActiveReblitCompleteRoutePersistenceError::UnexpectedSuccessor { phase: successor.phase },
            );
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::RouteConstruction { source });
        }
    };

    before_usr_rollback_active_reblit_complete_route_final_revalidation();
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::Authority(source));
    }
    let installation = authority.installation().clone();
    let advance = journal.advance(&source_record, &successor);

    // Canonical reopen begins only after the source-bound authority and old
    // lock-bearing store are destroyed. Neither can authorize another route.
    drop(authority);
    drop(journal);

    let reopened =
        reopen_canonical_journal(&installation).map_err(UsrRollbackActiveReblitCompleteRouteReopenError::from);
    match advance {
        Ok(()) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitCompleteRoutePersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => {
                Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::ReopenAfterSuccessfulAdvance { source })
            }
        },
        Err(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::CandidatePreserved,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::AdvanceAndReopen {
                advance: advance_error,
                reopen,
            }),
        },
    }
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackActiveReblitCompleteRouteReopenError {
    UsrRollbackActiveReblitCompleteRouteReopenError::UnexpectedRecord {
        expected_candidate_preserved: Box::new(source.clone()),
        expected_rollback_complete: Box::new(successor.clone()),
        actual: actual.map(Box::new),
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_AUTHORITY_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_active_reblit_complete_route_final_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_active_reblit_complete_route_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_active_reblit_complete_route_final_revalidation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitCompleteRoutePersistenceError {
    #[error("revalidate exact ActiveReblit CandidatePreserved rollback-completion authority")]
    Authority(#[source] UsrRollbackActiveReblitCompleteRouteAuthorityError),
    #[error("derive the sole legal ActiveReblit RollbackComplete successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("ActiveReblit rollback-completion routing selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("ActiveReblit rollback-completion journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackActiveReblitCompleteRouteRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its ActiveReblit RollbackComplete advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackActiveReblitCompleteRouteReopenError,
    },
    #[error(
        "ActiveReblit rollback-completion journal advance failed ({advance}) and its canonical record could not be reconciled"
    )]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackActiveReblitCompleteRouteReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitCompleteRouteReopenError {
    #[error("revalidate retained installation around ActiveReblit rollback-completion journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical ActiveReblit rollback-completion journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact ActiveReblit CandidatePreserved nor RollbackComplete record (candidate_preserved={expected_candidate_preserved:?}, rollback_complete={expected_rollback_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_candidate_preserved: Box<TransitionRecord>,
        expected_rollback_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackActiveReblitCompleteRouteReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
