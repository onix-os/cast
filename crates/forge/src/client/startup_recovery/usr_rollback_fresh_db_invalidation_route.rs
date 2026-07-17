//! Persist the journal-only route into fresh-database invalidation.
//!
//! The supplied authority retains exact `CandidatePreserved` namespace,
//! database, provenance, journal, plan, and installation evidence. This
//! boundary revalidates that evidence, derives the sole
//! `FreshDbInvalidationIntent` successor, performs exactly one conditional
//! advance, and drops both the authority and old store before canonical
//! reopen. Beyond that journal update, it performs no state-database,
//! provenance, or activation-namespace mutation.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{CodecError, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{
    UsrRollbackFreshDbInvalidationRouteAuthority, UsrRollbackFreshDbInvalidationRouteAuthorityError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

#[cfg(test)]
#[allow(dead_code)] // shared candidate fixture contains wider reconciliation helpers
#[path = "../startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod candidate_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider startup-recovery helpers
#[path = "test_support.rs"]
mod test_fixture;
#[cfg(test)]
mod tests;

/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackFreshDbInvalidationRouteRecord {
    CandidatePreserved,
    FreshDbInvalidationIntent,
}

/// Persist the sole fresh-database invalidation intent, then independently
/// reopen and compare the complete canonical record.
pub(in crate::client) fn persist_usr_rollback_fresh_db_invalidation_route_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackFreshDbInvalidationRouteAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackFreshDbInvalidationRoutePersistenceError> {
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::Authority(source));
    }
    let source_record = authority.record().clone();
    let successor = match source_record.rollback_successor(None) {
        Ok(successor) if successor.phase == Phase::FreshDbInvalidationIntent => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(
                UsrRollbackFreshDbInvalidationRoutePersistenceError::UnexpectedSuccessor { phase: successor.phase },
            );
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::RouteConstruction { source });
        }
    };

    before_usr_rollback_fresh_db_invalidation_route_final_revalidation();
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::Authority(source));
    }
    let installation = authority.installation().clone();
    let advance = journal.advance(&source_record, &successor);

    // Canonical reopen starts only after the authority and old lock-bearing
    // store are destroyed. Neither capability can authorize a second attempt.
    drop(authority);
    drop(journal);

    let reopened =
        reopen_canonical_journal(&installation).map_err(UsrRollbackFreshDbInvalidationRouteReopenError::from);
    match advance {
        Ok(()) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackFreshDbInvalidationRoutePersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => {
                Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::ReopenAfterSuccessfulAdvance { source })
            }
        },
        Err(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::AdvanceAndReopen {
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
) -> UsrRollbackFreshDbInvalidationRouteReopenError {
    UsrRollbackFreshDbInvalidationRouteReopenError::UnexpectedRecord {
        expected_candidate_preserved: Box::new(source.clone()),
        expected_fresh_db_invalidation_intent: Box::new(successor.clone()),
        actual: actual.map(Box::new),
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_AUTHORITY_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_fresh_db_invalidation_route_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_fresh_db_invalidation_route_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_fresh_db_invalidation_route_final_revalidation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackFreshDbInvalidationRoutePersistenceError {
    #[error("revalidate exact CandidatePreserved fresh-database invalidation routing authority")]
    Authority(#[source] UsrRollbackFreshDbInvalidationRouteAuthorityError),
    #[error("derive the sole legal FreshDbInvalidationIntent successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("fresh-database invalidation routing selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackFreshDbInvalidationRouteRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its FreshDbInvalidationIntent advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackFreshDbInvalidationRouteReopenError,
    },
    #[error("journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackFreshDbInvalidationRouteReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackFreshDbInvalidationRouteReopenError {
    #[error("revalidate retained installation around journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact CandidatePreserved nor FreshDbInvalidationIntent record (candidate_preserved={expected_candidate_preserved:?}, fresh_db_invalidation_intent={expected_fresh_db_invalidation_intent:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_candidate_preserved: Box<TransitionRecord>,
        expected_fresh_db_invalidation_intent: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackFreshDbInvalidationRouteReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
