//! Persist the journal-only route from `FreshDbInvalidated` to
//! `RollbackComplete`.
//!
//! The supplied authority retains exact jointly-absent database evidence and
//! exact namespace, journal, plan, installation, and active-state reservation
//! evidence. This boundary revalidates that authority twice, derives the sole
//! `RollbackComplete` successor, performs exactly one conditional journal
//! advance, and drops both the authority and old store before canonical
//! reopen. It performs no database, namespace, trigger, cleanup, retry,
//! finalizer, or journal-delete effect.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{CodecError, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{UsrRollbackCompleteRouteAuthority, UsrRollbackCompleteRouteAuthorityError};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

#[cfg(test)]
#[allow(dead_code)] // shared candidate fixture contains wider reconciliation helpers
#[path = "../startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod candidate_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared invalidation fixture contains wider effect helpers
#[path = "../startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/tests/support.rs"]
mod invalidation_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider startup-recovery helpers
#[path = "test_support.rs"]
mod test_fixture;
#[cfg(test)]
mod tests;

/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackCompleteRouteRecord {
    FreshDbInvalidated,
    RollbackComplete,
}

/// Persist the sole rollback-completion successor, then independently reopen
/// and compare the complete canonical record.
pub(in crate::client) fn persist_usr_rollback_complete_route_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackCompleteRouteAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackCompleteRoutePersistenceError> {
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackCompleteRoutePersistenceError::Authority(source));
    }
    let source_record = authority.record().clone();
    let successor = match source_record.rollback_successor(None) {
        Ok(successor) if successor.phase == Phase::RollbackComplete => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackCompleteRoutePersistenceError::UnexpectedSuccessor { phase: successor.phase });
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackCompleteRoutePersistenceError::RouteConstruction { source });
        }
    };

    before_usr_rollback_complete_route_final_revalidation();
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackCompleteRoutePersistenceError::Authority(source));
    }
    let installation = authority.installation().clone();
    let advance = journal.advance(&source_record, &successor);

    // Canonical reopen starts only after the source-bound authority and old
    // lock-bearing store are destroyed. Neither can authorize another route.
    drop(authority);
    drop(journal);

    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackCompleteRouteReopenError::from);
    match advance {
        Ok(()) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackCompleteRoutePersistenceError::ReopenAfterSuccessfulAdvance {
                    source: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(source) => Err(UsrRollbackCompleteRoutePersistenceError::ReopenAfterSuccessfulAdvance { source }),
        },
        Err(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackCompleteRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackCompleteRouteRecord::FreshDbInvalidated,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackCompleteRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackCompleteRouteRecord::RollbackComplete,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackCompleteRoutePersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackCompleteRoutePersistenceError::AdvanceAndReopen {
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
) -> UsrRollbackCompleteRouteReopenError {
    UsrRollbackCompleteRouteReopenError::UnexpectedRecord {
        expected_fresh_db_invalidated: Box::new(source.clone()),
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
pub(crate) fn arm_before_usr_rollback_complete_route_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_complete_route_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_complete_route_final_revalidation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackCompleteRoutePersistenceError {
    #[error("revalidate exact FreshDbInvalidated rollback-completion routing authority")]
    Authority(#[source] UsrRollbackCompleteRouteAuthorityError),
    #[error("derive the sole legal RollbackComplete successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("rollback-completion routing selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackCompleteRouteRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its RollbackComplete advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackCompleteRouteReopenError,
    },
    #[error("journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackCompleteRouteReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackCompleteRouteReopenError {
    #[error("revalidate retained installation around journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact FreshDbInvalidated nor RollbackComplete record (fresh_db_invalidated={expected_fresh_db_invalidated:?}, rollback_complete={expected_rollback_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_fresh_db_invalidated: Box<TransitionRecord>,
        expected_rollback_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackCompleteRouteReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
