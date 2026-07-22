//! Persist the exact forward ActiveReblit cleanup completion checkpoint.
//!
//! This boundary consumes only durable cleanup authority, derives the sole
//! `CommitCleanupComplete` successor, performs one bound journal advance, and
//! authenticates that successor before and after canonical writer reopen. It
//! performs no cleanup, receipt, database, boot, trigger, retry, or later-phase
//! effect.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    ActiveReblitCommitCleanupDurableAuthority, ActiveReblitCommitCleanupEffectError,
    ActiveReblitCommitCleanupPostAdvanceAuthority, ActiveReblitCommitCleanupRecordAdvanceError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableActiveReblitCommitCleanupRecord {
    CommitDecided,
    CommitCleanupComplete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitCommitCleanupValidationStage {
    SameStore,
    ReopenedOldBinding,
    ReopenedOldBindingAfterFreshCapture,
    ReopenedFreshBinding,
}

enum AdvanceOutcome<'reservation> {
    Published {
        successor_binding: TransitionJournalRecordBinding,
        post_advance_authority: ActiveReblitCommitCleanupPostAdvanceAuthority<'reservation>,
        same_store_validation: Result<(), ActiveReblitCommitCleanupEffectError>,
    },
    StorageFailed(StorageError),
}

/// Persist the sole exact cleanup-complete successor and return only a freshly
/// reopened store which still authenticates the published successor inode.
pub(in crate::client) fn persist_active_reblit_commit_cleanup_complete_and_reopen(
    journal: TransitionJournalStore,
    authority: ActiveReblitCommitCleanupDurableAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), ActiveReblitCommitCleanupPersistenceError> {
    authority
        .revalidate(&journal)
        .map_err(ActiveReblitCommitCleanupPersistenceError::Authority)?;
    let source_record = authority.record().clone();
    let successor = match source_record.forward_successor(None) {
        Ok(successor) if successor.phase == Phase::CommitCleanupComplete => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(ActiveReblitCommitCleanupPersistenceError::UnexpectedSuccessor {
                phase: successor.phase,
            });
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(ActiveReblitCommitCleanupPersistenceError::RouteConstruction { source });
        }
    };

    before_active_reblit_commit_cleanup_final_revalidation();
    let installation = authority.installation().clone();
    let advance = match authority.advance_record_binding(&journal, &successor) {
        Ok((successor_binding, post_advance_authority)) => {
            before_active_reblit_commit_cleanup_same_store_validation();
            let same_store_validation = post_advance_authority.revalidate_successor_same_store(
                &journal,
                &successor_binding,
                &successor,
            );
            AdvanceOutcome::Published {
                successor_binding,
                post_advance_authority,
                same_store_validation,
            }
        }
        Err(ActiveReblitCommitCleanupRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(ActiveReblitCommitCleanupPersistenceError::Authority(source));
        }
        Err(ActiveReblitCommitCleanupRecordAdvanceError::Record(source)) => {
            drop(journal);
            return Err(ActiveReblitCommitCleanupPersistenceError::BoundAdvanceRecord { source });
        }
        Err(ActiveReblitCommitCleanupRecordAdvanceError::UnexpectedSuccessor) => {
            drop(journal);
            return Err(ActiveReblitCommitCleanupPersistenceError::BoundAdvanceUnexpectedSuccessor);
        }
        Err(ActiveReblitCommitCleanupRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(ActiveReblitCommitCleanupPersistenceError::Installation(source));
        }
        Err(ActiveReblitCommitCleanupRecordAdvanceError::Storage(source)) => {
            AdvanceOutcome::StorageFailed(source)
        }
    };

    drop(journal);
    if let AdvanceOutcome::Published { .. } = &advance {
        after_active_reblit_commit_cleanup_same_store_check_before_reopen();
    }
    let reopened = reopen_canonical_journal(&installation)
        .map_err(ActiveReblitCommitCleanupReopenError::from);

    match advance {
        AdvanceOutcome::Published {
            successor_binding,
            post_advance_authority,
            same_store_validation: Ok(()),
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                before_active_reblit_commit_cleanup_reopened_validation();
                if let Err(source) = post_advance_authority.revalidate_successor_reopened(
                    &reopened,
                    &successor_binding,
                    &successor,
                ) {
                    drop(successor_binding);
                    drop(post_advance_authority);
                    drop(reopened);
                    return Err(ActiveReblitCommitCleanupPersistenceError::PostAdvanceValidation {
                        durable: DurableActiveReblitCommitCleanupRecord::CommitCleanupComplete,
                        stage: ActiveReblitCommitCleanupValidationStage::ReopenedOldBinding,
                        source,
                    });
                }
                after_active_reblit_commit_cleanup_old_binding_validation();
                let fresh_binding = match recapture_reopened_successor_binding(
                    &installation,
                    &reopened,
                    &successor,
                ) {
                    Ok(fresh_binding) => fresh_binding,
                    Err(source) => {
                        drop(successor_binding);
                        drop(post_advance_authority);
                        drop(reopened);
                        return Err(ActiveReblitCommitCleanupPersistenceError::FreshSuccessorBinding {
                            source,
                        });
                    }
                };
                if let Err(source) = post_advance_authority.revalidate_successor_reopened(
                    &reopened,
                    &successor_binding,
                    &successor,
                ) {
                    drop(fresh_binding);
                    drop(successor_binding);
                    drop(post_advance_authority);
                    drop(reopened);
                    return Err(ActiveReblitCommitCleanupPersistenceError::PostAdvanceValidation {
                        durable: DurableActiveReblitCommitCleanupRecord::CommitCleanupComplete,
                        stage:
                            ActiveReblitCommitCleanupValidationStage::ReopenedOldBindingAfterFreshCapture,
                        source,
                    });
                }
                drop(successor_binding);
                before_active_reblit_commit_cleanup_fresh_binding_validation();
                let final_validation = post_advance_authority.revalidate_successor_same_store(
                    &reopened,
                    &fresh_binding,
                    &successor,
                );
                drop(fresh_binding);
                drop(post_advance_authority);
                match final_validation {
                    Ok(()) => Ok((reopened, successor)),
                    Err(source) => {
                        drop(reopened);
                        Err(ActiveReblitCommitCleanupPersistenceError::PostAdvanceValidation {
                            durable: DurableActiveReblitCommitCleanupRecord::CommitCleanupComplete,
                            stage: ActiveReblitCommitCleanupValidationStage::ReopenedFreshBinding,
                            source,
                        })
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(ActiveReblitCommitCleanupPersistenceError::ReopenAfterSuccessfulAdvance {
                    source: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(source) => {
                Err(ActiveReblitCommitCleanupPersistenceError::ReopenAfterSuccessfulAdvance { source })
            }
        },
        AdvanceOutcome::Published {
            successor_binding,
            post_advance_authority,
            same_store_validation: Err(validation),
        } => {
            drop(successor_binding);
            drop(post_advance_authority);
            match reopened {
                Ok((reopened, Some(actual))) if actual == source_record => {
                    drop(reopened);
                    Err(ActiveReblitCommitCleanupPersistenceError::PostAdvanceValidation {
                        durable: DurableActiveReblitCommitCleanupRecord::CommitDecided,
                        stage: ActiveReblitCommitCleanupValidationStage::SameStore,
                        source: validation,
                    })
                }
                Ok((reopened, Some(actual))) if actual == successor => {
                    drop(reopened);
                    Err(ActiveReblitCommitCleanupPersistenceError::PostAdvanceValidation {
                        durable: DurableActiveReblitCommitCleanupRecord::CommitCleanupComplete,
                        stage: ActiveReblitCommitCleanupValidationStage::SameStore,
                        source: validation,
                    })
                }
                Ok((reopened, actual)) => {
                    drop(reopened);
                    Err(ActiveReblitCommitCleanupPersistenceError::PostAdvanceValidationAndReopen {
                        stage: ActiveReblitCommitCleanupValidationStage::SameStore,
                        validation,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    })
                }
                Err(reopen) => {
                    Err(ActiveReblitCommitCleanupPersistenceError::PostAdvanceValidationAndReopen {
                        stage: ActiveReblitCommitCleanupValidationStage::SameStore,
                        validation,
                        reopen,
                    })
                }
            }
        }
        AdvanceOutcome::StorageFailed(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(ActiveReblitCommitCleanupPersistenceError::Advance {
                    durable: DurableActiveReblitCommitCleanupRecord::CommitDecided,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(ActiveReblitCommitCleanupPersistenceError::Advance {
                    durable: DurableActiveReblitCommitCleanupRecord::CommitCleanupComplete,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(ActiveReblitCommitCleanupPersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(ActiveReblitCommitCleanupPersistenceError::AdvanceAndReopen {
                advance: advance_error,
                reopen,
            }),
        },
    }
}

fn recapture_reopened_successor_binding(
    installation: &crate::Installation,
    reopened: &TransitionJournalStore,
    successor: &TransitionRecord,
) -> Result<TransitionJournalRecordBinding, ActiveReblitCommitCleanupFreshBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitCommitCleanupFreshBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(ActiveReblitCommitCleanupFreshBindingError::Installation)?;
    let fresh_binding = reopened
        .record_binding(cast, successor)
        .map_err(ActiveReblitCommitCleanupFreshBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitCommitCleanupFreshBindingError::Installation)?;
    Ok(fresh_binding)
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> ActiveReblitCommitCleanupReopenError {
    ActiveReblitCommitCleanupReopenError::UnexpectedRecord {
        expected_commit_decided: Box::new(source.clone()),
        expected_commit_cleanup_complete: Box::new(successor.clone()),
        actual: actual.map(Box::new),
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
    arm_before_active_reblit_commit_cleanup_final_revalidation,
    before_active_reblit_commit_cleanup_final_revalidation,
    BEFORE_FINAL_REVALIDATION
);
test_hook!(
    arm_before_active_reblit_commit_cleanup_same_store_validation,
    before_active_reblit_commit_cleanup_same_store_validation,
    BEFORE_SAME_STORE_VALIDATION
);
test_hook!(
    arm_after_active_reblit_commit_cleanup_same_store_check_before_reopen,
    after_active_reblit_commit_cleanup_same_store_check_before_reopen,
    AFTER_SAME_STORE_BEFORE_REOPEN
);
test_hook!(
    arm_before_active_reblit_commit_cleanup_reopened_validation,
    before_active_reblit_commit_cleanup_reopened_validation,
    BEFORE_REOPENED_VALIDATION
);
test_hook!(
    arm_after_active_reblit_commit_cleanup_old_binding_validation,
    after_active_reblit_commit_cleanup_old_binding_validation,
    AFTER_OLD_BINDING_VALIDATION
);
test_hook!(
    arm_before_active_reblit_commit_cleanup_fresh_binding_validation,
    before_active_reblit_commit_cleanup_fresh_binding_validation,
    BEFORE_FRESH_BINDING_VALIDATION
);

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitCommitCleanupFreshBindingError {
    #[error("revalidate retained installation around fresh reopened cleanup-complete binding capture")]
    Installation(#[source] installation::Error),
    #[error("capture a fresh same-store binding for the reopened CommitCleanupComplete record")]
    Storage(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitCommitCleanupPersistenceError {
    #[error("revalidate exact durable ActiveReblit cleanup authority")]
    Authority(#[source] ActiveReblitCommitCleanupEffectError),
    #[error("derive the sole legal ActiveReblit CommitCleanupComplete successor")]
    RouteConstruction { #[source] source: CodecError },
    #[error("ActiveReblit cleanup routing selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("validate the exact ActiveReblit CommitCleanupComplete successor at the bound advance")]
    BoundAdvanceRecord { #[source] source: CodecError },
    #[error("the bound advance rejected the derived exact CommitCleanupComplete successor")]
    BoundAdvanceUnexpectedSuccessor,
    #[error("revalidate retained installation before the exact cleanup-complete advance")]
    Installation(#[source] installation::Error),
    #[error("cleanup-complete journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableActiveReblitCommitCleanupRecord,
        #[source]
        source: StorageError,
    },
    #[error("cleanup-complete journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: ActiveReblitCommitCleanupReopenError,
    },
    #[error("post-advance {stage:?} validation failed after reopening exact durable {durable:?} record")]
    PostAdvanceValidation {
        durable: DurableActiveReblitCommitCleanupRecord,
        stage: ActiveReblitCommitCleanupValidationStage,
        #[source]
        source: ActiveReblitCommitCleanupEffectError,
    },
    #[error("post-advance {stage:?} validation failed ({validation}) and its canonical record could not be reconciled")]
    PostAdvanceValidationAndReopen {
        stage: ActiveReblitCommitCleanupValidationStage,
        validation: ActiveReblitCommitCleanupEffectError,
        #[source]
        reopen: ActiveReblitCommitCleanupReopenError,
    },
    #[error("capture a fresh binding for the exact reopened CommitCleanupComplete record")]
    FreshSuccessorBinding { #[source] source: ActiveReblitCommitCleanupFreshBindingError },
    #[error("reopen the canonical journal after its CommitCleanupComplete advance succeeded")]
    ReopenAfterSuccessfulAdvance { #[source] source: ActiveReblitCommitCleanupReopenError },
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitCommitCleanupReopenError {
    #[error("revalidate retained installation around cleanup-complete journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical cleanup-complete journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither exact CommitDecided nor CommitCleanupComplete (commit_decided={expected_commit_decided:?}, commit_cleanup_complete={expected_commit_cleanup_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_commit_decided: Box<TransitionRecord>,
        expected_commit_cleanup_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for ActiveReblitCommitCleanupReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
