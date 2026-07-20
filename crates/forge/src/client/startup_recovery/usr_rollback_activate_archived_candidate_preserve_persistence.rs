//! Persist one durable ActivateArchived candidate outcome as `CandidatePreserved`.
//!
//! The supplied authority owns exact preserved namespace, database, journal,
//! plan, and installation evidence and fixes its Applied or AlreadySatisfied
//! origin privately. This boundary revalidates twice, derives one successor,
//! performs one conditional advance, destroys old capabilities, and reopens
//! the canonical journal. It performs no completion route or later work.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    UsrRollbackArchivedCandidatePreserveDurableEffectAuthority,
    UsrRollbackArchivedCandidatePreserveRecordAdvanceError, UsrRollbackCandidatePreserveAuthorityError,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackArchivedCandidatePreserveRecord {
    Source,
    CandidatePreserved,
}

enum UsrRollbackArchivedCandidatePreserveAdvanceOutcome {
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
        source: UsrRollbackArchivedCandidatePreserveSuccessorBindingError,
    },
}

pub(in crate::client) fn persist_usr_rollback_archived_candidate_preserve_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackArchivedCandidatePreservePersistenceError> {
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackArchivedCandidatePreservePersistenceError::Authority(source));
    }
    let source_record = authority.record().clone();

    before_usr_rollback_archived_candidate_preserve_persistence_final_revalidation();
    let installation = authority.installation().clone();
    let advance = match authority.advance_candidate_preserved_record_binding(&journal) {
        Ok(published) => {
            let (successor, successor_binding) = published.into_parts();
            before_usr_rollback_archived_candidate_preserve_successor_binding_revalidation();
            let exact = revalidate_published_archived_candidate_preserved_binding(
                &installation,
                &journal,
                &successor_binding,
                &successor,
            );
            match exact {
                Ok(true) => UsrRollbackArchivedCandidatePreserveAdvanceOutcome::Published {
                    successor,
                    binding: successor_binding,
                },
                Ok(false) => {
                    drop(successor_binding);
                    UsrRollbackArchivedCandidatePreserveAdvanceOutcome::SuccessorBindingFailed {
                        successor,
                        source: UsrRollbackArchivedCandidatePreserveSuccessorBindingError::Changed,
                    }
                }
                Err(source) => {
                    drop(successor_binding);
                    UsrRollbackArchivedCandidatePreserveAdvanceOutcome::SuccessorBindingFailed {
                        successor,
                        source,
                    }
                }
            }
        }
        Err(UsrRollbackArchivedCandidatePreserveRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(UsrRollbackArchivedCandidatePreservePersistenceError::Authority(source));
        }
        Err(UsrRollbackArchivedCandidatePreserveRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(UsrRollbackArchivedCandidatePreservePersistenceError::Installation(source));
        }
        Err(UsrRollbackArchivedCandidatePreserveRecordAdvanceError::Successor(source)) => {
            drop(journal);
            return Err(UsrRollbackArchivedCandidatePreservePersistenceError::SuccessorConstruction { source });
        }
        Err(UsrRollbackArchivedCandidatePreserveRecordAdvanceError::UnexpectedSuccessor { phase }) => {
            drop(journal);
            return Err(UsrRollbackArchivedCandidatePreservePersistenceError::UnexpectedSuccessor { phase });
        }
        Err(UsrRollbackArchivedCandidatePreserveRecordAdvanceError::Storage { source, successor }) => {
            UsrRollbackArchivedCandidatePreserveAdvanceOutcome::StorageFailed { successor, source }
        }
    };

    // The evidence authority and exact predecessor binding were consumed by
    // the bound advance. Reopening while the old store remains alive would
    // retain the canonical lock.
    drop(journal);

    if let UsrRollbackArchivedCandidatePreserveAdvanceOutcome::Published { .. } = &advance {
        after_usr_rollback_archived_candidate_preserve_successor_binding_check_before_reopen();
    }
    let reopened =
        reopen_canonical_journal(&installation).map_err(UsrRollbackArchivedCandidatePreserveReopenError::from);
    match advance {
        UsrRollbackArchivedCandidatePreserveAdvanceOutcome::Published {
            successor,
            binding: successor_binding,
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                let exact = revalidate_reopened_archived_candidate_preserved_binding(
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
                            UsrRollbackArchivedCandidatePreservePersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackArchivedCandidatePreserveRecord::CandidatePreserved,
                                source: UsrRollbackArchivedCandidatePreserveSuccessorBindingError::Changed,
                            },
                        )
                    }
                    Err(source) => {
                        drop(reopened);
                        Err(
                            UsrRollbackArchivedCandidatePreservePersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackArchivedCandidatePreserveRecord::CandidatePreserved,
                                source,
                            },
                        )
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackArchivedCandidatePreservePersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => {
                Err(UsrRollbackArchivedCandidatePreservePersistenceError::ReopenAfterSuccessfulAdvance { source })
            }
        },
        UsrRollbackArchivedCandidatePreserveAdvanceOutcome::StorageFailed {
            successor,
            source: advance_error,
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackArchivedCandidatePreservePersistenceError::Advance {
                    durable: DurableUsrRollbackArchivedCandidatePreserveRecord::Source,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackArchivedCandidatePreservePersistenceError::Advance {
                    durable: DurableUsrRollbackArchivedCandidatePreserveRecord::CandidatePreserved,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackArchivedCandidatePreservePersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackArchivedCandidatePreservePersistenceError::AdvanceAndReopen {
                advance: advance_error,
                reopen,
            }),
        },
        UsrRollbackArchivedCandidatePreserveAdvanceOutcome::SuccessorBindingFailed {
            successor,
            source: binding,
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(
                    UsrRollbackArchivedCandidatePreservePersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackArchivedCandidatePreserveRecord::Source,
                        source: binding,
                    },
                )
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(
                    UsrRollbackArchivedCandidatePreservePersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackArchivedCandidatePreserveRecord::CandidatePreserved,
                        source: binding,
                    },
                )
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackArchivedCandidatePreservePersistenceError::SuccessorRecordBindingAndReopen {
                        binding,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackArchivedCandidatePreservePersistenceError::SuccessorRecordBindingAndReopen {
                    binding,
                    reopen,
                },
            ),
        },
    }
}

fn revalidate_published_archived_candidate_preserved_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackArchivedCandidatePreserveSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackArchivedCandidatePreserveSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackArchivedCandidatePreserveSuccessorBindingError::Installation)?;
    let exact = journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackArchivedCandidatePreserveSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackArchivedCandidatePreserveSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn revalidate_reopened_archived_candidate_preserved_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackArchivedCandidatePreserveSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackArchivedCandidatePreserveSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackArchivedCandidatePreserveSuccessorBindingError::Installation)?;
    let exact = journal
        .has_reopened_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackArchivedCandidatePreserveSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackArchivedCandidatePreserveSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackArchivedCandidatePreserveReopenError {
    UsrRollbackArchivedCandidatePreserveReopenError::UnexpectedRecord {
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
pub(in crate::client) fn arm_before_usr_rollback_archived_candidate_preserve_persistence_final_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_archived_candidate_preserve_persistence_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_archived_candidate_preserve_persistence_final_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_archived_candidate_preserve_successor_binding_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_archived_candidate_preserve_successor_binding_revalidation() {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_archived_candidate_preserve_successor_binding_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_after_usr_rollback_archived_candidate_preserve_successor_binding_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_usr_rollback_archived_candidate_preserve_successor_binding_check_before_reopen() {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_archived_candidate_preserve_successor_binding_check_before_reopen() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackArchivedCandidatePreserveSuccessorBindingError {
    #[error("revalidate retained installation after publishing the archived CandidatePreserved outcome")]
    Installation(#[source] installation::Error),
    #[error("the published archived CandidatePreserved successor lost its exact record binding")]
    Changed,
    #[error("revalidate the published archived CandidatePreserved successor record binding")]
    Storage(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackArchivedCandidatePreservePersistenceError {
    #[error("revalidate exact durable ActivateArchived candidate-preservation authority")]
    Authority(#[source] UsrRollbackCandidatePreserveAuthorityError),
    #[error("derive the sole legal durable ActivateArchived CandidatePreserved successor")]
    SuccessorConstruction {
        #[source]
        source: CodecError,
    },
    #[error("durable ActivateArchived candidate-preservation authority selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("revalidate retained installation before the exact archived CandidatePreserved record advance")]
    Installation(#[from] installation::Error),
    #[error("published archived successor binding failed with exact durable {durable:?} evidence")]
    SuccessorRecordBinding {
        durable: DurableUsrRollbackArchivedCandidatePreserveRecord,
        #[source]
        source: UsrRollbackArchivedCandidatePreserveSuccessorBindingError,
    },
    #[error("archived successor binding failed ({binding}) and its canonical record could not be reconciled")]
    SuccessorRecordBindingAndReopen {
        binding: UsrRollbackArchivedCandidatePreserveSuccessorBindingError,
        #[source]
        reopen: UsrRollbackArchivedCandidatePreserveReopenError,
    },
    #[error("ActivateArchived journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackArchivedCandidatePreserveRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its durable ActivateArchived CandidatePreserved advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackArchivedCandidatePreserveReopenError,
    },
    #[error("ActivateArchived journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackArchivedCandidatePreserveReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackArchivedCandidatePreserveReopenError {
    #[error("revalidate retained installation around ActivateArchived journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical ActivateArchived journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact ActivateArchived source nor CandidatePreserved record (source={expected_source:?}, candidate_preserved={expected_candidate_preserved:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_source: Box<TransitionRecord>,
        expected_candidate_preserved: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackArchivedCandidatePreserveReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
