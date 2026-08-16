//! Persist the journal-only NewState route from `BootRepairStarted` to
//! `BootRepairUnverified`.
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
    UsrRollbackNewStateBootRepairUnverifiedAuthority, UsrRollbackNewStateBootRepairUnverifiedAuthorityError,
    UsrRollbackNewStateBootRepairUnverifiedRecordAdvanceError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};


/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackNewStateBootRepairUnverifiedRecord {
    BootRepairStarted,
    BootRepairUnverified,
}

enum UsrRollbackNewStateBootRepairUnverifiedAdvanceOutcome {
    Published(TransitionJournalRecordBinding),
    StorageFailed(StorageError),
    SuccessorBindingFailed(UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError),
}

/// Persist the sole boot-repair successor, then independently reopen
/// and compare its complete canonical record and exact inode binding.
pub(in crate::client) fn persist_usr_rollback_new_state_boot_repair_unverified_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackNewStateBootRepairUnverifiedAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackNewStateBootRepairUnverifiedPersistenceError> {
    authority.revalidate(&journal)?;
    let source_record = authority.record().clone();
    let successor = match source_record.boot_repair_unverified_successor() {
        Ok(successor) if successor.phase == Phase::BootRepairUnverified => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::UnexpectedSuccessor { phase: successor.phase });
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::RouteConstruction { source });
        }
    };

    before_usr_rollback_new_state_boot_repair_unverified_final_revalidation();
    let installation = authority.installation().clone();
    let advance = match authority.advance_record_binding(&journal, &successor) {
        Ok(successor_binding) => {
            before_usr_rollback_new_state_boot_repair_unverified_successor_binding_revalidation();
            let exact = revalidate_published_route_binding(&installation, &journal, &successor_binding, &successor);
            match exact {
                Ok(true) => UsrRollbackNewStateBootRepairUnverifiedAdvanceOutcome::Published(successor_binding),
                Ok(false) => {
                    drop(successor_binding);
                    UsrRollbackNewStateBootRepairUnverifiedAdvanceOutcome::SuccessorBindingFailed(
                        UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError::Changed,
                    )
                }
                Err(source) => {
                    drop(successor_binding);
                    UsrRollbackNewStateBootRepairUnverifiedAdvanceOutcome::SuccessorBindingFailed(source)
                }
            }
        }
        Err(UsrRollbackNewStateBootRepairUnverifiedRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::Authority(source));
        }
        Err(UsrRollbackNewStateBootRepairUnverifiedRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::Installation(source));
        }
        Err(UsrRollbackNewStateBootRepairUnverifiedRecordAdvanceError::Storage(source)) => {
            UsrRollbackNewStateBootRepairUnverifiedAdvanceOutcome::StorageFailed(source)
        }
    };

    // The predecessor binding and complete authority were consumed by the
    // bound advance. Destroy the old lock-bearing store before canonical
    // reopen so neither old per-open identity can be reused.
    drop(journal);

    if let UsrRollbackNewStateBootRepairUnverifiedAdvanceOutcome::Published(_) = &advance {
        after_usr_rollback_new_state_boot_repair_unverified_successor_binding_check_before_reopen();
    }
    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackNewStateBootRepairUnverifiedReopenError::from);
    match advance {
        UsrRollbackNewStateBootRepairUnverifiedAdvanceOutcome::Published(successor_binding) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                let exact = revalidate_reopened_route_binding(&installation, &reopened, &successor_binding, &successor);
                drop(successor_binding);
                match exact {
                    Ok(true) => Ok((reopened, successor)),
                    Ok(false) => {
                        drop(reopened);
                        Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackNewStateBootRepairUnverifiedRecord::BootRepairUnverified,
                            source: UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError::Changed,
                        })
                    }
                    Err(source) => {
                        drop(reopened);
                        Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackNewStateBootRepairUnverifiedRecord::BootRepairUnverified,
                            source,
                        })
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::ReopenAfterSuccessfulAdvance {
                    source: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(source) => Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::ReopenAfterSuccessfulAdvance { source }),
        },
        UsrRollbackNewStateBootRepairUnverifiedAdvanceOutcome::StorageFailed(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::Advance {
                    durable: DurableUsrRollbackNewStateBootRepairUnverifiedRecord::BootRepairStarted,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::Advance {
                    durable: DurableUsrRollbackNewStateBootRepairUnverifiedRecord::BootRepairUnverified,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::AdvanceAndReopen {
                advance: advance_error,
                reopen,
            }),
        },
        UsrRollbackNewStateBootRepairUnverifiedAdvanceOutcome::SuccessorBindingFailed(binding) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::SuccessorRecordBinding {
                    durable: DurableUsrRollbackNewStateBootRepairUnverifiedRecord::BootRepairStarted,
                    source: binding,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::SuccessorRecordBinding {
                    durable: DurableUsrRollbackNewStateBootRepairUnverifiedRecord::BootRepairUnverified,
                    source: binding,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackNewStateBootRepairUnverifiedPersistenceError::SuccessorRecordBindingAndReopen {
                        binding,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => {
                Err(UsrRollbackNewStateBootRepairUnverifiedPersistenceError::SuccessorRecordBindingAndReopen { binding, reopen })
            }
        },
    }
}

fn revalidate_published_route_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError::Installation)?;
    let exact = journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn revalidate_reopened_route_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError::Installation)?;
    let exact = journal
        .has_reopened_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackNewStateBootRepairUnverifiedReopenError {
    UsrRollbackNewStateBootRepairUnverifiedReopenError::UnexpectedRecord {
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
pub(crate) fn arm_before_usr_rollback_new_state_boot_repair_unverified_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_new_state_boot_repair_unverified_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_new_state_boot_repair_unverified_final_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_new_state_boot_repair_unverified_successor_binding_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_new_state_boot_repair_unverified_successor_binding_revalidation() {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_new_state_boot_repair_unverified_successor_binding_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_after_usr_rollback_new_state_boot_repair_unverified_successor_binding_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_usr_rollback_new_state_boot_repair_unverified_successor_binding_check_before_reopen() {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_new_state_boot_repair_unverified_successor_binding_check_before_reopen() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError {
    #[error("revalidate retained installation after publishing NewState BootRepairUnverified")]
    Installation(#[source] installation::Error),
    #[error("the published NewState BootRepairUnverified successor lost its exact record binding")]
    Changed,
    #[error("revalidate the published NewState BootRepairUnverified successor record binding")]
    Storage(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackNewStateBootRepairUnverifiedPersistenceError {
    #[error("revalidate exact FreshDbInvalidated rollback-completion routing authority")]
    Authority(#[from] UsrRollbackNewStateBootRepairUnverifiedAuthorityError),
    #[error("derive the sole legal BootRepairUnverified successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("rollback-completion routing selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("revalidate retained installation before the exact NewState BootRepairUnverified record advance")]
    Installation(#[from] installation::Error),
    #[error("published successor binding failed with exact durable {durable:?} NewState route evidence")]
    SuccessorRecordBinding {
        durable: DurableUsrRollbackNewStateBootRepairUnverifiedRecord,
        #[source]
        source: UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError,
    },
    #[error("successor binding failed ({binding}) and its canonical record could not be reconciled")]
    SuccessorRecordBindingAndReopen {
        binding: UsrRollbackNewStateBootRepairUnverifiedSuccessorBindingError,
        #[source]
        reopen: UsrRollbackNewStateBootRepairUnverifiedReopenError,
    },
    #[error("journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackNewStateBootRepairUnverifiedRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its BootRepairUnverified advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackNewStateBootRepairUnverifiedReopenError,
    },
    #[error("journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackNewStateBootRepairUnverifiedReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackNewStateBootRepairUnverifiedReopenError {
    #[error("revalidate retained installation around journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact BootRepairStarted nor BootRepairUnverified record (boot_repair_started={expected_fresh_db_invalidated:?}, boot_repair_required={expected_rollback_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_fresh_db_invalidated: Box<TransitionRecord>,
        expected_rollback_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackNewStateBootRepairUnverifiedReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
