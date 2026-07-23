//! Persist the journal-only route into fresh-database invalidation.
//!
//! The supplied authority retains exact `CandidatePreserved` namespace,
//! database, provenance, journal, plan, and installation evidence. This
//! boundary revalidates that evidence, derives the sole
//! `FreshDbInvalidationIntent` successor, performs exactly one conditional
//! advance, and drops both the authority and old store before canonical
//! reopen. Beyond that journal update, it performs no state-database,
//! provenance, or activation-namespace mutation.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    UsrRollbackFreshDbInvalidationRouteAuthority, UsrRollbackFreshDbInvalidationRouteAuthorityError,
    UsrRollbackFreshDbInvalidationRouteRecordAdvanceError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

#[cfg(test)]
#[allow(dead_code)] // shared candidate fixture contains wider reconciliation helpers
#[path = "../startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod candidate_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider startup-recovery helpers
#[path = "test_support.rs"]
mod test_fixture;
#[cfg(test)]
mod tests;

/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackFreshDbInvalidationRouteRecord {
    CandidatePreserved,
    FreshDbInvalidationIntent,
}

enum UsrRollbackFreshDbInvalidationRouteAdvanceOutcome {
    Published(TransitionJournalRecordBinding),
    StorageFailed(StorageError),
    SuccessorBindingFailed(UsrRollbackFreshDbInvalidationRouteSuccessorBindingError),
}

/// Persist the sole fresh-database invalidation intent, then independently
/// reopen and compare the complete canonical record.
pub(in crate::client) fn persist_usr_rollback_fresh_db_invalidation_route_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackFreshDbInvalidationRouteAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackFreshDbInvalidationRoutePersistenceError> {
    authority.revalidate(&journal)?;
    let source_record = authority.record().clone();
    let successor = match source_record.rollback_successor(None) {
        Ok(successor) if successor.phase == Phase::FreshDbInvalidationIntent => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(
                UsrRollbackFreshDbInvalidationRoutePersistenceError::UnexpectedSuccessor { phase: successor.phase },
            );
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::RouteConstruction { source });
        }
    };

    before_usr_rollback_fresh_db_invalidation_route_final_revalidation();
    let installation = authority.installation().clone();
    let advance = match authority.advance_record_binding(&journal, &successor) {
        Ok(successor_binding) => {
            before_usr_rollback_fresh_db_invalidation_route_successor_binding_revalidation();
            let exact = revalidate_published_route_binding(
                &installation,
                &journal,
                &successor_binding,
                &successor,
            );
            match exact {
                Ok(true) => UsrRollbackFreshDbInvalidationRouteAdvanceOutcome::Published(successor_binding),
                Ok(false) => {
                    drop(successor_binding);
                    UsrRollbackFreshDbInvalidationRouteAdvanceOutcome::SuccessorBindingFailed(
                        UsrRollbackFreshDbInvalidationRouteSuccessorBindingError::Changed,
                    )
                }
                Err(source) => {
                    drop(successor_binding);
                    UsrRollbackFreshDbInvalidationRouteAdvanceOutcome::SuccessorBindingFailed(source)
                }
            }
        }
        Err(UsrRollbackFreshDbInvalidationRouteRecordAdvanceError::Authority(source)) => {
            drop(journal);
            return Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::Authority(source));
        }
        Err(UsrRollbackFreshDbInvalidationRouteRecordAdvanceError::Installation(source)) => {
            drop(journal);
            return Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::Installation(source));
        }
        Err(UsrRollbackFreshDbInvalidationRouteRecordAdvanceError::Storage(source)) => {
            UsrRollbackFreshDbInvalidationRouteAdvanceOutcome::StorageFailed(source)
        }
    };

    // The exact predecessor binding and complete authority were consumed by
    // the bound advance. Destroy the old lock-bearing store before reopen.
    drop(journal);

    if let UsrRollbackFreshDbInvalidationRouteAdvanceOutcome::Published(_) = &advance {
        after_usr_rollback_fresh_db_invalidation_route_successor_binding_check_before_reopen();
    }
    let reopened =
        reopen_canonical_journal(&installation).map_err(UsrRollbackFreshDbInvalidationRouteReopenError::from);
    match advance {
        UsrRollbackFreshDbInvalidationRouteAdvanceOutcome::Published(successor_binding) => match reopened {
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
                            UsrRollbackFreshDbInvalidationRoutePersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,
                                source: UsrRollbackFreshDbInvalidationRouteSuccessorBindingError::Changed,
                            },
                        )
                    }
                    Err(source) => {
                        drop(reopened);
                        Err(
                            UsrRollbackFreshDbInvalidationRoutePersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,
                                source,
                            },
                        )
                    }
                }
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackFreshDbInvalidationRoutePersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => {
                Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::ReopenAfterSuccessfulAdvance { source })
            }
        },
        UsrRollbackFreshDbInvalidationRouteAdvanceOutcome::StorageFailed(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackFreshDbInvalidationRoutePersistenceError::AdvanceAndReopen {
                advance: advance_error,
                reopen,
            }),
        },
        UsrRollbackFreshDbInvalidationRouteAdvanceOutcome::SuccessorBindingFailed(binding) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(
                    UsrRollbackFreshDbInvalidationRoutePersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved,
                        source: binding,
                    },
                )
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(
                    UsrRollbackFreshDbInvalidationRoutePersistenceError::SuccessorRecordBinding {
                        durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,
                        source: binding,
                    },
                )
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackFreshDbInvalidationRoutePersistenceError::SuccessorRecordBindingAndReopen {
                        binding,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackFreshDbInvalidationRoutePersistenceError::SuccessorRecordBindingAndReopen {
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
) -> Result<bool, UsrRollbackFreshDbInvalidationRouteSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackFreshDbInvalidationRouteSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackFreshDbInvalidationRouteSuccessorBindingError::Installation)?;
    let exact = journal
        .has_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackFreshDbInvalidationRouteSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackFreshDbInvalidationRouteSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn revalidate_reopened_route_binding(
    installation: &crate::Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
) -> Result<bool, UsrRollbackFreshDbInvalidationRouteSuccessorBindingError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackFreshDbInvalidationRouteSuccessorBindingError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackFreshDbInvalidationRouteSuccessorBindingError::Installation)?;
    let exact = journal
        .has_reopened_record_binding(cast, successor_binding, successor)
        .map_err(UsrRollbackFreshDbInvalidationRouteSuccessorBindingError::Storage)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(UsrRollbackFreshDbInvalidationRouteSuccessorBindingError::Installation)?;
    Ok(exact)
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackFreshDbInvalidationRouteReopenError {
    UsrRollbackFreshDbInvalidationRouteReopenError::UnexpectedRecord {
        expected_candidate_preserved: Box::new(source.clone()),
        expected_fresh_db_invalidation_intent: Box::new(successor.clone()),
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
pub(crate) fn arm_before_usr_rollback_fresh_db_invalidation_route_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_fresh_db_invalidation_route_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_fresh_db_invalidation_route_final_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_fresh_db_invalidation_route_successor_binding_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_fresh_db_invalidation_route_successor_binding_revalidation() {
    BEFORE_SUCCESSOR_BINDING_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_fresh_db_invalidation_route_successor_binding_revalidation() {}

#[cfg(test)]
pub(crate) fn arm_after_usr_rollback_fresh_db_invalidation_route_successor_binding_check_before_reopen(
    hook: impl FnOnce() + 'static,
) {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_usr_rollback_fresh_db_invalidation_route_successor_binding_check_before_reopen() {
    AFTER_SUCCESSOR_BINDING_CHECK_BEFORE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_fresh_db_invalidation_route_successor_binding_check_before_reopen() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackFreshDbInvalidationRouteSuccessorBindingError {
    #[error("revalidate retained installation after publishing FreshDbInvalidationIntent")]
    Installation(#[source] installation::Error),
    #[error("the published FreshDbInvalidationIntent successor lost its exact record binding")]
    Changed,
    #[error("revalidate the published FreshDbInvalidationIntent successor record binding")]
    Storage(#[source] StorageError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackFreshDbInvalidationRoutePersistenceError {
    #[error("revalidate exact CandidatePreserved fresh-database invalidation routing authority")]
    Authority(#[from] UsrRollbackFreshDbInvalidationRouteAuthorityError),
    #[error("derive the sole legal FreshDbInvalidationIntent successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("fresh-database invalidation routing selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("revalidate retained installation before the exact FreshDbInvalidationIntent record advance")]
    Installation(#[from] installation::Error),
    #[error("published successor binding failed with exact durable {durable:?} fresh-database route evidence")]
    SuccessorRecordBinding {
        durable: DurableUsrRollbackFreshDbInvalidationRouteRecord,
        #[source]
        source: UsrRollbackFreshDbInvalidationRouteSuccessorBindingError,
    },
    #[error("successor binding failed ({binding}) and its canonical record could not be reconciled")]
    SuccessorRecordBindingAndReopen {
        binding: UsrRollbackFreshDbInvalidationRouteSuccessorBindingError,
        #[source]
        reopen: UsrRollbackFreshDbInvalidationRouteReopenError,
    },
    #[error("journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackFreshDbInvalidationRouteRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its FreshDbInvalidationIntent advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackFreshDbInvalidationRouteReopenError,
    },
    #[error("journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackFreshDbInvalidationRouteReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackFreshDbInvalidationRouteReopenError {
    #[error("revalidate retained installation around journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact CandidatePreserved nor FreshDbInvalidationIntent record (candidate_preserved={expected_candidate_preserved:?}, fresh_db_invalidation_intent={expected_fresh_db_invalidation_intent:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_candidate_preserved: Box<TransitionRecord>,
        expected_fresh_db_invalidation_intent: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackFreshDbInvalidationRouteReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
