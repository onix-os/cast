//! Persist the journal-only ActiveReblit route from `CandidatePreserved` to
//! `RollbackComplete`.
//!
//! The supplied authority retains exact cleared existing-state provenance,
//! preserved whole-wrapper namespace, journal, plan, installation, and
//! active-state-reservation evidence. This boundary revalidates that authority
//! twice, derives the sole `RollbackComplete` successor, performs exactly one
//! conditional journal advance, and drops both the authority and old store
//! before canonical reopen. It performs no database, namespace, trigger,
//! cleanup, retry, finalizer, or journal-delete effect.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    UsrRollbackActiveReblitCompleteRouteAuthority, UsrRollbackActiveReblitCompleteRouteAuthorityError,
    UsrRollbackActiveReblitCompleteRouteRecordAdvanceError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackActiveReblitCompleteRouteRecord {
    CandidatePreserved,
    RollbackComplete,
}

enum UsrRollbackActiveReblitCompleteRouteAdvanceOutcome {
    Published(TransitionJournalRecordBinding),
    StorageFailed(StorageError),
    SuccessorBindingFailed(UsrRollbackActiveReblitCompleteRouteSuccessorBindingError),
}

/// Persist the sole ActiveReblit rollback-completion successor, then
/// independently reopen and compare the complete canonical record.
pub(in crate::client) fn persist_usr_rollback_active_reblit_complete_route_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackActiveReblitCompleteRouteAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackActiveReblitCompleteRoutePersistenceError> {
    authority.revalidate(&journal)?;
    let source_record = authority.record().clone();
    let successor = match source_record.rollback_successor(None) {
        Ok(successor) if successor.phase == Phase::RollbackComplete => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(
                UsrRollbackActiveReblitCompleteRoutePersistenceError::UnexpectedSuccessor { phase: successor.phase },
            );
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::RouteConstruction { source });
        }
    };

    before_usr_rollback_active_reblit_complete_route_final_revalidation();
    let installation = authority.installation().clone();
    let advance = match authority.advance_record_binding(&journal, &successor) {
        Ok(successor_binding) => {
            before_usr_rollback_active_reblit_complete_route_successor_binding_revalidation();
            let exact = revalidate_published_route_binding(
                &installation,
                &journal,
                &successor_binding,
                &successor,
            );
            match exact {
                Ok(true) => UsrRollbackActiveReblitCompleteRouteAdvanceOutcome::Published(successor_binding),
                Ok(false) => {
                    drop(successor_binding);
                    UsrRollbackActiveReblitCompleteRouteAdvanceOutcome::SuccessorBindingFailed(
                        UsrRollbackActiveReblitCompleteRouteSuccessorBindingError::Changed,
                    )
                }
                Err(source) => {
                    drop(successor_binding);
                    UsrRollbackActiveReblitCompleteRouteAdvanceOutcome::SuccessorBindingFailed(source)
                }
            }
        }
        Err(UsrRollbackActiveReblitCompleteRouteRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::Authority(source));
        }
        Err(UsrRollbackActiveReblitCompleteRouteRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::Installation(source));
        }
        Err(UsrRollbackActiveReblitCompleteRouteRecordAdvanceError::Storage(source)) => {
            UsrRollbackActiveReblitCompleteRouteAdvanceOutcome::StorageFailed(source)
        }
    };

    // The predecessor binding and complete authority were consumed by the
    // bound advance. Destroy the old lock-bearing store before reopen.
    drop(journal);

    if let UsrRollbackActiveReblitCompleteRouteAdvanceOutcome::Published(_) = &advance {
        after_usr_rollback_active_reblit_complete_route_successor_binding_check_before_reopen();
    }
    let reopened =
        reopen_canonical_journal(&installation).map_err(UsrRollbackActiveReblitCompleteRouteReopenError::from);
    match advance {
        UsrRollbackActiveReblitCompleteRouteAdvanceOutcome::Published(successor_binding) => match reopened {
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
                            UsrRollbackActiveReblitCompleteRoutePersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
                                source: UsrRollbackActiveReblitCompleteRouteSuccessorBindingError::Changed,
                            },
                        )
                    }
                    Err(source) => {
                        drop(reopened);
                        Err(
                            UsrRollbackActiveReblitCompleteRoutePersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
                                source,
                            },
                        )
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitCompleteRoutePersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => {
                Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::ReopenAfterSuccessfulAdvance { source })
            }
        },
        UsrRollbackActiveReblitCompleteRouteAdvanceOutcome::StorageFailed(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::CandidatePreserved,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackActiveReblitCompleteRoutePersistenceError::AdvanceAndReopen {
                advance: advance_error,
                reopen,
            }),
        },
        UsrRollbackActiveReblitCompleteRouteAdvanceOutcome::SuccessorBindingFailed(binding) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitCompleteRoutePersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::CandidatePreserved,
                        source: binding,
                    },
                )
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitCompleteRoutePersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
                        source: binding,
                    },
                )
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitCompleteRoutePersistenceError::SuccessorRecordBindingAndReopen {
                        binding,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackActiveReblitCompleteRoutePersistenceError::SuccessorRecordBindingAndReopen {
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
) -> Result<bool, UsrRollbackActiveReblitCompleteRouteSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActiveReblitCompleteRouteSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackActiveReblitCompleteRouteSuccessorBindingError::Installation)?;
    let exact = journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackActiveReblitCompleteRouteSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActiveReblitCompleteRouteSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn revalidate_reopened_route_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackActiveReblitCompleteRouteSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActiveReblitCompleteRouteSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackActiveReblitCompleteRouteSuccessorBindingError::Installation)?;
    let exact = journal
        .has_reopened_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackActiveReblitCompleteRouteSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackActiveReblitCompleteRouteSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackActiveReblitCompleteRouteReopenError {
    UsrRollbackActiveReblitCompleteRouteReopenError::UnexpectedRecord {
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
pub(in crate::client) fn arm_before_usr_rollback_active_reblit_complete_route_final_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_active_reblit_complete_route_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_active_reblit_complete_route_final_revalidation() {}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_active_reblit_complete_route_successor_binding_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_active_reblit_complete_route_successor_binding_revalidation() {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_active_reblit_complete_route_successor_binding_revalidation() {}

#[cfg(test)]
pub(in crate::client) fn arm_after_usr_rollback_active_reblit_complete_route_successor_binding_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_usr_rollback_active_reblit_complete_route_successor_binding_check_before_reopen() {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_active_reblit_complete_route_successor_binding_check_before_reopen() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitCompleteRouteSuccessorBindingError {
    #[error("revalidate retained installation after publishing ActiveReblit RollbackComplete")]
    Installation(#[source] installation::Error),
    #[error("the published ActiveReblit RollbackComplete successor lost its exact record binding")]
    Changed,
    #[error("revalidate the published ActiveReblit RollbackComplete successor record binding")]
    Storage(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitCompleteRoutePersistenceError {
    #[error("revalidate exact ActiveReblit CandidatePreserved rollback-completion authority")]
    Authority(#[from] UsrRollbackActiveReblitCompleteRouteAuthorityError),
    #[error("derive the sole legal ActiveReblit RollbackComplete successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("ActiveReblit rollback-completion routing selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("revalidate retained installation before the exact ActiveReblit RollbackComplete record advance")]
    Installation(#[from] installation::Error),
    #[error("published successor binding failed with exact durable {durable:?} ActiveReblit route evidence")]
    SuccessorRecordBinding {
        durable: DurableUsrRollbackActiveReblitCompleteRouteRecord,
        #[source]
        source: UsrRollbackActiveReblitCompleteRouteSuccessorBindingError,
    },
    #[error("successor binding failed ({binding}) and its canonical record could not be reconciled")]
    SuccessorRecordBindingAndReopen {
        binding: UsrRollbackActiveReblitCompleteRouteSuccessorBindingError,
        #[source]
        reopen: UsrRollbackActiveReblitCompleteRouteReopenError,
    },
    #[error("ActiveReblit rollback-completion journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackActiveReblitCompleteRouteRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its ActiveReblit RollbackComplete advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackActiveReblitCompleteRouteReopenError,
    },
    #[error(
        "ActiveReblit rollback-completion journal advance failed ({advance}) and its canonical record could not be reconciled"
    )]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackActiveReblitCompleteRouteReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitCompleteRouteReopenError {
    #[error("revalidate retained installation around ActiveReblit rollback-completion journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical ActiveReblit rollback-completion journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact ActiveReblit CandidatePreserved nor RollbackComplete record (candidate_preserved={expected_candidate_preserved:?}, rollback_complete={expected_rollback_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_candidate_preserved: Box<TransitionRecord>,
        expected_rollback_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackActiveReblitCompleteRouteReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
