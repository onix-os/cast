//! Persist one fully durable reverse `/usr` outcome as `UsrRestored`.
//!
//! The supplied authority has already reconciled the reverse effect, completed
//! both parent-durability barriers, and fixed its outcome privately. This
//! boundary revalidates that complete evidence, derives the authority-owned
//! successor, performs exactly one conditional journal advance, and then
//! destroys both the authority and old lock-bearing store before reopening the
//! canonical journal. It performs no later rollback action or recovery effect.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{CodecError, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{
    UsrRollbackReverseAuthorityError, UsrRollbackReverseDurableEffectAuthority,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

#[cfg(test)]
#[allow(dead_code)] // shared reverse fixture contains wider reconciliation helpers
#[path = "../startup_reconciliation/usr_rollback_reverse_authority/tests/support.rs"]
mod reverse_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider startup-recovery helpers
#[path = "test_support.rs"]
mod test_fixture;
#[cfg(test)]
mod tests;

/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackReverseRecord {
    Source,
    UsrRestored,
}

/// Persist the sole `UsrRestored` successor fixed by durable reverse evidence,
/// then independently reopen and compare the complete canonical record.
#[allow(dead_code)] // wired only when the reverse startup dispatcher lands
pub(in crate::client) fn persist_usr_rollback_reverse_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackReverseDurableEffectAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackReversePersistenceError> {
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackReversePersistenceError::Authority(source));
    }
    let source_record = authority.record().clone();
    let successor = match authority.usr_restored_successor() {
        Ok(successor) if successor.phase == Phase::UsrRestored => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackReversePersistenceError::UnexpectedSuccessor { phase: successor.phase });
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackReversePersistenceError::SuccessorConstruction { source });
        }
    };

    before_usr_rollback_reverse_persistence_final_revalidation();
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackReversePersistenceError::Authority(source));
    }
    let installation = authority.installation().clone();
    let advance = journal.advance(&source_record, &successor);

    // No result may retain or reuse the authority or old store. Reopening while
    // either remains alive would retain stale evidence or the canonical lock.
    drop(authority);
    drop(journal);

    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackReverseReopenError::from);
    match advance {
        Ok(()) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackReversePersistenceError::ReopenAfterSuccessfulAdvance {
                    source: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(source) => Err(UsrRollbackReversePersistenceError::ReopenAfterSuccessfulAdvance { source }),
        },
        Err(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackReversePersistenceError::Advance {
                    durable: DurableUsrRollbackReverseRecord::Source,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackReversePersistenceError::Advance {
                    durable: DurableUsrRollbackReverseRecord::UsrRestored,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackReversePersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackReversePersistenceError::AdvanceAndReopen {
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
) -> UsrRollbackReverseReopenError {
    UsrRollbackReverseReopenError::UnexpectedRecord {
        expected_source: Box::new(source.clone()),
        expected_usr_restored: Box::new(successor.clone()),
        actual: actual.map(Box::new),
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_AUTHORITY_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_reverse_persistence_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_reverse_persistence_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_reverse_persistence_final_revalidation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackReversePersistenceError {
    #[error("revalidate exact durable startup /usr rollback-reverse authority")]
    Authority(#[source] UsrRollbackReverseAuthorityError),
    #[error("derive the sole legal durable startup /usr rollback-reverse successor")]
    SuccessorConstruction {
        #[source]
        source: CodecError,
    },
    #[error("durable rollback-reverse authority selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackReverseRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its durable rollback-reverse advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackReverseReopenError,
    },
    #[error("journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackReverseReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackReverseReopenError {
    #[error("revalidate retained installation around journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact source nor UsrRestored record (source={expected_source:?}, usr_restored={expected_usr_restored:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_source: Box<TransitionRecord>,
        expected_usr_restored: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackReverseReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
