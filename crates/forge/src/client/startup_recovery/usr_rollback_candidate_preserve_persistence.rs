//! Persist one fully durable NewState candidate outcome as `CandidatePreserved`.
//!
//! The supplied authority owns exact preserved namespace, database, journal,
//! plan, and installation evidence and fixes its Applied or AlreadySatisfied
//! origin privately. This boundary revalidates that complete authority,
//! derives its sole successor, performs exactly one conditional advance, and
//! drops both the authority and old store before canonical reopen. It performs
//! no database invalidation or later rollback action.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{CodecError, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{
    UsrRollbackCandidatePreserveAuthorityError, UsrRollbackNewStateCandidatePreserveDurableEffectAuthority,
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
pub(in crate::client) enum DurableUsrRollbackCandidatePreserveRecord {
    Source,
    CandidatePreserved,
}

/// Persist the sole `CandidatePreserved` successor fixed by durable candidate
/// evidence, then independently reopen and compare the complete record.
pub(in crate::client) fn persist_usr_rollback_candidate_preserve_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackNewStateCandidatePreserveDurableEffectAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackCandidatePreservePersistenceError> {
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackCandidatePreservePersistenceError::Authority(source));
    }
    let source_record = authority.record().clone();
    let successor = match authority.candidate_preserved_successor() {
        Ok(successor) if successor.phase == Phase::CandidatePreserved => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackCandidatePreservePersistenceError::UnexpectedSuccessor { phase: successor.phase });
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackCandidatePreservePersistenceError::SuccessorConstruction { source });
        }
    };

    before_usr_rollback_candidate_preserve_persistence_final_revalidation();
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackCandidatePreservePersistenceError::Authority(source));
    }
    let installation = authority.installation().clone();
    let advance = journal.advance(&source_record, &successor);

    // No result may retain or reuse the authority or old lock-bearing store.
    // Canonical reopen begins only after both capabilities are destroyed.
    drop(authority);
    drop(journal);

    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackCandidatePreserveReopenError::from);
    match advance {
        Ok(()) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackCandidatePreservePersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => Err(UsrRollbackCandidatePreservePersistenceError::ReopenAfterSuccessfulAdvance { source }),
        },
        Err(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackCandidatePreservePersistenceError::Advance {
                    durable: DurableUsrRollbackCandidatePreserveRecord::Source,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackCandidatePreservePersistenceError::Advance {
                    durable: DurableUsrRollbackCandidatePreserveRecord::CandidatePreserved,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackCandidatePreservePersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackCandidatePreservePersistenceError::AdvanceAndReopen {
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
) -> UsrRollbackCandidatePreserveReopenError {
    UsrRollbackCandidatePreserveReopenError::UnexpectedRecord {
        expected_source: Box::new(source.clone()),
        expected_candidate_preserved: Box::new(successor.clone()),
        actual: actual.map(Box::new),
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_AUTHORITY_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_candidate_preserve_persistence_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_candidate_preserve_persistence_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_candidate_preserve_persistence_final_revalidation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackCandidatePreservePersistenceError {
    #[error("revalidate exact durable NewState candidate-preservation authority")]
    Authority(#[source] UsrRollbackCandidatePreserveAuthorityError),
    #[error("derive the sole legal durable NewState CandidatePreserved successor")]
    SuccessorConstruction {
        #[source]
        source: CodecError,
    },
    #[error("durable candidate-preservation authority selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackCandidatePreserveRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its durable CandidatePreserved advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackCandidatePreserveReopenError,
    },
    #[error("journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackCandidatePreserveReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackCandidatePreserveReopenError {
    #[error("revalidate retained installation around journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact source nor CandidatePreserved record (source={expected_source:?}, candidate_preserved={expected_candidate_preserved:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_source: Box<TransitionRecord>,
        expected_candidate_preserved: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackCandidatePreserveReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
