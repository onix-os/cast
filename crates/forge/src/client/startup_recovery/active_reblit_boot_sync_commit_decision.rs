//! Persist the exact forward ActiveReblit transition from
//! `BootSyncComplete` to `CommitDecided`.
//!
//! The supplied authority retains the promoted boot-publication receipt,
//! cleared state/database provenance, complete state, active selection,
//! namespace shape, installation, and exact source journal binding. This
//! boundary derives the sole typed successor, performs exactly one bound
//! journal advance, authenticates the successor through the retained
//! post-advance authority, drops the old lock-bearing store handle, and reopens
//! the canonical journal before returning. It performs no database,
//! namespace, boot, cleanup, trigger, device, retry, or later-phase effect.

use thiserror::Error;

use crate::{
    client::active_reblit_boot_publication_preflight::{
        ActiveReblitBootCommitDecisionFinalValidation,
        ActiveReblitBootPostCompletionValidationError,
    },
    installation,
    transition_journal::{CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{
    ActiveReblitBootSyncCompleteAuthority, ActiveReblitBootSyncCompleteAuthorityError,
    ActiveReblitBootSyncCompletePostAdvanceAuthority, ActiveReblitBootSyncCompleteRecordAdvanceError,
};
use super::canonical_journal_reopen::{
    CanonicalJournalReopenError, try_reopen_canonical_journal,
};

/// Which exact canonical record survived an uncertain or rejected advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableActiveReblitBootSyncCommitDecisionRecord {
    BootSyncComplete,
    CommitDecided,
}

/// Which post-advance authentication boundary rejected the successor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitBootSyncCommitDecisionValidationStage {
    SameStore,
    ReopenedOldBinding,
    ReopenedOldBindingAfterFreshCapture,
    ReopenedFreshBinding,
}

enum ActiveReblitBootSyncCommitDecisionAdvanceOutcome<'reservation> {
    Published {
        successor_binding: TransitionJournalRecordBinding,
        post_advance_authority: ActiveReblitBootSyncCompletePostAdvanceAuthority<'reservation>,
        same_store_validation: Result<(), ActiveReblitBootSyncCompleteAuthorityError>,
    },
    StorageFailed(StorageError),
}

/// Persist the sole exact `CommitDecided` successor and return only a freshly
/// reopened store which still authenticates the published successor inode.
pub(in crate::client) fn persist_active_reblit_boot_sync_commit_decision_and_reopen(
    journal: TransitionJournalStore,
    authority: ActiveReblitBootSyncCompleteAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), ActiveReblitBootSyncCommitDecisionPersistenceError> {
    let (journal, record, binding) =
        persist_active_reblit_boot_sync_commit_decision_inner(journal, authority, None)?;
    drop(binding);
    Ok((journal, record))
}

/// Common persistence boundary for callers which must retain the fresh
/// successor binding with the reopened journal.
pub(in crate::client) fn persist_active_reblit_boot_sync_commit_decision_retaining_binding(
    journal: TransitionJournalStore,
    authority: ActiveReblitBootSyncCompleteAuthority<'_>,
    final_validation: ActiveReblitBootCommitDecisionFinalValidation<'_>,
) -> Result<
    (
        TransitionJournalStore,
        TransitionRecord,
        TransitionJournalRecordBinding,
    ),
    ActiveReblitBootSyncCommitDecisionPersistenceError,
> {
    persist_active_reblit_boot_sync_commit_decision_inner(
        journal,
        authority,
        Some(final_validation),
    )
}

fn persist_active_reblit_boot_sync_commit_decision_inner(
    journal: TransitionJournalStore,
    authority: ActiveReblitBootSyncCompleteAuthority<'_>,
    final_validation: Option<ActiveReblitBootCommitDecisionFinalValidation<'_>>,
) -> Result<
    (
        TransitionJournalStore,
        TransitionRecord,
        TransitionJournalRecordBinding,
    ),
    ActiveReblitBootSyncCommitDecisionPersistenceError,
> {
    authority
        .revalidate(&journal)
        .map_err(ActiveReblitBootSyncCommitDecisionPersistenceError::Authority)?;
    let source_record = authority.record().clone();
    let successor = match source_record.forward_successor(None) {
        Ok(successor) if successor.phase == Phase::CommitDecided => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(ActiveReblitBootSyncCommitDecisionPersistenceError::UnexpectedSuccessor {
                phase: successor.phase,
            });
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(ActiveReblitBootSyncCommitDecisionPersistenceError::RouteConstruction { source });
        }
    };

    before_active_reblit_boot_sync_commit_decision_final_revalidation();
    let installation = authority.installation().clone();
    let advance_result = match final_validation {
        Some(final_validation) => authority.advance_record_binding_after_final_validation(
            &journal,
            &successor,
            final_validation,
        ),
        None => authority.advance_record_binding(&journal, &successor),
    };
    let advance = match advance_result {
        Ok((successor_binding, post_advance_authority)) => {
            before_active_reblit_boot_sync_commit_decision_same_store_validation();
            let same_store_validation = post_advance_authority.revalidate_successor_same_store(
                &journal,
                &successor_binding,
                &successor,
            );
            ActiveReblitBootSyncCommitDecisionAdvanceOutcome::Published {
                successor_binding,
                post_advance_authority,
                same_store_validation,
            }
        }
        Err(ActiveReblitBootSyncCompleteRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(ActiveReblitBootSyncCommitDecisionPersistenceError::Authority(source));
        }
        Err(ActiveReblitBootSyncCompleteRecordAdvanceError::Record(source)) => {
            drop(journal);
            return Err(ActiveReblitBootSyncCommitDecisionPersistenceError::BoundAdvanceRecord { source });
        }
        Err(ActiveReblitBootSyncCompleteRecordAdvanceError::UnexpectedSuccessor) => {
            drop(journal);
            return Err(ActiveReblitBootSyncCommitDecisionPersistenceError::BoundAdvanceUnexpectedSuccessor);
        }
        Err(ActiveReblitBootSyncCompleteRecordAdvanceError::FinalTerminalValidation(source)) => {
            drop(journal);
            return Err(
                ActiveReblitBootSyncCommitDecisionPersistenceError::FinalTerminalValidation(
                    source,
                ),
            );
        }
        Err(ActiveReblitBootSyncCompleteRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(ActiveReblitBootSyncCommitDecisionPersistenceError::Installation(source));
        }
        Err(ActiveReblitBootSyncCompleteRecordAdvanceError::Storage(source)) => {
            ActiveReblitBootSyncCommitDecisionAdvanceOutcome::StorageFailed(source)
        }
    };

    // The source binding was consumed by the bound advance. Keep the returned
    // successor binding and post-advance authority, but drop this old store
    // before canonical reopen so its writer lock and per-open identity cannot
    // accidentally authorize a second action.
    drop(journal);

    if let ActiveReblitBootSyncCommitDecisionAdvanceOutcome::Published { .. } = &advance {
        after_active_reblit_boot_sync_commit_decision_same_store_check_before_reopen();
    }
    let reopened = try_reopen_canonical_journal(&installation)
        .map_err(ActiveReblitBootSyncCommitDecisionReopenError::from);

    match advance {
        ActiveReblitBootSyncCommitDecisionAdvanceOutcome::Published {
            successor_binding,
            post_advance_authority,
            same_store_validation: Ok(()),
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                before_active_reblit_boot_sync_commit_decision_reopened_validation();
                let reopened_validation = post_advance_authority.revalidate_successor_reopened(
                    &reopened,
                    &successor_binding,
                    &successor,
                );
                match reopened_validation {
                    Ok(()) => {
                        after_active_reblit_boot_sync_commit_decision_old_binding_validation();
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
                                return Err(
                                    ActiveReblitBootSyncCommitDecisionPersistenceError::FreshSuccessorBinding {
                                        source,
                                    },
                                );
                            }
                        };
                        let old_binding_revalidation = post_advance_authority.revalidate_successor_reopened(
                            &reopened,
                            &successor_binding,
                            &successor,
                        );
                        if let Err(source) = old_binding_revalidation {
                            drop(fresh_binding);
                            drop(successor_binding);
                            drop(post_advance_authority);
                            drop(reopened);
                            return Err(
                                ActiveReblitBootSyncCommitDecisionPersistenceError::PostAdvanceValidation {
                                    durable: DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided,
                                    stage:
                                        ActiveReblitBootSyncCommitDecisionValidationStage::ReopenedOldBindingAfterFreshCapture,
                                    source,
                                },
                            );
                        }
                        drop(successor_binding);
                        before_active_reblit_boot_sync_commit_decision_fresh_binding_validation();
                        let final_validation = post_advance_authority.revalidate_successor_same_store(
                            &reopened,
                            &fresh_binding,
                            &successor,
                        );
                        drop(post_advance_authority);
                        match final_validation {
                            Ok(()) => Ok((reopened, successor, fresh_binding)),
                            Err(source) => {
                                drop(fresh_binding);
                                drop(reopened);
                                Err(
                                    ActiveReblitBootSyncCommitDecisionPersistenceError::PostAdvanceValidation {
                                        durable: DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided,
                                        stage: ActiveReblitBootSyncCommitDecisionValidationStage::ReopenedFreshBinding,
                                        source,
                                    },
                                )
                            }
                        }
                    }
                    Err(source) => {
                        drop(successor_binding);
                        drop(post_advance_authority);
                        drop(reopened);
                        Err(ActiveReblitBootSyncCommitDecisionPersistenceError::PostAdvanceValidation {
                            durable: DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided,
                            stage: ActiveReblitBootSyncCommitDecisionValidationStage::ReopenedOldBinding,
                            source,
                        })
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(ActiveReblitBootSyncCommitDecisionPersistenceError::ReopenAfterSuccessfulAdvance {
                    source: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(source) => {
                Err(ActiveReblitBootSyncCommitDecisionPersistenceError::ReopenAfterSuccessfulAdvance { source })
            }
        },
        ActiveReblitBootSyncCommitDecisionAdvanceOutcome::Published {
            successor_binding,
            post_advance_authority,
            same_store_validation: Err(validation),
        } => {
            drop(successor_binding);
            drop(post_advance_authority);
            match reopened {
                Ok((reopened, Some(actual))) if actual == source_record => {
                    drop(reopened);
                    Err(ActiveReblitBootSyncCommitDecisionPersistenceError::PostAdvanceValidation {
                        durable: DurableActiveReblitBootSyncCommitDecisionRecord::BootSyncComplete,
                        stage: ActiveReblitBootSyncCommitDecisionValidationStage::SameStore,
                        source: validation,
                    })
                }
                Ok((reopened, Some(actual))) if actual == successor => {
                    drop(reopened);
                    Err(ActiveReblitBootSyncCommitDecisionPersistenceError::PostAdvanceValidation {
                        durable: DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided,
                        stage: ActiveReblitBootSyncCommitDecisionValidationStage::SameStore,
                        source: validation,
                    })
                }
                Ok((reopened, actual)) => {
                    drop(reopened);
                    Err(
                        ActiveReblitBootSyncCommitDecisionPersistenceError::PostAdvanceValidationAndReopen {
                            stage: ActiveReblitBootSyncCommitDecisionValidationStage::SameStore,
                            validation,
                            reopen: unexpected_record(&source_record, &successor, actual),
                        },
                    )
                }
                Err(reopen) => Err(
                    ActiveReblitBootSyncCommitDecisionPersistenceError::PostAdvanceValidationAndReopen {
                        stage: ActiveReblitBootSyncCommitDecisionValidationStage::SameStore,
                        validation,
                        reopen,
                    },
                ),
            }
        }
        ActiveReblitBootSyncCommitDecisionAdvanceOutcome::StorageFailed(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(ActiveReblitBootSyncCommitDecisionPersistenceError::Advance {
                    durable: DurableActiveReblitBootSyncCommitDecisionRecord::BootSyncComplete,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(ActiveReblitBootSyncCommitDecisionPersistenceError::Advance {
                    durable: DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(ActiveReblitBootSyncCommitDecisionPersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(ActiveReblitBootSyncCommitDecisionPersistenceError::AdvanceAndReopen {
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
) -> Result<TransitionJournalRecordBinding, ActiveReblitBootSyncCommitDecisionFreshBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncCommitDecisionFreshBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(ActiveReblitBootSyncCommitDecisionFreshBindingError::Installation)?;
    let fresh_binding = reopened
        .record_binding(cast, successor)
        .map_err(ActiveReblitBootSyncCommitDecisionFreshBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(ActiveReblitBootSyncCommitDecisionFreshBindingError::Installation)?;
    Ok(fresh_binding)
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> ActiveReblitBootSyncCommitDecisionReopenError {
    ActiveReblitBootSyncCommitDecisionReopenError::UnexpectedRecord {
        expected_boot_sync_complete: Box::new(source.clone()),
        expected_commit_decided: Box::new(successor.clone()),
        actual: actual.map(Box::new),
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_AUTHORITY_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_SAME_STORE_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_SAME_STORE_CHECK_BEFORE_REOPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_REOPENED_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_FRESH_BINDING_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_OLD_REOPENED_VALIDATION_BEFORE_FRESH_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_boot_sync_commit_decision_final_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_active_reblit_boot_sync_commit_decision_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_active_reblit_boot_sync_commit_decision_final_revalidation() {}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_boot_sync_commit_decision_same_store_validation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_SAME_STORE_VALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_active_reblit_boot_sync_commit_decision_same_store_validation() {
    BEFORE_SAME_STORE_VALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_active_reblit_boot_sync_commit_decision_same_store_validation() {}

#[cfg(test)]
pub(in crate::client) fn arm_after_active_reblit_boot_sync_commit_decision_same_store_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SAME_STORE_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_active_reblit_boot_sync_commit_decision_same_store_check_before_reopen() {
    AFTER_SAME_STORE_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_active_reblit_boot_sync_commit_decision_same_store_check_before_reopen() {}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_boot_sync_commit_decision_reopened_validation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_REOPENED_VALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_active_reblit_boot_sync_commit_decision_reopened_validation() {
    BEFORE_REOPENED_VALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_active_reblit_boot_sync_commit_decision_reopened_validation() {}

#[cfg(test)]
pub(in crate::client) fn arm_after_active_reblit_boot_sync_commit_decision_old_binding_validation(
    hook: impl FnOnce() + 'static,
) {
    AFTER_OLD_REOPENED_VALIDATION_BEFORE_FRESH_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_active_reblit_boot_sync_commit_decision_old_binding_validation() {
    AFTER_OLD_REOPENED_VALIDATION_BEFORE_FRESH_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_active_reblit_boot_sync_commit_decision_old_binding_validation() {}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_boot_sync_commit_decision_fresh_binding_validation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FRESH_BINDING_VALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_active_reblit_boot_sync_commit_decision_fresh_binding_validation() {
    BEFORE_FRESH_BINDING_VALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_active_reblit_boot_sync_commit_decision_fresh_binding_validation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncCommitDecisionFreshBindingError {
    #[error("revalidate retained installation around fresh reopened CommitDecided binding capture")]
    Installation(#[source] installation::Error),
    #[error("capture a fresh same-store binding for the reopened CommitDecided record")]
    Storage(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncCommitDecisionPersistenceError {
    #[error("revalidate exact ActiveReblit BootSyncComplete startup authority")]
    Authority(#[source] ActiveReblitBootSyncCompleteAuthorityError),
    #[error("derive the sole legal ActiveReblit CommitDecided successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("ActiveReblit boot-sync commit routing selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("validate the exact ActiveReblit CommitDecided successor at the bound advance")]
    BoundAdvanceRecord {
        #[source]
        source: CodecError,
    },
    #[error("the bound advance rejected the derived exact ActiveReblit CommitDecided successor")]
    BoundAdvanceUnexpectedSuccessor,
    #[error("repeat exact terminal output validation at the bound journal advance")]
    FinalTerminalValidation(
        #[source]
        ActiveReblitBootPostCompletionValidationError,
    ),
    #[error("revalidate retained installation before the exact ActiveReblit CommitDecided advance")]
    Installation(#[source] installation::Error),
    #[error("ActiveReblit commit-decision journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableActiveReblitBootSyncCommitDecisionRecord,
        #[source]
        source: StorageError,
    },
    #[error("ActiveReblit commit-decision journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: ActiveReblitBootSyncCommitDecisionReopenError,
    },
    #[error("post-advance {stage:?} validation failed after reopening exact durable {durable:?} record")]
    PostAdvanceValidation {
        durable: DurableActiveReblitBootSyncCommitDecisionRecord,
        stage: ActiveReblitBootSyncCommitDecisionValidationStage,
        #[source]
        source: ActiveReblitBootSyncCompleteAuthorityError,
    },
    #[error("post-advance {stage:?} validation failed ({validation}) and its canonical record could not be reconciled")]
    PostAdvanceValidationAndReopen {
        stage: ActiveReblitBootSyncCommitDecisionValidationStage,
        validation: ActiveReblitBootSyncCompleteAuthorityError,
        #[source]
        reopen: ActiveReblitBootSyncCommitDecisionReopenError,
    },
    #[error("capture a fresh binding for the exact reopened ActiveReblit CommitDecided record")]
    FreshSuccessorBinding {
        #[source]
        source: ActiveReblitBootSyncCommitDecisionFreshBindingError,
    },
    #[error("reopen the canonical journal after its ActiveReblit CommitDecided advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: ActiveReblitBootSyncCommitDecisionReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncCommitDecisionReopenError {
    #[error("revalidate retained installation around ActiveReblit commit-decision journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical ActiveReblit commit-decision journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact ActiveReblit BootSyncComplete nor CommitDecided record (boot_sync_complete={expected_boot_sync_complete:?}, commit_decided={expected_commit_decided:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_boot_sync_complete: Box<TransitionRecord>,
        expected_commit_decided: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for ActiveReblitBootSyncCommitDecisionReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
