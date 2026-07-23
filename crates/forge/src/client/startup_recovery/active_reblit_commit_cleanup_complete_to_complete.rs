//! Persist exact promoted ActiveReblit `CommitCleanupComplete` authority to
//! `Complete` and authenticate the successor across canonical writer reopen.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{
        CodecError, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::canonical_journal_reopen::{
    CanonicalJournalReopenError, reopen_canonical_journal, try_reopen_canonical_journal,
};
use super::super::startup_reconciliation::{
    ActiveReblitCommitCleanupCompleteAuthorityError,
    ActiveReblitCommitCleanupCompleteAuthority,
    ActiveReblitCommitCleanupCompletePostAdvanceAuthority,
    ActiveReblitCommitCleanupCompleteRecordAdvanceError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableActiveReblitCommitCleanupCompleteRecord {
    CommitCleanupComplete,
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitCommitCleanupCompleteValidationStage {
    SameStore,
    ReopenedOldBinding,
    ReopenedOldBindingAfterFreshCapture,
    ReopenedFreshBinding,
}

enum AdvanceOutcome<'reservation> {
    Published {
        successor: TransitionRecord,
        successor_binding: TransitionJournalRecordBinding,
        post_advance: ActiveReblitCommitCleanupCompletePostAdvanceAuthority<'reservation>,
        same_store_validation: Result<(), ActiveReblitCommitCleanupCompleteAuthorityError>,
    },
    StorageFailed {
        source: StorageError,
        successor: TransitionRecord,
    },
}

#[derive(Clone, Copy)]
enum CanonicalReopenMode {
    StartupBlocking,
    RetainedNonBlocking,
}

pub(in crate::client) fn persist_active_reblit_commit_cleanup_complete_to_complete_and_reopen(
    journal: TransitionJournalStore,
    authority: ActiveReblitCommitCleanupCompleteAuthority<'_>,
) -> Result<
    (TransitionJournalStore, TransitionRecord),
    ActiveReblitCommitCleanupCompletePersistenceError,
> {
    let (journal, record, binding) =
        persist_active_reblit_commit_cleanup_complete_to_complete_inner(
            journal,
            authority,
            CanonicalReopenMode::StartupBlocking,
        )?;
    drop(binding);
    Ok((journal, record))
}

/// Persist exact live cleanup completion while returning the fresh terminal
/// binding required by continuous coordinator ownership.
pub(in crate::client) fn persist_active_reblit_commit_cleanup_complete_to_complete_retaining_binding(
    journal: TransitionJournalStore,
    authority: ActiveReblitCommitCleanupCompleteAuthority<'_>,
) -> Result<
    (
        TransitionJournalStore,
        TransitionRecord,
        TransitionJournalRecordBinding,
    ),
    ActiveReblitCommitCleanupCompletePersistenceError,
> {
    persist_active_reblit_commit_cleanup_complete_to_complete_inner(
        journal,
        authority,
        CanonicalReopenMode::RetainedNonBlocking,
    )
}

fn persist_active_reblit_commit_cleanup_complete_to_complete_inner(
    journal: TransitionJournalStore,
    authority: ActiveReblitCommitCleanupCompleteAuthority<'_>,
    reopen_mode: CanonicalReopenMode,
) -> Result<
    (
        TransitionJournalStore,
        TransitionRecord,
        TransitionJournalRecordBinding,
    ),
    ActiveReblitCommitCleanupCompletePersistenceError,
> {
    let source_record = authority.record().clone();
    let installation = authority.installation().clone();
    before_active_reblit_commit_cleanup_complete_final_revalidation();
    let advance = match authority.advance_to_complete(&journal) {
        Ok((successor, successor_binding, post_advance)) => {
            before_active_reblit_commit_cleanup_complete_same_store_validation();
            let same_store_validation = post_advance.revalidate_successor_same_store(
                &journal,
                &successor_binding,
                &successor,
            );
            AdvanceOutcome::Published {
                successor,
                successor_binding,
                post_advance,
                same_store_validation,
            }
        }
        Err(ActiveReblitCommitCleanupCompleteRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(ActiveReblitCommitCleanupCompletePersistenceError::Authority(source));
        }
        Err(ActiveReblitCommitCleanupCompleteRecordAdvanceError::Record(source)) => {
            drop(journal);
            return Err(ActiveReblitCommitCleanupCompletePersistenceError::RouteConstruction {
                source,
            });
        }
        Err(ActiveReblitCommitCleanupCompleteRecordAdvanceError::UnexpectedSuccessor) => {
            drop(journal);
            return Err(
                ActiveReblitCommitCleanupCompletePersistenceError::UnexpectedSuccessor,
            );
        }
        Err(ActiveReblitCommitCleanupCompleteRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(ActiveReblitCommitCleanupCompletePersistenceError::Installation(source));
        }
        Err(ActiveReblitCommitCleanupCompleteRecordAdvanceError::Storage {
            source,
            successor,
        }) => AdvanceOutcome::StorageFailed {
            source,
            successor: *successor,
        },
    };

    drop(journal);
    if matches!(advance, AdvanceOutcome::Published { .. }) {
        after_active_reblit_commit_cleanup_complete_same_store_before_reopen();
    }
    let reopened = match reopen_mode {
        CanonicalReopenMode::StartupBlocking => reopen_canonical_journal(&installation),
        CanonicalReopenMode::RetainedNonBlocking => try_reopen_canonical_journal(&installation),
    }
        .map_err(ActiveReblitCommitCleanupCompleteReopenError::from);

    match advance {
        AdvanceOutcome::Published {
            successor,
            successor_binding,
            post_advance,
            same_store_validation: Ok(()),
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                before_active_reblit_commit_cleanup_complete_reopened_validation();
                if let Err(source) = post_advance.revalidate_successor_reopened(
                    &reopened,
                    &successor_binding,
                    &successor,
                ) {
                    drop(successor_binding);
                    drop(post_advance);
                    drop(reopened);
                    return Err(
                        ActiveReblitCommitCleanupCompletePersistenceError::PostAdvanceValidation {
                            durable: DurableActiveReblitCommitCleanupCompleteRecord::Complete,
                            stage: ActiveReblitCommitCleanupCompleteValidationStage::ReopenedOldBinding,
                            source,
                        },
                    );
                }
                after_active_reblit_commit_cleanup_complete_old_binding_validation();
                let fresh_binding = match recapture_successor_binding(
                    &installation,
                    &reopened,
                    &successor,
                ) {
                    Ok(binding) => binding,
                    Err(source) => {
                        drop(successor_binding);
                        drop(post_advance);
                        drop(reopened);
                        return Err(
                            ActiveReblitCommitCleanupCompletePersistenceError::FreshSuccessorBinding {
                                source,
                            },
                        );
                    }
                };
                if let Err(source) = post_advance.revalidate_successor_reopened(
                    &reopened,
                    &successor_binding,
                    &successor,
                ) {
                    drop(fresh_binding);
                    drop(successor_binding);
                    drop(post_advance);
                    drop(reopened);
                    return Err(
                        ActiveReblitCommitCleanupCompletePersistenceError::PostAdvanceValidation {
                            durable: DurableActiveReblitCommitCleanupCompleteRecord::Complete,
                            stage: ActiveReblitCommitCleanupCompleteValidationStage::ReopenedOldBindingAfterFreshCapture,
                            source,
                        },
                    );
                }
                drop(successor_binding);
                before_active_reblit_commit_cleanup_complete_fresh_binding_validation();
                let validation = post_advance.revalidate_successor_same_store(
                    &reopened,
                    &fresh_binding,
                    &successor,
                );
                drop(post_advance);
                match validation {
                    Ok(()) => Ok((reopened, successor, fresh_binding)),
                    Err(source) => {
                        drop(fresh_binding);
                        drop(reopened);
                        Err(
                            ActiveReblitCommitCleanupCompletePersistenceError::PostAdvanceValidation {
                                durable: DurableActiveReblitCommitCleanupCompleteRecord::Complete,
                                stage: ActiveReblitCommitCleanupCompleteValidationStage::ReopenedFreshBinding,
                                source,
                            },
                        )
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    ActiveReblitCommitCleanupCompletePersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => Err(
                ActiveReblitCommitCleanupCompletePersistenceError::ReopenAfterSuccessfulAdvance {
                    source,
                },
            ),
        },
        AdvanceOutcome::Published {
            successor,
            successor_binding,
            post_advance,
            same_store_validation: Err(validation),
        } => {
            drop(successor_binding);
            drop(post_advance);
            match reopened {
                Ok((reopened, Some(actual))) if actual == source_record => {
                    drop(reopened);
                    Err(
                        ActiveReblitCommitCleanupCompletePersistenceError::PostAdvanceValidation {
                            durable: DurableActiveReblitCommitCleanupCompleteRecord::CommitCleanupComplete,
                            stage: ActiveReblitCommitCleanupCompleteValidationStage::SameStore,
                            source: validation,
                        },
                    )
                }
                Ok((reopened, Some(actual))) if actual == successor => {
                    drop(reopened);
                    Err(
                        ActiveReblitCommitCleanupCompletePersistenceError::PostAdvanceValidation {
                            durable: DurableActiveReblitCommitCleanupCompleteRecord::Complete,
                            stage: ActiveReblitCommitCleanupCompleteValidationStage::SameStore,
                            source: validation,
                        },
                    )
                }
                Ok((reopened, actual)) => {
                    drop(reopened);
                    Err(
                        ActiveReblitCommitCleanupCompletePersistenceError::PostAdvanceValidationAndReopen {
                            validation,
                            reopen: unexpected_record(&source_record, &successor, actual),
                        },
                    )
                }
                Err(reopen) => Err(
                    ActiveReblitCommitCleanupCompletePersistenceError::PostAdvanceValidationAndReopen {
                        validation,
                        reopen,
                    },
                ),
            }
        }
        AdvanceOutcome::StorageFailed { source, successor } => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(ActiveReblitCommitCleanupCompletePersistenceError::Advance {
                    durable: DurableActiveReblitCommitCleanupCompleteRecord::CommitCleanupComplete,
                    source,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(ActiveReblitCommitCleanupCompletePersistenceError::Advance {
                    durable: DurableActiveReblitCommitCleanupCompleteRecord::Complete,
                    source,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    ActiveReblitCommitCleanupCompletePersistenceError::AdvanceAndReopen {
                        advance: source,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                ActiveReblitCommitCleanupCompletePersistenceError::AdvanceAndReopen {
                    advance: source,
                    reopen,
                },
            ),
        },
    }
}

fn recapture_successor_binding(
    installation: &crate::Installation,
    reopened: &TransitionJournalStore,
    successor: &TransitionRecord,
) -> Result<TransitionJournalRecordBinding, ActiveReblitCommitCleanupCompleteFreshBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitCommitCleanupCompleteFreshBindingError::Installation)?;
    let binding = reopened
        .record_binding(
            installation
                .retained_mutable_cast_directory()
                .map_err(ActiveReblitCommitCleanupCompleteFreshBindingError::Installation)?,
            successor,
        )
        .map_err(ActiveReblitCommitCleanupCompleteFreshBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitCommitCleanupCompleteFreshBindingError::Installation)?;
    Ok(binding)
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> ActiveReblitCommitCleanupCompleteReopenError {
    ActiveReblitCommitCleanupCompleteReopenError::UnexpectedRecord {
        expected_commit_cleanup_complete: Box::new(source.clone()),
        expected_complete: Box::new(successor.clone()),
        actual: actual.map(Box::new),
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitCommitCleanupCompleteFreshBindingError {
    #[error("revalidate installation around fresh Complete binding capture")]
    Installation(#[source] installation::Error),
    #[error("capture fresh same-store binding for reopened Complete record")]
    Storage(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitCommitCleanupCompletePersistenceError {
    #[error("revalidate exact promoted CommitCleanupComplete authority")]
    Authority(#[source] ActiveReblitCommitCleanupCompleteAuthorityError),
    #[error("derive sole legal ActiveReblit Complete successor")]
    RouteConstruction { #[source] source: CodecError },
    #[error("derived successor was not exact ActiveReblit Complete")]
    UnexpectedSuccessor,
    #[error("revalidate installation before bound Complete advance")]
    Installation(#[source] installation::Error),
    #[error("Complete journal advance failed after reopening exact durable {durable:?}")]
    Advance {
        durable: DurableActiveReblitCommitCleanupCompleteRecord,
        #[source]
        source: StorageError,
    },
    #[error("Complete journal advance failed and canonical reopen was inconclusive")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: ActiveReblitCommitCleanupCompleteReopenError,
    },
    #[error("post-advance {stage:?} validation failed after reopening exact {durable:?}")]
    PostAdvanceValidation {
        durable: DurableActiveReblitCommitCleanupCompleteRecord,
        stage: ActiveReblitCommitCleanupCompleteValidationStage,
        #[source]
        source: ActiveReblitCommitCleanupCompleteAuthorityError,
    },
    #[error("same-store validation failed and canonical reopen was inconclusive")]
    PostAdvanceValidationAndReopen {
        validation: ActiveReblitCommitCleanupCompleteAuthorityError,
        #[source]
        reopen: ActiveReblitCommitCleanupCompleteReopenError,
    },
    #[error("capture fresh binding for exact reopened Complete record")]
    FreshSuccessorBinding {
        #[source]
        source: ActiveReblitCommitCleanupCompleteFreshBindingError,
    },
    #[error("reopen canonical journal after Complete advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: ActiveReblitCommitCleanupCompleteReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitCommitCleanupCompleteReopenError {
    #[error("revalidate installation around Complete journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load canonical Complete journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened journal is neither exact CommitCleanupComplete nor Complete (source={expected_commit_cleanup_complete:?}, complete={expected_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_commit_cleanup_complete: Box<TransitionRecord>,
        expected_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for ActiveReblitCommitCleanupCompleteReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_SAME_STORE_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static AFTER_SAME_STORE_BEFORE_REOPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_REOPENED_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static AFTER_OLD_BINDING_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_FRESH_BINDING_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

macro_rules! test_hook {
    ($arm:ident, $run:ident, $slot:ident) => {
        #[cfg(test)]
        pub(in crate::client) fn $arm(hook: impl FnOnce() + 'static) {
            $slot.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
        }
        #[cfg(test)]
        fn $run() {
            $slot.with(|slot| {
                if let Some(hook) = slot.borrow_mut().take() {
                    hook();
                }
            });
        }
        #[cfg(not(test))]
        fn $run() {}
    };
}

test_hook!(
    arm_before_active_reblit_commit_cleanup_complete_final_revalidation,
    before_active_reblit_commit_cleanup_complete_final_revalidation,
    BEFORE_FINAL_REVALIDATION
);
test_hook!(
    arm_before_active_reblit_commit_cleanup_complete_same_store_validation,
    before_active_reblit_commit_cleanup_complete_same_store_validation,
    BEFORE_SAME_STORE_VALIDATION
);
test_hook!(
    arm_after_active_reblit_commit_cleanup_complete_same_store_before_reopen,
    after_active_reblit_commit_cleanup_complete_same_store_before_reopen,
    AFTER_SAME_STORE_BEFORE_REOPEN
);
test_hook!(
    arm_before_active_reblit_commit_cleanup_complete_reopened_validation,
    before_active_reblit_commit_cleanup_complete_reopened_validation,
    BEFORE_REOPENED_VALIDATION
);
test_hook!(
    arm_after_active_reblit_commit_cleanup_complete_old_binding_validation,
    after_active_reblit_commit_cleanup_complete_old_binding_validation,
    AFTER_OLD_BINDING_VALIDATION
);
test_hook!(
    arm_before_active_reblit_commit_cleanup_complete_fresh_binding_validation,
    before_active_reblit_commit_cleanup_complete_fresh_binding_validation,
    BEFORE_FRESH_BINDING_VALIDATION
);
