//! Persist one authenticated `/usr` rollback decision and reopen its journal.
//!
//! Namespace and database evidence are owned by the supplied authority. This
//! executor performs no recovery effect other than the single conditional
//! journal advance. The old journal store and authority are explicitly dropped
//! before the descriptor-rooted reopen, so an uncertain storage result cannot
//! be retried through stale in-memory authority. Only an exact successfully
//! persisted decision returns that freshly reopened lock-bearing store.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{CodecError, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{UsrRollbackDecisionAuthority, UsrRollbackDecisionAuthorityError};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

#[cfg(test)]
mod tests;

/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackDecisionRecord {
    Source,
    Decision,
}

/// Persist exactly one authenticated rollback decision, then independently
/// reopen and compare the complete canonical record.
///
/// The authority owns the installation and source record. Callers supply only
/// the journal store because it must be consumed and dropped before reopening;
/// they cannot mix an independently chosen installation or expected record
/// into this mutation boundary.
#[allow(dead_code)] // wired only after the complete startup authority slice lands
pub(in crate::client) fn persist_usr_rollback_decision_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackDecisionAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackDecisionPersistenceError> {
    authority.revalidate(&journal)?;
    let observations = authority.observations();
    let source_record = authority.record().clone();
    let decision = match source_record.rollback_decision(observations) {
        Ok(decision) => decision,
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackDecisionPersistenceError::DecisionConstruction { source });
        }
    };

    before_usr_rollback_decision_final_revalidation();
    authority.revalidate(&journal)?;
    let installation = authority.installation().clone();
    let advance = journal.advance(&source_record, &decision);

    // Neither branch below may retain or reuse the old lock-bearing store or
    // its evidence authority. In particular, reopening while either remains
    // alive would deadlock or authorize a second persistence attempt.
    drop(authority);
    drop(journal);

    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackDecisionReopenError::from);
    match advance {
        Ok(()) => match reopened {
            Ok((reopened, Some(actual))) if actual == decision => Ok((reopened, decision)),
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackDecisionPersistenceError::ReopenAfterSuccessfulAdvance {
                    source: unexpected_record(&source_record, &decision, actual),
                })
            }
            Err(source) => Err(UsrRollbackDecisionPersistenceError::ReopenAfterSuccessfulAdvance { source }),
        },
        Err(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackDecisionPersistenceError::Advance {
                    durable: DurableUsrRollbackDecisionRecord::Source,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == decision => {
                drop(reopened);
                Err(UsrRollbackDecisionPersistenceError::Advance {
                    durable: DurableUsrRollbackDecisionRecord::Decision,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackDecisionPersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &decision, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackDecisionPersistenceError::AdvanceAndReopen {
                advance: advance_error,
                reopen,
            }),
        },
    }
}

fn unexpected_record(
    source: &TransitionRecord,
    decision: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackDecisionReopenError {
    UsrRollbackDecisionReopenError::UnexpectedRecord {
        expected_source: Box::new(source.clone()),
        expected_decision: Box::new(decision.clone()),
        actual: actual.map(Box::new),
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_AUTHORITY_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[allow(dead_code)] // consumed by the focused startup recovery race matrix
pub(crate) fn arm_before_usr_rollback_decision_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_decision_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_decision_final_revalidation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackDecisionPersistenceError {
    #[error("revalidate exact startup /usr rollback-decision authority")]
    Authority(#[from] UsrRollbackDecisionAuthorityError),
    #[error("derive the sole legal startup /usr rollback decision")]
    DecisionConstruction {
        #[source]
        source: CodecError,
    },
    #[error("journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackDecisionRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its rollback-decision advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackDecisionReopenError,
    },
    #[error("journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackDecisionReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackDecisionReopenError {
    #[error("revalidate retained installation around journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact source nor decision record (source={expected_source:?}, decision={expected_decision:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_source: Box<TransitionRecord>,
        expected_decision: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackDecisionReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
