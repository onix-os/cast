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
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    UsrRollbackReverseAuthorityError, UsrRollbackReverseDurableEffectAuthority,
    UsrRollbackReverseRecordAdvanceError,
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

enum UsrRollbackReverseAdvanceOutcome {
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
        source: UsrRollbackReverseSuccessorBindingError,
    },
}

/// Persist the sole `UsrRestored` successor fixed by durable reverse evidence,
/// then independently reopen and compare the complete canonical record.
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

    before_usr_rollback_reverse_persistence_final_revalidation();
    let installation = authority.installation().clone();
    let advance = match authority.advance_usr_restored_record_binding(&journal) {
        Ok(published) => {
            let (successor, successor_binding) = published.into_parts();
            before_usr_rollback_reverse_successor_binding_revalidation();
            let exact = match installation.retained_mutable_cast_directory() {
                Ok(cast) => journal
                    .has_record_binding(cast, &successor_binding, &successor)
                    .map_err(UsrRollbackReverseSuccessorBindingError::Storage),
                Err(source) => Err(UsrRollbackReverseSuccessorBindingError::Installation(source)),
            };
            match exact {
                Ok(true) => UsrRollbackReverseAdvanceOutcome::Published {
                    successor,
                    binding: successor_binding,
                },
                Ok(false) => {
                    drop(successor_binding);
                    UsrRollbackReverseAdvanceOutcome::SuccessorBindingFailed {
                        successor,
                        source: UsrRollbackReverseSuccessorBindingError::Changed,
                    }
                }
                Err(source) => {
                    drop(successor_binding);
                    UsrRollbackReverseAdvanceOutcome::SuccessorBindingFailed { successor, source }
                }
            }
        }
        Err(UsrRollbackReverseRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(UsrRollbackReversePersistenceError::Authority(source));
        }
        Err(UsrRollbackReverseRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(UsrRollbackReversePersistenceError::Installation(source));
        }
        Err(UsrRollbackReverseRecordAdvanceError::Successor(source)) => {
            drop(journal);
            return Err(UsrRollbackReversePersistenceError::SuccessorConstruction { source });
        }
        Err(UsrRollbackReverseRecordAdvanceError::UnexpectedSuccessor { phase }) => {
            drop(journal);
            return Err(UsrRollbackReversePersistenceError::UnexpectedSuccessor { phase });
        }
        Err(UsrRollbackReverseRecordAdvanceError::Storage { source, successor }) => {
            UsrRollbackReverseAdvanceOutcome::StorageFailed { successor, source }
        }
    };

    // The evidence authority and exact predecessor binding were consumed by
    // the bound advance. Reopening while the old store remains alive would
    // retain the canonical lock.
    drop(journal);

    if let UsrRollbackReverseAdvanceOutcome::Published { .. } = &advance {
        after_usr_rollback_reverse_successor_binding_check_before_reopen();
    }
    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackReverseReopenError::from);
    match advance {
        UsrRollbackReverseAdvanceOutcome::Published {
            successor,
            binding: successor_binding,
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                let exact = revalidate_reopened_reverse_binding(
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
                        Err(UsrRollbackReversePersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackReverseRecord::UsrRestored,
                            source: UsrRollbackReverseSuccessorBindingError::Changed,
                        })
                    }
                    Err(source) => {
                        drop(reopened);
                        Err(UsrRollbackReversePersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackReverseRecord::UsrRestored,
                            source,
                        })
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackReversePersistenceError::ReopenAfterSuccessfulAdvance {
                    source: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(source) => Err(UsrRollbackReversePersistenceError::ReopenAfterSuccessfulAdvance { source }),
        },
        UsrRollbackReverseAdvanceOutcome::StorageFailed {
            successor,
            source: advance_error,
        } => match reopened {
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
        UsrRollbackReverseAdvanceOutcome::SuccessorBindingFailed {
            successor,
            source: binding,
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackReversePersistenceError::SuccessorRecordBinding {
                    durable: DurableUsrRollbackReverseRecord::Source,
                    source: binding,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackReversePersistenceError::SuccessorRecordBinding {
                    durable: DurableUsrRollbackReverseRecord::UsrRestored,
                    source: binding,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackReversePersistenceError::SuccessorRecordBindingAndReopen {
                    binding,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackReversePersistenceError::SuccessorRecordBindingAndReopen {
                binding,
                reopen,
            }),
        },
    }
}

fn revalidate_reopened_reverse_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackReverseSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackReverseSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackReverseSuccessorBindingError::Installation)?;
    let exact = journal
        .has_reopened_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackReverseSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackReverseSuccessorBindingError::Installation)?;
    Ok(exact)
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
    static BEFORE_SUCCESSOR_BINDING_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
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

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_reverse_successor_binding_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_reverse_successor_binding_revalidation() {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_reverse_successor_binding_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_after_usr_rollback_reverse_successor_binding_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_usr_rollback_reverse_successor_binding_check_before_reopen() {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_reverse_successor_binding_check_before_reopen() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackReverseSuccessorBindingError {
    #[error("revalidate retained installation after publishing the rollback-reverse outcome")]
    Installation(#[source] installation::Error),
    #[error("the published rollback-reverse successor lost its exact record binding")]
    Changed,
    #[error("revalidate the published rollback-reverse successor record binding")]
    Storage(#[source] StorageError),
}

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
    #[error("revalidate retained installation before the exact rollback-reverse record advance")]
    Installation(#[from] installation::Error),
    #[error("successor binding failed after reopening exact durable {durable:?} rollback-reverse evidence")]
    SuccessorRecordBinding {
        durable: DurableUsrRollbackReverseRecord,
        #[source]
        source: UsrRollbackReverseSuccessorBindingError,
    },
    #[error("successor binding failed ({binding}) and its canonical record could not be reconciled")]
    SuccessorRecordBindingAndReopen {
        binding: UsrRollbackReverseSuccessorBindingError,
        #[source]
        reopen: UsrRollbackReverseReopenError,
    },
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
