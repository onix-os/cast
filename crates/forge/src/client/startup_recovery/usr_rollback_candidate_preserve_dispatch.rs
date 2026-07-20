//! Consuming leaf for one exact candidate-preservation checkpoint.
//!
//! Absent or restrictive targets permit one preparation attempt and return
//! only after proving that the exact source journal remains unchanged. Empty
//! targets permit one candidate move, and already-preserved candidates permit
//! no move. Those successful paths converge after the same post-move
//! durability suffix and persist `CandidatePreserved` exactly once. No branch
//! can continue into the next recovery checkpoint in this invocation.

use thiserror::Error;

use crate::transition_journal::{Phase, TransitionJournalStore, TransitionRecord};

use super::super::startup_reconciliation::{
    UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority,
    UsrRollbackActiveReblitCandidatePreserveAppliedEffectAuthority,
    UsrRollbackActiveReblitCandidatePreserveApplyReconciliation,
    UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority,
    UsrRollbackArchivedCandidatePreserveAppliedEffectAuthority,
    UsrRollbackArchivedCandidatePreserveApplyReconciliation, UsrRollbackCandidatePreserveApplyAuthority,
    UsrRollbackCandidatePreserveApplyEffectSelection, UsrRollbackCandidatePreserveAuthorityError,
    UsrRollbackCandidatePreserveFinishAuthority, UsrRollbackCandidatePreserveFinishDurabilitySelection,
    UsrRollbackCandidatePreserveRestartAuthority,
    UsrRollbackNewStateCandidatePreserveAlreadySatisfiedEffectAuthority,
    UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority,
    UsrRollbackNewStateCandidatePreserveApplyReconciliation,
    UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation,
    UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation,
};
use super::{
    UsrRollbackActiveReblitCandidatePreservePersistenceError, UsrRollbackArchivedCandidatePreservePersistenceError,
    UsrRollbackCandidatePreservePersistenceError, persist_usr_rollback_active_reblit_candidate_preserve_and_reopen,
    persist_usr_rollback_archived_candidate_preserve_and_reopen, persist_usr_rollback_candidate_preserve_and_reopen,
};

/// Unforgeable permission to consume read-only candidate-preservation
/// admission into one operation-specific preparation or namespace mutation.
/// The private field is rooted in this exact consuming leaf.
pub(in crate::client) struct UsrRollbackCandidatePreserveEffectSeal {
    _private: (),
}

impl UsrRollbackCandidatePreserveEffectSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable permission to consume exact NewState POST authority through
/// the candidate-preservation durability suffix. The private field is rooted
/// in this exact consuming leaf.
pub(in crate::client) struct UsrRollbackCandidatePreserveDurabilitySeal {
    _private: (),
}

impl UsrRollbackCandidatePreserveDurabilitySeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable permission to consume exact ActiveReblit POST authority through
/// its separate post-exchange durability suffix. The private field is rooted
/// in this exact consuming leaf.
pub(in crate::client) struct UsrRollbackActiveReblitCandidatePreserveDurabilitySeal {
    _private: (),
}

impl UsrRollbackActiveReblitCandidatePreserveDurabilitySeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable permission to consume exact ActivateArchived POST authority
/// through its child-move durability suffix.
pub(in crate::client) struct UsrRollbackArchivedCandidatePreserveDurabilitySeal {
    _private: (),
}

impl UsrRollbackArchivedCandidatePreserveDurabilitySeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Exact read-only candidate-preservation admission ready for one consuming
/// operation-specific startup checkpoint.
pub(in crate::client) enum UsrRollbackCandidatePreserveReady<'reservation> {
    Apply(UsrRollbackCandidatePreserveApplyAuthority<'reservation>),
    Finish(UsrRollbackCandidatePreserveFinishAuthority<'reservation>),
}

impl UsrRollbackCandidatePreserveReady<'_> {
    fn exact_source_record(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<TransitionRecord, UsrRollbackCandidatePreserveAuthorityError> {
        match self {
            Self::Apply(authority) => authority.exact_source_record(journal),
            Self::Finish(authority) => authority.exact_source_record(journal),
        }
    }
}

/// Capability which may enter the shared post-move durability suffix.
///
/// Finish admission remains opaque until every preparation-only branch has
/// returned, so target creation and normalization receive no durability seal.
enum NewStateCandidatePreserveDurabilityReady<'reservation> {
    Applied(UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority<'reservation>),
    Finish(UsrRollbackNewStateCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation>),
}

enum ActiveReblitCandidatePreserveDurabilityReady<'reservation> {
    Applied(UsrRollbackActiveReblitCandidatePreserveAppliedEffectAuthority<'reservation>),
    Finish(UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation>),
}

enum ArchivedCandidatePreserveDurabilityReady<'reservation> {
    Applied(UsrRollbackArchivedCandidatePreserveAppliedEffectAuthority<'reservation>),
    Finish(UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation>),
}

enum CandidatePreserveDurabilityReady<'reservation> {
    NewState(NewStateCandidatePreserveDurabilityReady<'reservation>),
    Archived(ArchivedCandidatePreserveDurabilityReady<'reservation>),
    ActiveReblit(ActiveReblitCandidatePreserveDurabilityReady<'reservation>),
}

/// Consume at most one candidate-preservation effect and, only when the
/// candidate is already in its durable preserved namespace, persist its sole
/// successor once.
pub(in crate::client) fn dispatch_usr_rollback_candidate_preserve_and_reopen<'reservation>(
    journal: TransitionJournalStore,
    source_record: TransitionRecord,
    ready: UsrRollbackCandidatePreserveReady<'reservation>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackCandidatePreserveDispatchError> {
    let actual_source = ready.exact_source_record(&journal)?;
    require_exact_source(&source_record, actual_source)?;

    let effect_seal = UsrRollbackCandidatePreserveEffectSeal::new();
    let durability_ready = match ready {
        UsrRollbackCandidatePreserveReady::Apply(authority) => {
            match authority.into_effect_selection(&effect_seal, &journal)? {
                UsrRollbackCandidatePreserveApplyEffectSelection::CreateNewStateTarget(lease) => {
                    match lease.reconcile(&effect_seal, &journal)? {
                        UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired(
                            authority,
                        ) => {
                            return return_exact_unchanged_source(journal, source_record, authority);
                        }
                        UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::NotApplied => {
                            drop(journal);
                            return Err(UsrRollbackCandidatePreserveDispatchError::NotApplied);
                        }
                        UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::Ambiguous => {
                            drop(journal);
                            return Err(UsrRollbackCandidatePreserveDispatchError::Ambiguous);
                        }
                    }
                }
                UsrRollbackCandidatePreserveApplyEffectSelection::NormalizeNewStateTarget(lease) => {
                    match lease.reconcile(&effect_seal, &journal)? {
                        UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::RestartRequired(
                            authority,
                        ) => {
                            return return_exact_unchanged_source(journal, source_record, authority);
                        }
                        UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::NotApplied => {
                            drop(journal);
                            return Err(UsrRollbackCandidatePreserveDispatchError::NotApplied);
                        }
                        UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::Ambiguous => {
                            drop(journal);
                            return Err(UsrRollbackCandidatePreserveDispatchError::Ambiguous);
                        }
                    }
                }
                UsrRollbackCandidatePreserveApplyEffectSelection::MoveNewState(lease) => {
                    match lease.reconcile(&effect_seal, &journal)? {
                        UsrRollbackNewStateCandidatePreserveApplyReconciliation::Applied(authority) => {
                            CandidatePreserveDurabilityReady::NewState(
                                NewStateCandidatePreserveDurabilityReady::Applied(authority),
                            )
                        }
                        UsrRollbackNewStateCandidatePreserveApplyReconciliation::NotApplied => {
                            drop(journal);
                            return Err(UsrRollbackCandidatePreserveDispatchError::NotApplied);
                        }
                        UsrRollbackNewStateCandidatePreserveApplyReconciliation::Ambiguous => {
                            drop(journal);
                            return Err(UsrRollbackCandidatePreserveDispatchError::Ambiguous);
                        }
                    }
                }
                UsrRollbackCandidatePreserveApplyEffectSelection::MoveArchived(lease) => {
                    match lease.reconcile(&effect_seal, &journal)? {
                        UsrRollbackArchivedCandidatePreserveApplyReconciliation::Applied(authority) => {
                            CandidatePreserveDurabilityReady::Archived(
                                ArchivedCandidatePreserveDurabilityReady::Applied(authority),
                            )
                        }
                        UsrRollbackArchivedCandidatePreserveApplyReconciliation::NotApplied => {
                            drop(journal);
                            return Err(UsrRollbackCandidatePreserveDispatchError::NotApplied);
                        }
                        UsrRollbackArchivedCandidatePreserveApplyReconciliation::Ambiguous => {
                            drop(journal);
                            return Err(UsrRollbackCandidatePreserveDispatchError::Ambiguous);
                        }
                    }
                }
                UsrRollbackCandidatePreserveApplyEffectSelection::ExchangeActiveReblit(lease) => {
                    match lease.reconcile(&effect_seal, &journal)? {
                        UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::Applied(authority) => {
                            CandidatePreserveDurabilityReady::ActiveReblit(
                                ActiveReblitCandidatePreserveDurabilityReady::Applied(authority),
                            )
                        }
                        UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::NotApplied => {
                            drop(journal);
                            return Err(UsrRollbackCandidatePreserveDispatchError::NotApplied);
                        }
                        UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::Ambiguous => {
                            drop(journal);
                            return Err(UsrRollbackCandidatePreserveDispatchError::Ambiguous);
                        }
                    }
                }
                UsrRollbackCandidatePreserveApplyEffectSelection::Unsupported => {
                    drop(journal);
                    return Err(UsrRollbackCandidatePreserveDispatchError::Unsupported);
                }
            }
        }
        UsrRollbackCandidatePreserveReady::Finish(authority) => {
            match authority.into_post_move_durability_selection(&effect_seal, &journal)? {
                UsrRollbackCandidatePreserveFinishDurabilitySelection::NewState(authority) => {
                    CandidatePreserveDurabilityReady::NewState(NewStateCandidatePreserveDurabilityReady::Finish(
                        authority,
                    ))
                }
                UsrRollbackCandidatePreserveFinishDurabilitySelection::Archived(authority) => {
                    CandidatePreserveDurabilityReady::Archived(ArchivedCandidatePreserveDurabilityReady::Finish(
                        authority,
                    ))
                }
                UsrRollbackCandidatePreserveFinishDurabilitySelection::ActiveReblit(authority) => {
                    CandidatePreserveDurabilityReady::ActiveReblit(
                        ActiveReblitCandidatePreserveDurabilityReady::Finish(authority),
                    )
                }
            }
        }
    };

    match durability_ready {
        CandidatePreserveDurabilityReady::NewState(ready) => {
            let durability_seal = UsrRollbackCandidatePreserveDurabilitySeal::new();
            let durable = match ready {
                NewStateCandidatePreserveDurabilityReady::Applied(authority) => {
                    authority.complete_post_move_durability(&durability_seal, &journal)?
                }
                NewStateCandidatePreserveDurabilityReady::Finish(authority) => {
                    authority.complete_post_move_durability(&durability_seal, &journal)?
                }
            };
            persist_usr_rollback_candidate_preserve_and_reopen(journal, durable)
                .map_err(UsrRollbackCandidatePreserveDispatchError::from)
        }
        CandidatePreserveDurabilityReady::ActiveReblit(ready) => {
            let durability_seal = UsrRollbackActiveReblitCandidatePreserveDurabilitySeal::new();
            let durable = match ready {
                ActiveReblitCandidatePreserveDurabilityReady::Applied(authority) => {
                    authority.complete_post_exchange_durability(&durability_seal, &journal)?
                }
                ActiveReblitCandidatePreserveDurabilityReady::Finish(authority) => {
                    authority.complete_post_exchange_durability(&durability_seal, &journal)?
                }
            };
            persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(journal, durable)
                .map_err(UsrRollbackCandidatePreserveDispatchError::from)
        }
        CandidatePreserveDurabilityReady::Archived(ready) => {
            let durability_seal = UsrRollbackArchivedCandidatePreserveDurabilitySeal::new();
            let durable = match ready {
                ArchivedCandidatePreserveDurabilityReady::Applied(authority) => {
                    authority.complete_post_move_durability(&durability_seal, &journal)?
                }
                ArchivedCandidatePreserveDurabilityReady::Finish(authority) => {
                    authority.complete_post_move_durability(&durability_seal, &journal)?
                }
            };
            persist_usr_rollback_archived_candidate_preserve_and_reopen(journal, durable)
                .map_err(UsrRollbackCandidatePreserveDispatchError::from)
        }
    }
}

fn require_exact_source(
    expected: &TransitionRecord,
    actual: TransitionRecord,
) -> Result<(), UsrRollbackCandidatePreserveDispatchError> {
    if actual == *expected && actual.phase == Phase::CandidatePreserveIntent {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveDispatchError::UnexpectedSource {
            expected: Box::new(expected.clone()),
            actual: Some(Box::new(actual)),
        })
    }
}

fn return_exact_unchanged_source<'reservation>(
    journal: TransitionJournalStore,
    expected: TransitionRecord,
    authority: UsrRollbackCandidatePreserveRestartAuthority<'reservation>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackCandidatePreserveDispatchError> {
    let actual = authority.into_exact_source_record(&journal)?;
    if actual == expected && actual.phase == Phase::CandidatePreserveIntent {
        Ok((journal, actual))
    } else {
        drop(journal);
        Err(UsrRollbackCandidatePreserveDispatchError::UnexpectedSource {
            expected: Box::new(expected),
            actual: Some(Box::new(actual)),
        })
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackCandidatePreserveDispatchError {
    #[error("candidate-preservation leaf was paired with an unexpected canonical source record")]
    UnexpectedSource {
        expected: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
    #[error("consume and reconcile exact operation-specific candidate-preservation authority")]
    Authority(#[from] UsrRollbackCandidatePreserveAuthorityError),
    #[error("persist exact durable NewState candidate-preservation outcome")]
    Persistence(#[from] UsrRollbackCandidatePreservePersistenceError),
    #[error("persist exact durable ActiveReblit candidate-preservation outcome")]
    ActiveReblitPersistence(#[from] UsrRollbackActiveReblitCandidatePreservePersistenceError),
    #[error("persist exact durable ActivateArchived candidate-preservation outcome")]
    ArchivedPersistence(#[from] UsrRollbackArchivedCandidatePreservePersistenceError),
    #[error("one-shot candidate-preservation namespace attempt was not applied")]
    NotApplied,
    #[error("one-shot candidate-preservation namespace attempt has ambiguous evidence")]
    Ambiguous,
    #[error("candidate-preservation authority selected an unsupported operation family")]
    Unsupported,
}
