//! Persist the conservative ActiveReblit `BootRepairRequired ->
//! BootRepairStarted` journal boundary.
//!
//! The executor performs exactly one conditional journal advance and returns
//! immediately. It invokes no boot, filesystem, database, cleanup, trigger,
//! finalization, or journal-delete effect. A later startup independently
//! recaptures Started evidence and retains `BootRepairUnverified` for manual
//! recovery.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore,
        TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    UsrRollbackActiveReblitBootRepairStartAuthority,
    UsrRollbackActiveReblitBootRepairStartAuthorityError,
    UsrRollbackActiveReblitBootRepairStartRecordAdvanceError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackActiveReblitBootRepairStartRecord {
    BootRepairRequired,
    BootRepairStarted,
}

enum UsrRollbackActiveReblitBootRepairStartAdvanceOutcome {
    Published(TransitionJournalRecordBinding),
    StorageFailed(StorageError),
    SuccessorBindingFailed(UsrRollbackActiveReblitBootRepairStartSuccessorBindingError),
}

pub(in crate::client) fn persist_usr_rollback_active_reblit_boot_repair_start_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackActiveReblitBootRepairStartAuthority<'_, '_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackActiveReblitBootRepairStartPersistenceError> {
    authority.revalidate(&journal)?;
    let source_record = authority.record().clone();
    let successor = authority.boot_repair_started_successor()?;
    if successor.phase != Phase::BootRepairStarted {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackActiveReblitBootRepairStartPersistenceError::UnexpectedSuccessor {
            phase: successor.phase,
        });
    }

    let installation = authority.installation().clone();
    let advance = match authority.advance_record_binding(&journal, &successor) {
        Ok(successor_binding) => {
            before_usr_rollback_active_reblit_boot_repair_start_successor_binding_revalidation();
            let exact = revalidate_published_start_binding(
                &installation,
                &journal,
                &successor_binding,
                &successor,
            );
            match exact {
                Ok(true) => {
                    UsrRollbackActiveReblitBootRepairStartAdvanceOutcome::Published(successor_binding)
                }
                Ok(false) => {
                    drop(successor_binding);
                    UsrRollbackActiveReblitBootRepairStartAdvanceOutcome::SuccessorBindingFailed(
                        UsrRollbackActiveReblitBootRepairStartSuccessorBindingError::Changed,
                    )
                }
                Err(source) => {
                    drop(successor_binding);
                    UsrRollbackActiveReblitBootRepairStartAdvanceOutcome::SuccessorBindingFailed(source)
                }
            }
        }
        Err(UsrRollbackActiveReblitBootRepairStartRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(UsrRollbackActiveReblitBootRepairStartPersistenceError::Authority(source));
        }
        Err(UsrRollbackActiveReblitBootRepairStartRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(UsrRollbackActiveReblitBootRepairStartPersistenceError::Installation(source));
        }
        Err(UsrRollbackActiveReblitBootRepairStartRecordAdvanceError::Storage(source)) => {
            UsrRollbackActiveReblitBootRepairStartAdvanceOutcome::StorageFailed(source)
        }
    };

    // The predecessor binding and complete authority were consumed by the
    // bound advance. Destroy the old lock-bearing store before canonical
    // reopen and never reuse it after an uncertain write result.
    drop(journal);

    if let UsrRollbackActiveReblitBootRepairStartAdvanceOutcome::Published(_) = &advance {
        after_usr_rollback_active_reblit_boot_repair_start_successor_binding_check_before_reopen();
    }
    let reopened = reopen_canonical_journal(&installation)
        .map_err(UsrRollbackActiveReblitBootRepairStartReopenError::from);
    match advance {
        UsrRollbackActiveReblitBootRepairStartAdvanceOutcome::Published(successor_binding) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                let exact = revalidate_reopened_start_binding(
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
                            UsrRollbackActiveReblitBootRepairStartPersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted,
                                source: UsrRollbackActiveReblitBootRepairStartSuccessorBindingError::Changed,
                            },
                        )
                    }
                    Err(source) => {
                        drop(reopened);
                        Err(
                            UsrRollbackActiveReblitBootRepairStartPersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted,
                                source,
                            },
                        )
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitBootRepairStartPersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => Err(
                UsrRollbackActiveReblitBootRepairStartPersistenceError::ReopenAfterSuccessfulAdvance { source },
            ),
        },
        UsrRollbackActiveReblitBootRepairStartAdvanceOutcome::StorageFailed(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackActiveReblitBootRepairStartPersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairRequired,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackActiveReblitBootRepairStartPersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackActiveReblitBootRepairStartPersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackActiveReblitBootRepairStartPersistenceError::AdvanceAndReopen {
                advance: advance_error,
                reopen,
            }),
        },
        UsrRollbackActiveReblitBootRepairStartAdvanceOutcome::SuccessorBindingFailed(binding) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitBootRepairStartPersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairRequired,
                        source: binding,
                    },
                )
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitBootRepairStartPersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted,
                        source: binding,
                    },
                )
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitBootRepairStartPersistenceError::SuccessorRecordBindingAndReopen {
                        binding,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackActiveReblitBootRepairStartPersistenceError::SuccessorRecordBindingAndReopen {
                    binding,
                    reopen,
                },
            ),
        },
    }
}

fn revalidate_published_start_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackActiveReblitBootRepairStartSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActiveReblitBootRepairStartSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackActiveReblitBootRepairStartSuccessorBindingError::Installation)?;
    let exact = journal.has_record_store_binding(successor_binding)
        && journal
            .has_record_binding(cast, successor_binding, successor)
            .map_err(UsrRollbackActiveReblitBootRepairStartSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActiveReblitBootRepairStartSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn revalidate_reopened_start_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackActiveReblitBootRepairStartSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActiveReblitBootRepairStartSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackActiveReblitBootRepairStartSuccessorBindingError::Installation)?;
    let exact = journal
        .has_reopened_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackActiveReblitBootRepairStartSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActiveReblitBootRepairStartSuccessorBindingError::Installation)?;
    Ok(exact)
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_SUCCESSOR_BINDING_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_active_reblit_boot_repair_start_successor_binding_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_active_reblit_boot_repair_start_successor_binding_revalidation() {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_active_reblit_boot_repair_start_successor_binding_revalidation() {}

#[cfg(test)]
pub(in crate::client) fn arm_after_usr_rollback_active_reblit_boot_repair_start_successor_binding_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_usr_rollback_active_reblit_boot_repair_start_successor_binding_check_before_reopen() {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_active_reblit_boot_repair_start_successor_binding_check_before_reopen() {}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackActiveReblitBootRepairStartReopenError {
    UsrRollbackActiveReblitBootRepairStartReopenError::UnexpectedRecord {
        expected_required: Box::new(source.clone()),
        expected_started: Box::new(successor.clone()),
        actual: actual.map(Box::new),
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitBootRepairStartSuccessorBindingError {
    #[error("revalidate retained installation after publishing ActiveReblit BootRepairStarted")]
    Installation(#[source] installation::Error),
    #[error("the published ActiveReblit BootRepairStarted successor lost its exact record binding")]
    Changed,
    #[error("revalidate the published ActiveReblit BootRepairStarted successor record binding")]
    Storage(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitBootRepairStartPersistenceError {
    #[error("revalidate exact ActiveReblit BootRepairRequired start authority")]
    Authority(#[from] UsrRollbackActiveReblitBootRepairStartAuthorityError),
    #[error("derive the sole legal ActiveReblit BootRepairStarted successor")]
    RouteConstruction(#[from] CodecError),
    #[error("ActiveReblit Required route selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("revalidate retained installation before the exact ActiveReblit BootRepairStarted record advance")]
    Installation(#[from] installation::Error),
    #[error("published successor binding failed with exact durable {durable:?} ActiveReblit boot-repair evidence")]
    SuccessorRecordBinding {
        durable: DurableUsrRollbackActiveReblitBootRepairStartRecord,
        #[source]
        source: UsrRollbackActiveReblitBootRepairStartSuccessorBindingError,
    },
    #[error("successor binding failed ({binding}) and its canonical boot-repair record could not be reconciled")]
    SuccessorRecordBindingAndReopen {
        binding: UsrRollbackActiveReblitBootRepairStartSuccessorBindingError,
        #[source]
        reopen: UsrRollbackActiveReblitBootRepairStartReopenError,
    },
    #[error("ActiveReblit Required -> Started advance failed after reopening exact durable {durable:?}")]
    Advance {
        durable: DurableUsrRollbackActiveReblitBootRepairStartRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen exact canonical BootRepairStarted after a successful ActiveReblit advance")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackActiveReblitBootRepairStartReopenError,
    },
    #[error("ActiveReblit Required -> Started advance failed ({advance}) and canonical reopen was inconclusive")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackActiveReblitBootRepairStartReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitBootRepairStartReopenError {
    #[error("revalidate retained installation around ActiveReblit BootRepairStarted reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load descriptor-rooted canonical ActiveReblit BootRepairStarted journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither exact BootRepairRequired nor BootRepairStarted (required={expected_required:?}, started={expected_started:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_required: Box<TransitionRecord>,
        expected_started: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackActiveReblitBootRepairStartReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
