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
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    UsrRollbackCandidatePreserveAuthorityError, UsrRollbackNewStateCandidatePreserveDurableEffectAuthority,
    UsrRollbackCandidatePreserveRecordAdvanceError,
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

enum UsrRollbackCandidatePreserveAdvanceOutcome {
    Published {
        successor: TransitionRecord,
        binding: TransitionJournalRecordBinding,
    },
    StorageFailed {
        successor: TransitionRecord,
        source: StorageError,
    },
    SuccessorBindingFailed {
        successor: TransitionRecord,
        source: UsrRollbackCandidatePreserveSuccessorBindingError,
    },
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

    before_usr_rollback_candidate_preserve_persistence_final_revalidation();
    let installation = authority.installation().clone();
    let advance = match authority.advance_candidate_preserved_record_binding(&journal) {
        Ok(published) => {
            let (successor, successor_binding) = published.into_parts();
            before_usr_rollback_candidate_preserve_successor_binding_revalidation();
            let exact = revalidate_published_candidate_preserved_binding(
                &installation,
                &journal,
                &successor_binding,
                &successor,
            );
            match exact {
                Ok(true) => UsrRollbackCandidatePreserveAdvanceOutcome::Published {
                    successor,
                    binding: successor_binding,
                },
                Ok(false) => {
                    drop(successor_binding);
                    UsrRollbackCandidatePreserveAdvanceOutcome::SuccessorBindingFailed {
                        successor,
                        source: UsrRollbackCandidatePreserveSuccessorBindingError::Changed,
                    }
                }
                Err(source) => {
                    drop(successor_binding);
                    UsrRollbackCandidatePreserveAdvanceOutcome::SuccessorBindingFailed { successor, source }
                }
            }
        }
        Err(UsrRollbackCandidatePreserveRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(UsrRollbackCandidatePreservePersistenceError::Authority(source));
        }
        Err(UsrRollbackCandidatePreserveRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(UsrRollbackCandidatePreservePersistenceError::Installation(source));
        }
        Err(UsrRollbackCandidatePreserveRecordAdvanceError::Successor(source)) => {
            drop(journal);
            return Err(UsrRollbackCandidatePreservePersistenceError::SuccessorConstruction { source });
        }
        Err(UsrRollbackCandidatePreserveRecordAdvanceError::UnexpectedSuccessor { phase }) => {
            drop(journal);
            return Err(UsrRollbackCandidatePreservePersistenceError::UnexpectedSuccessor { phase });
        }
        Err(UsrRollbackCandidatePreserveRecordAdvanceError::Storage { source, successor }) => {
            UsrRollbackCandidatePreserveAdvanceOutcome::StorageFailed { successor, source }
        }
    };

    // The evidence authority and exact predecessor binding were consumed by
    // the bound advance. Reopening while the old store remains alive would
    // retain the canonical lock.
    drop(journal);

    if let UsrRollbackCandidatePreserveAdvanceOutcome::Published { .. } = &advance {
        after_usr_rollback_candidate_preserve_successor_binding_check_before_reopen();
    }
    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackCandidatePreserveReopenError::from);
    match advance {
        UsrRollbackCandidatePreserveAdvanceOutcome::Published {
            successor,
            binding: successor_binding,
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                let exact = revalidate_reopened_candidate_preserved_binding(
                    &installation,
                    &reopened,
                    &successor_binding,
                    &successor,
                );
                drop(successor_binding);
                match exact {
                    Ok(true) => Ok((reopened, successor)),
                    Ok(false) => {
                        drop(reopened);
                        Err(
                            UsrRollbackCandidatePreservePersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackCandidatePreserveRecord::CandidatePreserved,
                                source: UsrRollbackCandidatePreserveSuccessorBindingError::Changed,
                            },
                        )
                    }
                    Err(source) => {
                        drop(reopened);
                        Err(
                            UsrRollbackCandidatePreservePersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackCandidatePreserveRecord::CandidatePreserved,
                                source,
                            },
                        )
                    }
                }
            }
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
        UsrRollbackCandidatePreserveAdvanceOutcome::StorageFailed {
            successor,
            source: advance_error,
        } => match reopened {
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
        UsrRollbackCandidatePreserveAdvanceOutcome::SuccessorBindingFailed {
            successor,
            source: binding,
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackCandidatePreservePersistenceError::SuccessorRecordBinding {
                    durable: DurableUsrRollbackCandidatePreserveRecord::Source,
                    source: binding,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackCandidatePreservePersistenceError::SuccessorRecordBinding {
                    durable: DurableUsrRollbackCandidatePreserveRecord::CandidatePreserved,
                    source: binding,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackCandidatePreservePersistenceError::SuccessorRecordBindingAndReopen {
                        binding,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackCandidatePreservePersistenceError::SuccessorRecordBindingAndReopen {
                    binding,
                    reopen,
                },
            ),
        },
    }
}

fn revalidate_published_candidate_preserved_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackCandidatePreserveSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackCandidatePreserveSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackCandidatePreserveSuccessorBindingError::Installation)?;
    let exact = journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackCandidatePreserveSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackCandidatePreserveSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn revalidate_reopened_candidate_preserved_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackCandidatePreserveSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackCandidatePreserveSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackCandidatePreserveSuccessorBindingError::Installation)?;
    let exact = journal
        .has_reopened_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackCandidatePreserveSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackCandidatePreserveSuccessorBindingError::Installation)?;
    Ok(exact)
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
    static BEFORE_SUCCESSOR_BINDING_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
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

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_candidate_preserve_successor_binding_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_candidate_preserve_successor_binding_revalidation() {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_candidate_preserve_successor_binding_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_after_usr_rollback_candidate_preserve_successor_binding_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_usr_rollback_candidate_preserve_successor_binding_check_before_reopen() {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_candidate_preserve_successor_binding_check_before_reopen() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackCandidatePreserveSuccessorBindingError {
    #[error("revalidate retained installation after publishing the CandidatePreserved outcome")]
    Installation(#[source] installation::Error),
    #[error("the published CandidatePreserved successor lost its exact record binding")]
    Changed,
    #[error("revalidate the published CandidatePreserved successor record binding")]
    Storage(#[source] StorageError),
}

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
    #[error("revalidate retained installation before the exact CandidatePreserved record advance")]
    Installation(#[from] installation::Error),
    #[error("published successor binding failed with exact durable {durable:?} candidate-preservation evidence")]
    SuccessorRecordBinding {
        durable: DurableUsrRollbackCandidatePreserveRecord,
        #[source]
        source: UsrRollbackCandidatePreserveSuccessorBindingError,
    },
    #[error("successor binding failed ({binding}) and its canonical record could not be reconciled")]
    SuccessorRecordBindingAndReopen {
        binding: UsrRollbackCandidatePreserveSuccessorBindingError,
        #[source]
        reopen: UsrRollbackCandidatePreserveReopenError,
    },
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
