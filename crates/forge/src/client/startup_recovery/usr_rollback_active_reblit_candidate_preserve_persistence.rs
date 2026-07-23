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
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority,
    UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError, UsrRollbackCandidatePreserveAuthorityError,
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

enum UsrRollbackActiveReblitCandidatePreserveAdvanceOutcome {
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
        source: UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError,
    },
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

    before_usr_rollback_active_reblit_candidate_preserve_persistence_final_revalidation();
    let installation = authority.installation().clone();
    let advance = match authority.advance_candidate_preserved_record_binding(&journal) {
        Ok(published) => {
            let (successor, successor_binding) = published.into_parts();
            before_usr_rollback_active_reblit_candidate_preserve_successor_binding_revalidation();
            let exact = revalidate_published_active_reblit_candidate_preserved_binding(
                &installation,
                &journal,
                &successor_binding,
                &successor,
            );
            match exact {
                Ok(true) => UsrRollbackActiveReblitCandidatePreserveAdvanceOutcome::Published {
                    successor,
                    binding: successor_binding,
                },
                Ok(false) => {
                    drop(successor_binding);
                    UsrRollbackActiveReblitCandidatePreserveAdvanceOutcome::SuccessorBindingFailed {
                        successor,
                        source: UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError::Changed,
                    }
                }
                Err(source) => {
                    drop(successor_binding);
                    UsrRollbackActiveReblitCandidatePreserveAdvanceOutcome::SuccessorBindingFailed {
                        successor,
                        source,
                    }
                }
            }
        }
        Err(UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(UsrRollbackActiveReblitCandidatePreservePersistenceError::Authority(source));
        }
        Err(UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(UsrRollbackActiveReblitCandidatePreservePersistenceError::Installation(source));
        }
        Err(UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError::Successor(source)) => {
            drop(journal);
            return Err(UsrRollbackActiveReblitCandidatePreservePersistenceError::SuccessorConstruction { source });
        }
        Err(UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError::UnexpectedSuccessor { phase }) => {
            drop(journal);
            return Err(UsrRollbackActiveReblitCandidatePreservePersistenceError::UnexpectedSuccessor { phase });
        }
        Err(UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError::Storage { source, successor }) => {
            UsrRollbackActiveReblitCandidatePreserveAdvanceOutcome::StorageFailed { successor, source }
        }
    };

    // The evidence authority and exact predecessor binding were consumed by
    // the bound advance. Reopening while the old store remains alive would
    // retain the canonical lock.
    drop(journal);

    if let UsrRollbackActiveReblitCandidatePreserveAdvanceOutcome::Published { .. } = &advance {
        after_usr_rollback_active_reblit_candidate_preserve_successor_binding_check_before_reopen();
    }
    let reopened =
        reopen_canonical_journal(&installation).map_err(UsrRollbackActiveReblitCandidatePreserveReopenError::from);
    match advance {
        UsrRollbackActiveReblitCandidatePreserveAdvanceOutcome::Published {
            successor,
            binding: successor_binding,
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                let exact = revalidate_reopened_active_reblit_candidate_preserved_binding(
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
                            UsrRollbackActiveReblitCandidatePreservePersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,
                                source: UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError::Changed,
                            },
                        )
                    }
                    Err(source) => {
                        drop(reopened);
                        Err(
                            UsrRollbackActiveReblitCandidatePreservePersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,
                                source,
                            },
                        )
                    }
                }
            }
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
        UsrRollbackActiveReblitCandidatePreserveAdvanceOutcome::StorageFailed {
            successor,
            source: advance_error,
        } => match reopened {
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
        UsrRollbackActiveReblitCandidatePreserveAdvanceOutcome::SuccessorBindingFailed {
            successor,
            source: binding,
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitCandidatePreservePersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source,
                        source: binding,
                    },
                )
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitCandidatePreservePersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,
                        source: binding,
                    },
                )
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitCandidatePreservePersistenceError::SuccessorRecordBindingAndReopen {
                        binding,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackActiveReblitCandidatePreservePersistenceError::SuccessorRecordBindingAndReopen {
                    binding,
                    reopen,
                },
            ),
        },
    }
}

fn revalidate_published_active_reblit_candidate_preserved_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError::Installation)?;
    let exact = journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn revalidate_reopened_active_reblit_candidate_preserved_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError::Installation)?;
    let exact = journal
        .has_reopened_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError::Installation)?;
    Ok(exact)
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
    static BEFORE_SUCCESSOR_BINDING_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
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

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_active_reblit_candidate_preserve_successor_binding_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_active_reblit_candidate_preserve_successor_binding_revalidation() {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_active_reblit_candidate_preserve_successor_binding_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_after_usr_rollback_active_reblit_candidate_preserve_successor_binding_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_usr_rollback_active_reblit_candidate_preserve_successor_binding_check_before_reopen() {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_active_reblit_candidate_preserve_successor_binding_check_before_reopen() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError {
    #[error("revalidate retained installation after publishing the ActiveReblit CandidatePreserved outcome")]
    Installation(#[source] installation::Error),
    #[error("the published ActiveReblit CandidatePreserved successor lost its exact record binding")]
    Changed,
    #[error("revalidate the published ActiveReblit CandidatePreserved successor record binding")]
    Storage(#[source] StorageError),
}

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
    #[error("revalidate retained installation before the exact ActiveReblit CandidatePreserved record advance")]
    Installation(#[from] installation::Error),
    #[error("published ActiveReblit successor binding failed with exact durable {durable:?} evidence")]
    SuccessorRecordBinding {
        durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord,
        #[source]
        source: UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError,
    },
    #[error("ActiveReblit successor binding failed ({binding}) and its canonical record could not be reconciled")]
    SuccessorRecordBindingAndReopen {
        binding: UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError,
        #[source]
        reopen: UsrRollbackActiveReblitCandidatePreserveReopenError,
    },
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
