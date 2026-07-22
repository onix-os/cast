//! Persist the exact forward ActiveReblit transition from
//! `BootSyncStarted` to `BootSyncComplete` during startup recovery.
//!
//! The supplied authority retains the promoted boot-publication receipt,
//! cleared state/database provenance, complete state, active selection,
//! namespace shape, installation, and exact source journal binding. This
//! boundary derives the sole receipt-bound successor, performs exactly one
//! bound journal advance, authenticates the successor through the retained
//! post-advance authority, drops the old lock-bearing store handle, and reopens
//! the canonical journal before returning. It performs no database,
//! namespace, boot, cleanup, trigger, device, retry, or later-phase effect.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    ActiveReblitBootSyncStartedPostAdvanceAuthority,
    ActiveReblitBootSyncStartedRecordAdvanceError,
    ActiveReblitBootSyncStartedRecoveryAuthority,
    ActiveReblitBootSyncStartedRecoveryAuthorityError,
};
use super::canonical_journal_reopen::{
    CanonicalJournalReopenError, reopen_canonical_journal,
};

/// Which exact canonical record survived an uncertain or rejected advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableActiveReblitBootSyncStartedCompletionRecord {
    BootSyncStarted,
    BootSyncComplete,
}

/// Which post-advance authentication boundary rejected the successor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitBootSyncStartedCompletionValidationStage {
    SameStore,
    ReopenedOldBinding,
    ReopenedOldBindingAfterFreshCapture,
    ReopenedFreshBinding,
}

enum ActiveReblitBootSyncStartedCompletionAdvanceOutcome<'reservation> {
    Published {
        successor_binding: TransitionJournalRecordBinding,
        post_advance_authority:
            ActiveReblitBootSyncStartedPostAdvanceAuthority<'reservation>,
        same_store_validation:
            Result<(), ActiveReblitBootSyncStartedRecoveryAuthorityError>,
    },
    StorageFailed(StorageError),
}

/// Persist the sole exact `BootSyncComplete` successor and return only a
/// freshly reopened store which still authenticates the published successor
/// inode.
pub(in crate::client) fn persist_active_reblit_boot_sync_started_completion_and_reopen(
    journal: TransitionJournalStore,
    authority: ActiveReblitBootSyncStartedRecoveryAuthority<'_>,
) -> Result<
    (TransitionJournalStore, TransitionRecord),
    ActiveReblitBootSyncStartedCompletionPersistenceError,
> {
    authority
        .revalidate(&journal)
        .map_err(ActiveReblitBootSyncStartedCompletionPersistenceError::Authority)?;
    let source_record = authority.record().clone();
    let pair = match source_record.boot_publication_receipt_correlation() {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            drop(authority);
            drop(journal);
            return Err(
                ActiveReblitBootSyncStartedCompletionPersistenceError::MissingReceiptCorrelation,
            );
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(
                ActiveReblitBootSyncStartedCompletionPersistenceError::SourceRecord {
                    source,
                },
            );
        }
    };
    let successor = match source_record.boot_sync_complete_successor(pair) {
        Ok(successor) if successor.phase == Phase::BootSyncComplete => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(
                ActiveReblitBootSyncStartedCompletionPersistenceError::UnexpectedSuccessor {
                    phase: successor.phase,
                },
            );
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(
                ActiveReblitBootSyncStartedCompletionPersistenceError::RouteConstruction {
                    source,
                },
            );
        }
    };

    before_active_reblit_boot_sync_started_completion_final_revalidation();
    let installation = authority.installation().clone();
    let advance = match authority.advance_record_binding(&journal, &successor) {
        Ok((successor_binding, post_advance_authority)) => {
            before_active_reblit_boot_sync_started_completion_same_store_validation();
            let same_store_validation =
                post_advance_authority.revalidate_successor_same_store(
                    &journal,
                    &successor_binding,
                    &successor,
                );
            ActiveReblitBootSyncStartedCompletionAdvanceOutcome::Published {
                successor_binding,
                post_advance_authority,
                same_store_validation,
            }
        }
        Err(ActiveReblitBootSyncStartedRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(
                ActiveReblitBootSyncStartedCompletionPersistenceError::Authority(source),
            );
        }
        Err(ActiveReblitBootSyncStartedRecordAdvanceError::Record(source)) => {
            drop(journal);
            return Err(
                ActiveReblitBootSyncStartedCompletionPersistenceError::BoundAdvanceRecord {
                    source,
                },
            );
        }
        Err(ActiveReblitBootSyncStartedRecordAdvanceError::UnexpectedSuccessor) => {
            drop(journal);
            return Err(
                ActiveReblitBootSyncStartedCompletionPersistenceError::BoundAdvanceUnexpectedSuccessor,
            );
        }
        Err(ActiveReblitBootSyncStartedRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(
                ActiveReblitBootSyncStartedCompletionPersistenceError::Installation(source),
            );
        }
        Err(ActiveReblitBootSyncStartedRecordAdvanceError::Storage(source)) => {
            ActiveReblitBootSyncStartedCompletionAdvanceOutcome::StorageFailed(source)
        }
    };

    // The source binding was consumed by the bound advance. Keep the returned
    // successor binding and post-advance authority, but drop this old store
    // before canonical reopen so its writer lock and per-open identity cannot
    // accidentally authorize a second action.
    drop(journal);

    if let ActiveReblitBootSyncStartedCompletionAdvanceOutcome::Published { .. } =
        &advance
    {
        after_active_reblit_boot_sync_started_completion_same_store_check_before_reopen();
    }
    let reopened = reopen_canonical_journal(&installation)
        .map_err(ActiveReblitBootSyncStartedCompletionReopenError::from);

    match advance {
        ActiveReblitBootSyncStartedCompletionAdvanceOutcome::Published {
            successor_binding,
            post_advance_authority,
            same_store_validation: Ok(()),
        } => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                before_active_reblit_boot_sync_started_completion_reopened_validation();
                let reopened_validation =
                    post_advance_authority.revalidate_successor_reopened(
                        &reopened,
                        &successor_binding,
                        &successor,
                    );
                match reopened_validation {
                    Ok(()) => {
                        after_active_reblit_boot_sync_started_completion_old_binding_validation();
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
                                    ActiveReblitBootSyncStartedCompletionPersistenceError::FreshSuccessorBinding {
                                        source,
                                    },
                                );
                            }
                        };
                        let old_binding_revalidation =
                            post_advance_authority.revalidate_successor_reopened(
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
                                ActiveReblitBootSyncStartedCompletionPersistenceError::PostAdvanceValidation {
                                    durable:
                                        DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncComplete,
                                    stage:
                                        ActiveReblitBootSyncStartedCompletionValidationStage::ReopenedOldBindingAfterFreshCapture,
                                    source,
                                },
                            );
                        }
                        drop(successor_binding);
                        before_active_reblit_boot_sync_started_completion_fresh_binding_validation();
                        let final_validation =
                            post_advance_authority.revalidate_successor_same_store(
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
                                Err(
                                    ActiveReblitBootSyncStartedCompletionPersistenceError::PostAdvanceValidation {
                                        durable:
                                            DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncComplete,
                                        stage:
                                            ActiveReblitBootSyncStartedCompletionValidationStage::ReopenedFreshBinding,
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
                        Err(
                            ActiveReblitBootSyncStartedCompletionPersistenceError::PostAdvanceValidation {
                                durable:
                                    DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncComplete,
                                stage:
                                    ActiveReblitBootSyncStartedCompletionValidationStage::ReopenedOldBinding,
                                source,
                            },
                        )
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    ActiveReblitBootSyncStartedCompletionPersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => Err(
                ActiveReblitBootSyncStartedCompletionPersistenceError::ReopenAfterSuccessfulAdvance {
                    source,
                },
            ),
        },
        ActiveReblitBootSyncStartedCompletionAdvanceOutcome::Published {
            successor_binding,
            post_advance_authority,
            same_store_validation: Err(validation),
        } => {
            drop(successor_binding);
            drop(post_advance_authority);
            match reopened {
                Ok((reopened, Some(actual))) if actual == source_record => {
                    drop(reopened);
                    Err(
                        ActiveReblitBootSyncStartedCompletionPersistenceError::PostAdvanceValidation {
                            durable:
                                DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncStarted,
                            stage:
                                ActiveReblitBootSyncStartedCompletionValidationStage::SameStore,
                            source: validation,
                        },
                    )
                }
                Ok((reopened, Some(actual))) if actual == successor => {
                    drop(reopened);
                    Err(
                        ActiveReblitBootSyncStartedCompletionPersistenceError::PostAdvanceValidation {
                            durable:
                                DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncComplete,
                            stage:
                                ActiveReblitBootSyncStartedCompletionValidationStage::SameStore,
                            source: validation,
                        },
                    )
                }
                Ok((reopened, actual)) => {
                    drop(reopened);
                    Err(
                        ActiveReblitBootSyncStartedCompletionPersistenceError::PostAdvanceValidationAndReopen {
                            stage:
                                ActiveReblitBootSyncStartedCompletionValidationStage::SameStore,
                            validation,
                            reopen: unexpected_record(
                                &source_record,
                                &successor,
                                actual,
                            ),
                        },
                    )
                }
                Err(reopen) => Err(
                    ActiveReblitBootSyncStartedCompletionPersistenceError::PostAdvanceValidationAndReopen {
                        stage:
                            ActiveReblitBootSyncStartedCompletionValidationStage::SameStore,
                        validation,
                        reopen,
                    },
                ),
            }
        }
        ActiveReblitBootSyncStartedCompletionAdvanceOutcome::StorageFailed(
            advance_error,
        ) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(
                    ActiveReblitBootSyncStartedCompletionPersistenceError::Advance {
                        durable:
                            DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncStarted,
                        source: advance_error,
                    },
                )
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(
                    ActiveReblitBootSyncStartedCompletionPersistenceError::Advance {
                        durable:
                            DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncComplete,
                        source: advance_error,
                    },
                )
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    ActiveReblitBootSyncStartedCompletionPersistenceError::AdvanceAndReopen {
                        advance: advance_error,
                        reopen: unexpected_record(
                            &source_record,
                            &successor,
                            actual,
                        ),
                    },
                )
            }
            Err(reopen) => Err(
                ActiveReblitBootSyncStartedCompletionPersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen,
                },
            ),
        },
    }
}

fn recapture_reopened_successor_binding(
    installation: &crate::Installation,
    reopened: &TransitionJournalStore,
    successor: &TransitionRecord,
) -> Result<
    TransitionJournalRecordBinding,
    ActiveReblitBootSyncStartedCompletionFreshBindingError,
> {
    installation.revalidate_mutable_namespace().map_err(
        ActiveReblitBootSyncStartedCompletionFreshBindingError::Installation,
    )?;
    let cast = installation.retained_mutable_cast_directory().map_err(
        ActiveReblitBootSyncStartedCompletionFreshBindingError::Installation,
    )?;
    let fresh_binding = reopened.record_binding(cast, successor).map_err(
        ActiveReblitBootSyncStartedCompletionFreshBindingError::Storage,
    )?;
    installation.revalidate_mutable_namespace().map_err(
        ActiveReblitBootSyncStartedCompletionFreshBindingError::Installation,
    )?;
    Ok(fresh_binding)
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> ActiveReblitBootSyncStartedCompletionReopenError {
    ActiveReblitBootSyncStartedCompletionReopenError::UnexpectedRecord {
        expected_boot_sync_started: Box::new(source.clone()),
        expected_boot_sync_complete: Box::new(successor.clone()),
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
pub(in crate::client) fn arm_before_active_reblit_boot_sync_started_completion_final_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_active_reblit_boot_sync_started_completion_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_active_reblit_boot_sync_started_completion_final_revalidation() {}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_boot_sync_started_completion_same_store_validation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_SAME_STORE_VALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_active_reblit_boot_sync_started_completion_same_store_validation() {
    BEFORE_SAME_STORE_VALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_active_reblit_boot_sync_started_completion_same_store_validation() {}

#[cfg(test)]
pub(in crate::client) fn arm_after_active_reblit_boot_sync_started_completion_same_store_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SAME_STORE_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_active_reblit_boot_sync_started_completion_same_store_check_before_reopen() {
    AFTER_SAME_STORE_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_active_reblit_boot_sync_started_completion_same_store_check_before_reopen() {}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_boot_sync_started_completion_reopened_validation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_REOPENED_VALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_active_reblit_boot_sync_started_completion_reopened_validation() {
    BEFORE_REOPENED_VALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_active_reblit_boot_sync_started_completion_reopened_validation() {}

#[cfg(test)]
pub(in crate::client) fn arm_after_active_reblit_boot_sync_started_completion_old_binding_validation(
    hook: impl FnOnce() + 'static,
) {
    AFTER_OLD_REOPENED_VALIDATION_BEFORE_FRESH_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_active_reblit_boot_sync_started_completion_old_binding_validation() {
    AFTER_OLD_REOPENED_VALIDATION_BEFORE_FRESH_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_active_reblit_boot_sync_started_completion_old_binding_validation() {}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_boot_sync_started_completion_fresh_binding_validation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FRESH_BINDING_VALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_active_reblit_boot_sync_started_completion_fresh_binding_validation() {
    BEFORE_FRESH_BINDING_VALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_active_reblit_boot_sync_started_completion_fresh_binding_validation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncStartedCompletionFreshBindingError {
    #[error(
        "revalidate retained installation around fresh reopened BootSyncComplete binding capture"
    )]
    Installation(#[source] installation::Error),
    #[error("capture a fresh same-store binding for the reopened BootSyncComplete record")]
    Storage(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncStartedCompletionPersistenceError {
    #[error("revalidate exact ActiveReblit BootSyncStarted startup recovery authority")]
    Authority(#[source] ActiveReblitBootSyncStartedRecoveryAuthorityError),
    #[error("decode the exact ActiveReblit BootSyncStarted receipt correlation")]
    SourceRecord {
        #[source]
        source: CodecError,
    },
    #[error("the exact ActiveReblit BootSyncStarted record has no receipt correlation")]
    MissingReceiptCorrelation,
    #[error("derive the sole legal receipt-bound ActiveReblit BootSyncComplete successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error(
        "ActiveReblit boot-sync completion routing selected unexpected successor phase {phase:?}"
    )]
    UnexpectedSuccessor { phase: Phase },
    #[error("validate the exact ActiveReblit BootSyncComplete successor at the bound advance")]
    BoundAdvanceRecord {
        #[source]
        source: CodecError,
    },
    #[error(
        "the bound advance rejected the derived exact ActiveReblit BootSyncComplete successor"
    )]
    BoundAdvanceUnexpectedSuccessor,
    #[error("revalidate retained installation before the exact ActiveReblit BootSyncComplete advance")]
    Installation(#[source] installation::Error),
    #[error(
        "ActiveReblit boot-sync completion journal advance failed after reopening exact durable {durable:?} record"
    )]
    Advance {
        durable: DurableActiveReblitBootSyncStartedCompletionRecord,
        #[source]
        source: StorageError,
    },
    #[error(
        "ActiveReblit boot-sync completion journal advance failed ({advance}) and its canonical record could not be reconciled"
    )]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: ActiveReblitBootSyncStartedCompletionReopenError,
    },
    #[error(
        "post-advance {stage:?} validation failed after reopening exact durable {durable:?} record"
    )]
    PostAdvanceValidation {
        durable: DurableActiveReblitBootSyncStartedCompletionRecord,
        stage: ActiveReblitBootSyncStartedCompletionValidationStage,
        #[source]
        source: ActiveReblitBootSyncStartedRecoveryAuthorityError,
    },
    #[error(
        "post-advance {stage:?} validation failed ({validation}) and its canonical record could not be reconciled"
    )]
    PostAdvanceValidationAndReopen {
        stage: ActiveReblitBootSyncStartedCompletionValidationStage,
        validation: ActiveReblitBootSyncStartedRecoveryAuthorityError,
        #[source]
        reopen: ActiveReblitBootSyncStartedCompletionReopenError,
    },
    #[error("capture a fresh binding for the exact reopened ActiveReblit BootSyncComplete record")]
    FreshSuccessorBinding {
        #[source]
        source: ActiveReblitBootSyncStartedCompletionFreshBindingError,
    },
    #[error("reopen the canonical journal after its ActiveReblit BootSyncComplete advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: ActiveReblitBootSyncStartedCompletionReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncStartedCompletionReopenError {
    #[error("revalidate retained installation around ActiveReblit boot-sync completion journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical ActiveReblit boot-sync completion journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact ActiveReblit BootSyncStarted nor BootSyncComplete record (boot_sync_started={expected_boot_sync_started:?}, boot_sync_complete={expected_boot_sync_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_boot_sync_started: Box<TransitionRecord>,
        expected_boot_sync_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError>
    for ActiveReblitBootSyncStartedCompletionReopenError
{
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => {
                Self::Installation(source)
            }
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
