//! Persist one exact fresh-database invalidation outcome.
//!
//! The supplied authority retains bound joint absence plus exact namespace,
//! journal, plan, installation, and invocation-origin evidence. This boundary
//! revalidates that complete authority twice, derives its sole successor,
//! performs exactly one conditional journal advance, and drops both the
//! authority and old store before canonical reopen. It performs no database
//! invalidation, namespace mutation, later rollback route, trigger, or cleanup.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    UsrRollbackFreshDbInvalidationAuthorityError, UsrRollbackFreshDbInvalidationEffectAuthority,
    UsrRollbackFreshDbInvalidationRecordAdvanceError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

#[cfg(test)]
#[allow(dead_code)] // shared candidate fixture contains wider reconciliation helpers
#[path = "../startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod candidate_test_support;
#[cfg(test)]
#[allow(dead_code, unused_imports)] // shared invalidation fixture contains wider effect helpers
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
pub(in crate::client) enum DurableUsrRollbackFreshDbInvalidationRecord {
    FreshDbInvalidationIntent,
    FreshDbInvalidated,
}

enum UsrRollbackFreshDbInvalidationAdvanceOutcome {
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
        source: UsrRollbackFreshDbInvalidationSuccessorBindingError,
    },
}

/// Persist the sole `FreshDbInvalidated` successor fixed by the effect
/// authority, then independently reopen and compare the complete record.
pub(in crate::client) fn persist_usr_rollback_fresh_db_invalidation_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackFreshDbInvalidationEffectAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackFreshDbInvalidationPersistenceError> {
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackFreshDbInvalidationPersistenceError::Authority(source));
    }
    let source_record = authority.record().clone();

    before_usr_rollback_fresh_db_invalidation_persistence_final_revalidation();
    let installation = authority.installation().clone();
    let advance = match authority.advance_fresh_db_invalidated_record_binding(&journal) {
        Ok(published) => {
            let (successor, successor_binding) = published.into_parts();
            before_usr_rollback_fresh_db_invalidation_successor_binding_revalidation();
            let exact = revalidate_published_fresh_db_invalidated_binding(
                &installation,
                &journal,
                &successor_binding,
                &successor,
            );
            match exact {
                Ok(true) => UsrRollbackFreshDbInvalidationAdvanceOutcome::Published {
                    successor,
                    binding: successor_binding,
                },
                Ok(false) => {
                    drop(successor_binding);
                    UsrRollbackFreshDbInvalidationAdvanceOutcome::SuccessorBindingFailed {
                        successor,
                        source: UsrRollbackFreshDbInvalidationSuccessorBindingError::Changed,
                    }
                }
                Err(source) => {
                    drop(successor_binding);
                    UsrRollbackFreshDbInvalidationAdvanceOutcome::SuccessorBindingFailed { successor, source }
                }
            }
        }
        Err(UsrRollbackFreshDbInvalidationRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(UsrRollbackFreshDbInvalidationPersistenceError::Authority(source));
        }
        Err(UsrRollbackFreshDbInvalidationRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(UsrRollbackFreshDbInvalidationPersistenceError::Installation(source));
        }
        Err(UsrRollbackFreshDbInvalidationRecordAdvanceError::Successor(source)) => {
            drop(journal);
            return Err(UsrRollbackFreshDbInvalidationPersistenceError::SuccessorConstruction { source });
        }
        Err(UsrRollbackFreshDbInvalidationRecordAdvanceError::UnexpectedSuccessor { phase }) => {
            drop(journal);
            return Err(UsrRollbackFreshDbInvalidationPersistenceError::UnexpectedSuccessor { phase });
        }
        Err(UsrRollbackFreshDbInvalidationRecordAdvanceError::Storage { source, successor }) => {
            UsrRollbackFreshDbInvalidationAdvanceOutcome::StorageFailed { successor, source }
        }
    };

    // The evidence authority and exact predecessor binding were consumed by
    // the bound advance. Canonical reopen starts only after the old store is
    // destroyed so its lock and per-open identity cannot be reused.
    drop(journal);

    if let UsrRollbackFreshDbInvalidationAdvanceOutcome::Published { .. } = &advance {
        after_usr_rollback_fresh_db_invalidation_successor_binding_check_before_reopen();
    }
    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackFreshDbInvalidationReopenError::from);
    match advance {
        UsrRollbackFreshDbInvalidationAdvanceOutcome::Published {
            successor,
            binding: successor_binding,
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                let exact = revalidate_reopened_fresh_db_invalidated_binding(
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
                        Err(UsrRollbackFreshDbInvalidationPersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,
                            source: UsrRollbackFreshDbInvalidationSuccessorBindingError::Changed,
                        })
                    }
                    Err(source) => {
                        drop(reopened);
                        Err(UsrRollbackFreshDbInvalidationPersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,
                            source,
                        })
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackFreshDbInvalidationPersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => Err(UsrRollbackFreshDbInvalidationPersistenceError::ReopenAfterSuccessfulAdvance { source }),
        },
        UsrRollbackFreshDbInvalidationAdvanceOutcome::StorageFailed {
            successor,
            source: advance_error,
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackFreshDbInvalidationPersistenceError::Advance {
                    durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidationIntent,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackFreshDbInvalidationPersistenceError::Advance {
                    durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackFreshDbInvalidationPersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackFreshDbInvalidationPersistenceError::AdvanceAndReopen {
                advance: advance_error,
                reopen,
            }),
        },
        UsrRollbackFreshDbInvalidationAdvanceOutcome::SuccessorBindingFailed {
            successor,
            source: binding,
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackFreshDbInvalidationPersistenceError::SuccessorRecordBinding {
                    durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidationIntent,
                    source: binding,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackFreshDbInvalidationPersistenceError::SuccessorRecordBinding {
                    durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,
                    source: binding,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackFreshDbInvalidationPersistenceError::SuccessorRecordBindingAndReopen {
                        binding,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackFreshDbInvalidationPersistenceError::SuccessorRecordBindingAndReopen {
                    binding,
                    reopen,
                },
            ),
        },
    }
}

fn revalidate_published_fresh_db_invalidated_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackFreshDbInvalidationSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackFreshDbInvalidationSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackFreshDbInvalidationSuccessorBindingError::Installation)?;
    let exact = journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackFreshDbInvalidationSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackFreshDbInvalidationSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn revalidate_reopened_fresh_db_invalidated_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackFreshDbInvalidationSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackFreshDbInvalidationSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackFreshDbInvalidationSuccessorBindingError::Installation)?;
    let exact = journal
        .has_reopened_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackFreshDbInvalidationSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackFreshDbInvalidationSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackFreshDbInvalidationReopenError {
    UsrRollbackFreshDbInvalidationReopenError::UnexpectedRecord {
        expected_fresh_db_invalidation_intent: Box::new(source.clone()),
        expected_fresh_db_invalidated: Box::new(successor.clone()),
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
pub(crate) fn arm_before_usr_rollback_fresh_db_invalidation_persistence_final_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_fresh_db_invalidation_persistence_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_fresh_db_invalidation_persistence_final_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_fresh_db_invalidation_successor_binding_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_fresh_db_invalidation_successor_binding_revalidation() {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_fresh_db_invalidation_successor_binding_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_after_usr_rollback_fresh_db_invalidation_successor_binding_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_usr_rollback_fresh_db_invalidation_successor_binding_check_before_reopen() {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_fresh_db_invalidation_successor_binding_check_before_reopen() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackFreshDbInvalidationSuccessorBindingError {
    #[error("revalidate retained installation after publishing FreshDbInvalidated")]
    Installation(#[source] installation::Error),
    #[error("the published FreshDbInvalidated successor lost its exact record binding")]
    Changed,
    #[error("revalidate the published FreshDbInvalidated successor record binding")]
    Storage(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackFreshDbInvalidationPersistenceError {
    #[error("revalidate exact fresh-database invalidation effect authority")]
    Authority(#[source] UsrRollbackFreshDbInvalidationAuthorityError),
    #[error("revalidate retained installation before the exact FreshDbInvalidated record advance")]
    Installation(#[source] installation::Error),
    #[error("derive the sole legal FreshDbInvalidated successor")]
    SuccessorConstruction {
        #[source]
        source: CodecError,
    },
    #[error("fresh-database invalidation authority selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackFreshDbInvalidationRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its FreshDbInvalidated advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackFreshDbInvalidationReopenError,
    },
    #[error("journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackFreshDbInvalidationReopenError,
    },
    #[error("published journal binding failed after reopening exact durable {durable:?} record")]
    SuccessorRecordBinding {
        durable: DurableUsrRollbackFreshDbInvalidationRecord,
        #[source]
        source: UsrRollbackFreshDbInvalidationSuccessorBindingError,
    },
    #[error("published journal binding failed ({binding}) and its canonical record could not be reconciled")]
    SuccessorRecordBindingAndReopen {
        binding: UsrRollbackFreshDbInvalidationSuccessorBindingError,
        #[source]
        reopen: UsrRollbackFreshDbInvalidationReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackFreshDbInvalidationReopenError {
    #[error("revalidate retained installation around journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact FreshDbInvalidationIntent nor FreshDbInvalidated record (fresh_db_invalidation_intent={expected_fresh_db_invalidation_intent:?}, fresh_db_invalidated={expected_fresh_db_invalidated:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_fresh_db_invalidation_intent: Box<TransitionRecord>,
        expected_fresh_db_invalidated: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackFreshDbInvalidationReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
