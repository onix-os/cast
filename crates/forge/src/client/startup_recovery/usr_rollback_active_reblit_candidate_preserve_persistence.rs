//! Persist one fully durable ActiveReblit candidate outcome as `CandidatePreserved`.
//!
//! The supplied authority owns exact preserved namespace, database, journal, plan, and
//! installation evidence and fixes its Applied or AlreadySatisfied origin
//! privately. This boundary revalidates that authority twice, derives its sole
//! successor, performs exactly one conditional advance, and drops both the
//! authority and old store before canonical reopen. It performs no later
//! rollback route, database mutation, cleanup, or trigger work.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{CodecError, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{
    UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority, UsrRollbackCandidatePreserveAuthorityError,
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
pub(in crate::client) enum DurableUsrRollbackActiveReblitCandidatePreserveRecord {
    Source,
    CandidatePreserved,
}

/// Persist the sole `CandidatePreserved` successor fixed by durable
/// ActiveReblit evidence, then independently reopen and compare the record.
pub(in crate::client) fn persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackActiveReblitCandidatePreservePersistenceError> {
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackActiveReblitCandidatePreservePersistenceError::Authority(
            source,
        ));
    }
    let source_record = authority.record().clone();
    let successor = match authority.candidate_preserved_successor() {
        Ok(successor) if successor.phase == Phase::CandidatePreserved => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(
                UsrRollbackActiveReblitCandidatePreservePersistenceError::UnexpectedSuccessor {
                    phase: successor.phase,
                },
            );
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackActiveReblitCandidatePreservePersistenceError::SuccessorConstruction { source });
        }
    };

    before_usr_rollback_active_reblit_candidate_preserve_persistence_final_revalidation();
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackActiveReblitCandidatePreservePersistenceError::Authority(
            source,
        ));
    }
    let installation = authority.installation().clone();
    let advance = journal.advance(&source_record, &successor);

    // Canonical reopen starts only after every old capability is destroyed.
    drop(authority);
    drop(journal);

    let reopened =
        reopen_canonical_journal(&installation).map_err(UsrRollbackActiveReblitCandidatePreserveReopenError::from);
    match advance {
        Ok(()) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitCandidatePreservePersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => {
                Err(UsrRollbackActiveReblitCandidatePreservePersistenceError::ReopenAfterSuccessfulAdvance { source })
            }
        },
        Err(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackActiveReblitCandidatePreservePersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackActiveReblitCandidatePreservePersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitCandidatePreservePersistenceError::AdvanceAndReopen {
                        advance: advance_error,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackActiveReblitCandidatePreservePersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen,
                },
            ),
        },
    }
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackActiveReblitCandidatePreserveReopenError {
    UsrRollbackActiveReblitCandidatePreserveReopenError::UnexpectedRecord {
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
pub(in crate::client) fn arm_before_usr_rollback_active_reblit_candidate_preserve_persistence_final_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_active_reblit_candidate_preserve_persistence_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_active_reblit_candidate_preserve_persistence_final_revalidation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitCandidatePreservePersistenceError {
    #[error("revalidate exact durable ActiveReblit candidate-preservation authority")]
    Authority(#[source] UsrRollbackCandidatePreserveAuthorityError),
    #[error("derive the sole legal durable ActiveReblit CandidatePreserved successor")]
    SuccessorConstruction {
        #[source]
        source: CodecError,
    },
    #[error("durable ActiveReblit candidate-preservation authority selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("ActiveReblit journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its durable ActiveReblit CandidatePreserved advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackActiveReblitCandidatePreserveReopenError,
    },
    #[error("ActiveReblit journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackActiveReblitCandidatePreserveReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitCandidatePreserveReopenError {
    #[error("revalidate retained installation around ActiveReblit journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical ActiveReblit journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact ActiveReblit source nor CandidatePreserved record (source={expected_source:?}, candidate_preserved={expected_candidate_preserved:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_source: Box<TransitionRecord>,
        expected_candidate_preserved: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackActiveReblitCandidatePreserveReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
