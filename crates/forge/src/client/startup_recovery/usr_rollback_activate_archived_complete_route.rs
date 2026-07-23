//! Persist the journal-only ActivateArchived route from `CandidatePreserved`
//! to `RollbackComplete`.
//!
//! The supplied authority retains exact cleared candidate and previous-state
//! provenance, the preserved archived canonical-slot namespace, journal,
//! plan, installation, and active-state-reservation evidence. This boundary
//! revalidates that authority twice, derives the sole `RollbackComplete`
//! successor, performs exactly one conditional journal advance, and drops
//! both the authority and old store before canonical reopen. It performs no
//! database, namespace, trigger, cleanup, retry, finalizer, or journal-delete
//! effect.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    UsrRollbackActivateArchivedCompleteRouteAuthority, UsrRollbackActivateArchivedCompleteRouteAuthorityError,
    UsrRollbackActivateArchivedCompleteRouteRecordAdvanceError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackActivateArchivedCompleteRouteRecord {
    CandidatePreserved,
    RollbackComplete,
}

enum UsrRollbackActivateArchivedCompleteRouteAdvanceOutcome {
    Published(TransitionJournalRecordBinding),
    StorageFailed(StorageError),
    SuccessorBindingFailed(UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError),
}

/// Persist the sole ActivateArchived rollback-completion successor, then
/// independently reopen and compare the complete canonical record.
pub(in crate::client) fn persist_usr_rollback_activate_archived_complete_route_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackActivateArchivedCompleteRouteAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackActivateArchivedCompleteRoutePersistenceError> {
    authority.revalidate(&journal)?;
    let source_record = authority.record().clone();
    let successor = match source_record.rollback_successor(None) {
        Ok(successor) if successor.phase == Phase::RollbackComplete => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(
                UsrRollbackActivateArchivedCompleteRoutePersistenceError::UnexpectedSuccessor {
                    phase: successor.phase,
                },
            );
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackActivateArchivedCompleteRoutePersistenceError::RouteConstruction { source });
        }
    };

    before_usr_rollback_activate_archived_complete_route_final_revalidation();
    let installation = authority.installation().clone();
    let advance = match authority.advance_record_binding(&journal, &successor) {
        Ok(successor_binding) => {
            before_usr_rollback_activate_archived_complete_route_successor_binding_revalidation();
            let exact = revalidate_published_route_binding(
                &installation,
                &journal,
                &successor_binding,
                &successor,
            );
            match exact {
                Ok(true) => UsrRollbackActivateArchivedCompleteRouteAdvanceOutcome::Published(successor_binding),
                Ok(false) => {
                    drop(successor_binding);
                    UsrRollbackActivateArchivedCompleteRouteAdvanceOutcome::SuccessorBindingFailed(
                        UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError::Changed,
                    )
                }
                Err(source) => {
                    drop(successor_binding);
                    UsrRollbackActivateArchivedCompleteRouteAdvanceOutcome::SuccessorBindingFailed(source)
                }
            }
        }
        Err(UsrRollbackActivateArchivedCompleteRouteRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(UsrRollbackActivateArchivedCompleteRoutePersistenceError::Authority(source));
        }
        Err(UsrRollbackActivateArchivedCompleteRouteRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(UsrRollbackActivateArchivedCompleteRoutePersistenceError::Installation(source));
        }
        Err(UsrRollbackActivateArchivedCompleteRouteRecordAdvanceError::Storage(source)) => {
            UsrRollbackActivateArchivedCompleteRouteAdvanceOutcome::StorageFailed(source)
        }
    };

    // The predecessor binding and complete authority were consumed by the
    // bound advance. Destroy the old lock-bearing store before reopen.
    drop(journal);

    if let UsrRollbackActivateArchivedCompleteRouteAdvanceOutcome::Published(_) = &advance {
        after_usr_rollback_activate_archived_complete_route_successor_binding_check_before_reopen();
    }
    let reopened =
        reopen_canonical_journal(&installation).map_err(UsrRollbackActivateArchivedCompleteRouteReopenError::from);
    match advance {
        UsrRollbackActivateArchivedCompleteRouteAdvanceOutcome::Published(successor_binding) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => {
                let exact = revalidate_reopened_route_binding(
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
                            UsrRollbackActivateArchivedCompleteRoutePersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
                                source: UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError::Changed,
                            },
                        )
                    }
                    Err(source) => {
                        drop(reopened);
                        Err(
                            UsrRollbackActivateArchivedCompleteRoutePersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
                                source,
                            },
                        )
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActivateArchivedCompleteRoutePersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => {
                Err(UsrRollbackActivateArchivedCompleteRoutePersistenceError::ReopenAfterSuccessfulAdvance { source })
            }
        },
        UsrRollbackActivateArchivedCompleteRouteAdvanceOutcome::StorageFailed(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackActivateArchivedCompleteRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::CandidatePreserved,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackActivateArchivedCompleteRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActivateArchivedCompleteRoutePersistenceError::AdvanceAndReopen {
                        advance: advance_error,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackActivateArchivedCompleteRoutePersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen,
                },
            ),
        },
        UsrRollbackActivateArchivedCompleteRouteAdvanceOutcome::SuccessorBindingFailed(binding) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(
                    UsrRollbackActivateArchivedCompleteRoutePersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::CandidatePreserved,
                        source: binding,
                    },
                )
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(
                    UsrRollbackActivateArchivedCompleteRoutePersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
                        source: binding,
                    },
                )
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActivateArchivedCompleteRoutePersistenceError::SuccessorRecordBindingAndReopen {
                        binding,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackActivateArchivedCompleteRoutePersistenceError::SuccessorRecordBindingAndReopen {
                    binding,
                    reopen,
                },
            ),
        },
    }
}

fn revalidate_published_route_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError::Installation)?;
    let exact = journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn revalidate_reopened_route_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError::Installation)?;
    let exact = journal
        .has_reopened_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackActivateArchivedCompleteRouteReopenError {
    UsrRollbackActivateArchivedCompleteRouteReopenError::UnexpectedRecord {
        expected_candidate_preserved: Box::new(source.clone()),
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
pub(in crate::client) fn arm_before_usr_rollback_activate_archived_complete_route_final_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_activate_archived_complete_route_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_activate_archived_complete_route_final_revalidation() {}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_activate_archived_complete_route_successor_binding_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_activate_archived_complete_route_successor_binding_revalidation() {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_activate_archived_complete_route_successor_binding_revalidation() {}

#[cfg(test)]
pub(in crate::client) fn arm_after_usr_rollback_activate_archived_complete_route_successor_binding_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_usr_rollback_activate_archived_complete_route_successor_binding_check_before_reopen() {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_activate_archived_complete_route_successor_binding_check_before_reopen() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError {
    #[error("revalidate retained installation after publishing ActivateArchived RollbackComplete")]
    Installation(#[source] installation::Error),
    #[error("the published ActivateArchived RollbackComplete successor lost its exact record binding")]
    Changed,
    #[error("revalidate the published ActivateArchived RollbackComplete successor record binding")]
    Storage(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActivateArchivedCompleteRoutePersistenceError {
    #[error("revalidate exact ActivateArchived CandidatePreserved rollback-completion authority")]
    Authority(#[from] UsrRollbackActivateArchivedCompleteRouteAuthorityError),
    #[error("derive the sole legal ActivateArchived RollbackComplete successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("ActivateArchived rollback-completion routing selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("revalidate retained installation before the exact ActivateArchived RollbackComplete record advance")]
    Installation(#[from] installation::Error),
    #[error("published successor binding failed with exact durable {durable:?} ActivateArchived route evidence")]
    SuccessorRecordBinding {
        durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord,
        #[source]
        source: UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError,
    },
    #[error("successor binding failed ({binding}) and its canonical record could not be reconciled")]
    SuccessorRecordBindingAndReopen {
        binding: UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError,
        #[source]
        reopen: UsrRollbackActivateArchivedCompleteRouteReopenError,
    },
    #[error(
        "ActivateArchived rollback-completion journal advance failed after reopening exact durable {durable:?} record"
    )]
    Advance {
        durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its ActivateArchived RollbackComplete advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackActivateArchivedCompleteRouteReopenError,
    },
    #[error(
        "ActivateArchived rollback-completion journal advance failed ({advance}) and its canonical record could not be reconciled"
    )]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackActivateArchivedCompleteRouteReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActivateArchivedCompleteRouteReopenError {
    #[error("revalidate retained installation around ActivateArchived rollback-completion journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical ActivateArchived rollback-completion journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact ActivateArchived CandidatePreserved nor RollbackComplete record (candidate_preserved={expected_candidate_preserved:?}, rollback_complete={expected_rollback_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_candidate_preserved: Box<TransitionRecord>,
        expected_rollback_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackActivateArchivedCompleteRouteReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
