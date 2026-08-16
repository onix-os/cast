//! Persist the journal-only NewState route from `BootRepairRequired` to
//! `BootRepairStarted`.
//!
//! The supplied authority retains exact jointly-absent database evidence and
//! exact namespace, journal-record, plan, installation, and active-state
//! reservation evidence. This boundary revalidates that authority twice,
//! consumes its exact predecessor binding through one conditional journal
//! advance, validates the published successor against the same store, destroys
//! the old store, and independently reopens the same successor inode and
//! record. It performs no database, namespace, trigger, cleanup, retry,
//! finalizer, or journal-delete effect.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    UsrRollbackNewStateBootRepairStartAuthority, UsrRollbackNewStateBootRepairStartAuthorityError,
    UsrRollbackNewStateBootRepairStartRecordAdvanceError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};


/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackNewStateBootRepairStartRecord {
    BootRepairRequired,
    BootRepairStarted,
}

enum UsrRollbackNewStateBootRepairStartAdvanceOutcome {
    Published(TransitionJournalRecordBinding),
    StorageFailed(StorageError),
    SuccessorBindingFailed(UsrRollbackNewStateBootRepairStartSuccessorBindingError),
}

/// Persist the sole boot-repair successor, then independently reopen
/// and compare its complete canonical record and exact inode binding.
pub(in crate::client) fn persist_usr_rollback_new_state_boot_repair_start_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackNewStateBootRepairStartAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackNewStateBootRepairStartPersistenceError> {
    authority.revalidate(&journal)?;
    let source_record = authority.record().clone();
    let successor = match source_record.boot_repair_started_successor() {
        Ok(successor) if successor.phase == Phase::BootRepairStarted => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackNewStateBootRepairStartPersistenceError::UnexpectedSuccessor { phase: successor.phase });
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackNewStateBootRepairStartPersistenceError::RouteConstruction { source });
        }
    };

    before_usr_rollback_new_state_boot_repair_start_final_revalidation();
    let installation = authority.installation().clone();
    let advance = match authority.advance_record_binding(&journal, &successor) {
        Ok(successor_binding) => {
            before_usr_rollback_new_state_boot_repair_start_successor_binding_revalidation();
            let exact = revalidate_published_route_binding(&installation, &journal, &successor_binding, &successor);
            match exact {
                Ok(true) => UsrRollbackNewStateBootRepairStartAdvanceOutcome::Published(successor_binding),
                Ok(false) => {
                    drop(successor_binding);
                    UsrRollbackNewStateBootRepairStartAdvanceOutcome::SuccessorBindingFailed(
                        UsrRollbackNewStateBootRepairStartSuccessorBindingError::Changed,
                    )
                }
                Err(source) => {
                    drop(successor_binding);
                    UsrRollbackNewStateBootRepairStartAdvanceOutcome::SuccessorBindingFailed(source)
                }
            }
        }
        Err(UsrRollbackNewStateBootRepairStartRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(UsrRollbackNewStateBootRepairStartPersistenceError::Authority(source));
        }
        Err(UsrRollbackNewStateBootRepairStartRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(UsrRollbackNewStateBootRepairStartPersistenceError::Installation(source));
        }
        Err(UsrRollbackNewStateBootRepairStartRecordAdvanceError::Storage(source)) => {
            UsrRollbackNewStateBootRepairStartAdvanceOutcome::StorageFailed(source)
        }
    };

    // The predecessor binding and complete authority were consumed by the
    // bound advance. Destroy the old lock-bearing store before canonical
    // reopen so neither old per-open identity can be reused.
    drop(journal);

    if let UsrRollbackNewStateBootRepairStartAdvanceOutcome::Published(_) = &advance {
        after_usr_rollback_new_state_boot_repair_start_successor_binding_check_before_reopen();
    }
    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackNewStateBootRepairStartReopenError::from);
    match advance {
        UsrRollbackNewStateBootRepairStartAdvanceOutcome::Published(successor_binding) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                let exact = revalidate_reopened_route_binding(&installation, &reopened, &successor_binding, &successor);
                drop(successor_binding);
                match exact {
                    Ok(true) => Ok((reopened, successor)),
                    Ok(false) => {
                        drop(reopened);
                        Err(UsrRollbackNewStateBootRepairStartPersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackNewStateBootRepairStartRecord::BootRepairStarted,
                            source: UsrRollbackNewStateBootRepairStartSuccessorBindingError::Changed,
                        })
                    }
                    Err(source) => {
                        drop(reopened);
                        Err(UsrRollbackNewStateBootRepairStartPersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackNewStateBootRepairStartRecord::BootRepairStarted,
                            source,
                        })
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackNewStateBootRepairStartPersistenceError::ReopenAfterSuccessfulAdvance {
                    source: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(source) => Err(UsrRollbackNewStateBootRepairStartPersistenceError::ReopenAfterSuccessfulAdvance { source }),
        },
        UsrRollbackNewStateBootRepairStartAdvanceOutcome::StorageFailed(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackNewStateBootRepairStartPersistenceError::Advance {
                    durable: DurableUsrRollbackNewStateBootRepairStartRecord::BootRepairRequired,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackNewStateBootRepairStartPersistenceError::Advance {
                    durable: DurableUsrRollbackNewStateBootRepairStartRecord::BootRepairStarted,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackNewStateBootRepairStartPersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackNewStateBootRepairStartPersistenceError::AdvanceAndReopen {
                advance: advance_error,
                reopen,
            }),
        },
        UsrRollbackNewStateBootRepairStartAdvanceOutcome::SuccessorBindingFailed(binding) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackNewStateBootRepairStartPersistenceError::SuccessorRecordBinding {
                    durable: DurableUsrRollbackNewStateBootRepairStartRecord::BootRepairRequired,
                    source: binding,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackNewStateBootRepairStartPersistenceError::SuccessorRecordBinding {
                    durable: DurableUsrRollbackNewStateBootRepairStartRecord::BootRepairStarted,
                    source: binding,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackNewStateBootRepairStartPersistenceError::SuccessorRecordBindingAndReopen {
                        binding,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => {
                Err(UsrRollbackNewStateBootRepairStartPersistenceError::SuccessorRecordBindingAndReopen { binding, reopen })
            }
        },
    }
}

fn revalidate_published_route_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackNewStateBootRepairStartSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackNewStateBootRepairStartSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackNewStateBootRepairStartSuccessorBindingError::Installation)?;
    let exact = journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackNewStateBootRepairStartSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackNewStateBootRepairStartSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn revalidate_reopened_route_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackNewStateBootRepairStartSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackNewStateBootRepairStartSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackNewStateBootRepairStartSuccessorBindingError::Installation)?;
    let exact = journal
        .has_reopened_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackNewStateBootRepairStartSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackNewStateBootRepairStartSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackNewStateBootRepairStartReopenError {
    UsrRollbackNewStateBootRepairStartReopenError::UnexpectedRecord {
        expected_fresh_db_invalidated: Box::new(source.clone()),
        expected_rollback_complete: Box::new(successor.clone()),
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
pub(crate) fn arm_before_usr_rollback_new_state_boot_repair_start_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_new_state_boot_repair_start_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_new_state_boot_repair_start_final_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_new_state_boot_repair_start_successor_binding_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_new_state_boot_repair_start_successor_binding_revalidation() {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_new_state_boot_repair_start_successor_binding_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_after_usr_rollback_new_state_boot_repair_start_successor_binding_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_usr_rollback_new_state_boot_repair_start_successor_binding_check_before_reopen() {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_new_state_boot_repair_start_successor_binding_check_before_reopen() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackNewStateBootRepairStartSuccessorBindingError {
    #[error("revalidate retained installation after publishing NewState BootRepairStarted")]
    Installation(#[source] installation::Error),
    #[error("the published NewState BootRepairStarted successor lost its exact record binding")]
    Changed,
    #[error("revalidate the published NewState BootRepairStarted successor record binding")]
    Storage(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackNewStateBootRepairStartPersistenceError {
    #[error("revalidate exact FreshDbInvalidated rollback-completion routing authority")]
    Authority(#[from] UsrRollbackNewStateBootRepairStartAuthorityError),
    #[error("derive the sole legal BootRepairStarted successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("rollback-completion routing selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("revalidate retained installation before the exact NewState BootRepairStarted record advance")]
    Installation(#[from] installation::Error),
    #[error("published successor binding failed with exact durable {durable:?} NewState route evidence")]
    SuccessorRecordBinding {
        durable: DurableUsrRollbackNewStateBootRepairStartRecord,
        #[source]
        source: UsrRollbackNewStateBootRepairStartSuccessorBindingError,
    },
    #[error("successor binding failed ({binding}) and its canonical record could not be reconciled")]
    SuccessorRecordBindingAndReopen {
        binding: UsrRollbackNewStateBootRepairStartSuccessorBindingError,
        #[source]
        reopen: UsrRollbackNewStateBootRepairStartReopenError,
    },
    #[error("journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackNewStateBootRepairStartRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its BootRepairStarted advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackNewStateBootRepairStartReopenError,
    },
    #[error("journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackNewStateBootRepairStartReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackNewStateBootRepairStartReopenError {
    #[error("revalidate retained installation around journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact BootRepairRequired nor BootRepairStarted record (boot_repair_required={expected_fresh_db_invalidated:?}, boot_repair_required={expected_rollback_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_fresh_db_invalidated: Box<TransitionRecord>,
        expected_rollback_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackNewStateBootRepairStartReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
