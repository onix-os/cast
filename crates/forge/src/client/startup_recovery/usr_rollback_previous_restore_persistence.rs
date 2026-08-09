//! Persist one reconciled previous-restore outcome as
//! `PreviousRestoredToStaging`.
//!
//! The supplied authority has already reconciled the effect and fixed its
//! outcome privately. This boundary revalidates that complete evidence,
//! derives the authority-owned successor, performs exactly one conditional
//! journal advance, and then destroys both the authority and the old
//! lock-bearing store before reopening the canonical journal.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    UsrRollbackPreviousRestoreAuthorityError, UsrRollbackPreviousRestoreDurableEffectAuthority,
    UsrRollbackPreviousRestoreRecordAdvanceError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackPreviousRestoreRecord {
    Source,
    PreviousRestoredToStaging,
}

enum AdvanceOutcome {
    Published {
        successor: TransitionRecord,
        binding: TransitionJournalRecordBinding,
    },
    StorageFailed {
        successor: TransitionRecord,
        source: StorageError,
    },
}

/// Persist the sole `PreviousRestoredToStaging` successor fixed by the durable
/// evidence, then independently reopen and compare the canonical record.
pub(in crate::client) fn persist_usr_rollback_previous_restore_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackPreviousRestoreDurableEffectAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackPreviousRestorePersistenceError> {
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackPreviousRestorePersistenceError::Authority(source));
    }
    let source_record = authority.record().clone();
    let installation = authority.installation().clone();

    let advance = match authority.advance_previous_restored_record_binding(&journal) {
        Ok(published) => {
            let (successor, binding) = published.into_parts();
            let exact = match installation.retained_mutable_cast_directory() {
                Ok(cast) => journal.has_record_binding(cast, &binding, &successor),
                Err(source) => {
                    drop(binding);
                    drop(journal);
                    return Err(UsrRollbackPreviousRestorePersistenceError::Installation(source));
                }
            };
            match exact {
                Ok(true) => AdvanceOutcome::Published { successor, binding },
                Ok(false) => {
                    drop(binding);
                    drop(journal);
                    return Err(UsrRollbackPreviousRestorePersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackPreviousRestoreRecord::PreviousRestoredToStaging,
                    });
                }
                Err(source) => {
                    drop(binding);
                    drop(journal);
                    return Err(UsrRollbackPreviousRestorePersistenceError::Journal(source));
                }
            }
        }
        Err(UsrRollbackPreviousRestoreRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(UsrRollbackPreviousRestorePersistenceError::Authority(source));
        }
        Err(UsrRollbackPreviousRestoreRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(UsrRollbackPreviousRestorePersistenceError::Installation(source));
        }
        Err(UsrRollbackPreviousRestoreRecordAdvanceError::Successor(source)) => {
            drop(journal);
            return Err(UsrRollbackPreviousRestorePersistenceError::Successor { source });
        }
        Err(UsrRollbackPreviousRestoreRecordAdvanceError::UnexpectedSuccessor { phase }) => {
            drop(journal);
            return Err(UsrRollbackPreviousRestorePersistenceError::UnexpectedSuccessor { phase });
        }
        Err(UsrRollbackPreviousRestoreRecordAdvanceError::Storage { source, successor }) => {
            AdvanceOutcome::StorageFailed { successor, source }
        }
    };

    // The evidence authority and its exact predecessor binding were consumed
    // by the bound advance. Never reopen while the old lock-bearing store
    // remains alive, and never reuse it after an uncertain write result.
    drop(journal);
    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackPreviousRestoreReopenError::from);

    match advance {
        AdvanceOutcome::Published { successor, binding } => {
            drop(binding);
            match reopened {
                Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),
                Ok((reopened, actual)) => {
                    drop(reopened);
                    Err(
                        UsrRollbackPreviousRestorePersistenceError::ReopenAfterSuccessfulAdvance {
                            source: unexpected_record(&source_record, &successor, actual),
                        },
                    )
                }
                Err(source) => Err(UsrRollbackPreviousRestorePersistenceError::ReopenAfterSuccessfulAdvance { source }),
            }
        }
        AdvanceOutcome::StorageFailed { successor, source } => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackPreviousRestorePersistenceError::Advance {
                    durable: DurableUsrRollbackPreviousRestoreRecord::Source,
                    source,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackPreviousRestorePersistenceError::Advance {
                    durable: DurableUsrRollbackPreviousRestoreRecord::PreviousRestoredToStaging,
                    source,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackPreviousRestorePersistenceError::AdvanceAndReopen {
                    advance: source,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackPreviousRestorePersistenceError::AdvanceAndReopen {
                advance: source,
                reopen,
            }),
        },
    }
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackPreviousRestoreReopenError {
    UsrRollbackPreviousRestoreReopenError::UnexpectedRecord {
        expected_source: Box::new(source.clone()),
        expected_successor: Box::new(successor.clone()),
        actual: actual.map(Box::new),
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackPreviousRestorePersistenceError {
    #[error("revalidate exact durable startup previous-restore authority")]
    Authority(#[from] UsrRollbackPreviousRestoreAuthorityError),
    #[error("revalidate retained installation around the previous-restore advance")]
    Installation(#[from] installation::Error),
    #[error("revalidate the exact previous-restore journal record binding")]
    Journal(#[source] StorageError),
    #[error("derive the authority-owned previous-restore successor")]
    Successor {
        #[source]
        source: CodecError,
    },
    #[error("authority-owned previous-restore successor has unexpected phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("the published previous-restore {durable:?} record lost its exact binding")]
    SuccessorRecordBinding {
        durable: DurableUsrRollbackPreviousRestoreRecord,
    },
    #[error("journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackPreviousRestoreRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its previous-restore advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackPreviousRestoreReopenError,
    },
    #[error("journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackPreviousRestoreReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackPreviousRestoreReopenError {
    #[error("revalidate retained installation around journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact source nor successor record (source={expected_source:?}, successor={expected_successor:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_source: Box<TransitionRecord>,
        expected_successor: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackPreviousRestoreReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
