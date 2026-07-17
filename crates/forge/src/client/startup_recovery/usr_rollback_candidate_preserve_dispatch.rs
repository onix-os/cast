//! Consuming leaf for one exact NewState candidate-preservation checkpoint.
//!
//! Absent or restrictive targets permit one preparation attempt and return
//! only after proving that the exact source journal remains unchanged. Empty
//! targets permit one candidate move, and already-preserved candidates permit
//! no move. Those successful paths converge after the same post-move
//! durability suffix and persist `CandidatePreserved` exactly once. No branch
//! can continue into the next recovery checkpoint in this invocation.

use thiserror::Error;

use crate::transition_journal::{Phase, StorageError, TransitionJournalStore, TransitionRecord};

use super::super::startup_reconciliation::{
    UsrRollbackCandidatePreserveApplyAuthority, UsrRollbackCandidatePreserveApplyEffectSelection,
    UsrRollbackCandidatePreserveAuthorityError, UsrRollbackCandidatePreserveFinishAuthority,
    UsrRollbackCandidatePreserveFinishDurabilitySelection, UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority,
    UsrRollbackNewStateCandidatePreserveApplyReconciliation,
    UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation,
    UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation,
};
use super::{UsrRollbackCandidatePreservePersistenceError, persist_usr_rollback_candidate_preserve_and_reopen};

/// Unforgeable permission to consume read-only candidate-preservation
/// admission into one NewState target-creation, normalization, or move
/// checkpoint. The private field is rooted in this exact consuming leaf.
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

/// Exact read-only candidate-preservation admission ready for one consuming
/// NewState startup checkpoint.
pub(in crate::client) enum UsrRollbackCandidatePreserveReady<'reservation> {
    Apply(UsrRollbackCandidatePreserveApplyAuthority<'reservation>),
    Finish(UsrRollbackCandidatePreserveFinishAuthority<'reservation>),
}

/// Capability which may enter the shared post-move durability suffix.
///
/// Finish admission remains opaque until every preparation-only branch has
/// returned, so target creation and normalization receive no durability seal.
enum CandidatePreserveDurabilityReady<'reservation> {
    Applied(UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority<'reservation>),
    Finish(UsrRollbackCandidatePreserveFinishAuthority<'reservation>),
}

/// Consume at most one candidate-preservation effect and, only when the
/// candidate is already in its durable preserved namespace, persist its sole
/// successor once.
pub(in crate::client) fn dispatch_usr_rollback_candidate_preserve_and_reopen<'reservation>(
    journal: TransitionJournalStore,
    source_record: TransitionRecord,
    ready: UsrRollbackCandidatePreserveReady<'reservation>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackCandidatePreserveDispatchError> {
    require_exact_source(&journal, &source_record)?;

    let durability_ready = match ready {
        UsrRollbackCandidatePreserveReady::Apply(authority) => {
            // This is the only production constructor. Finish admission never
            // receives an unused effect capability.
            let effect_seal = UsrRollbackCandidatePreserveEffectSeal::new();
            match authority.into_effect_selection(&effect_seal, &journal)? {
                UsrRollbackCandidatePreserveApplyEffectSelection::CreateNewStateTarget(lease) => {
                    match lease.reconcile(&effect_seal, &journal)? {
                        UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired => {
                            return return_exact_unchanged_source(journal, source_record);
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
                        UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::RestartRequired => {
                            return return_exact_unchanged_source(journal, source_record);
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
                            CandidatePreserveDurabilityReady::Applied(authority)
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
                UsrRollbackCandidatePreserveApplyEffectSelection::Unsupported => {
                    drop(journal);
                    return Err(UsrRollbackCandidatePreserveDispatchError::Unsupported);
                }
            }
        }
        UsrRollbackCandidatePreserveReady::Finish(authority) => CandidatePreserveDurabilityReady::Finish(authority),
    };

    // This is the only production constructor, reached only after every
    // preparation-only branch has returned without a durability capability.
    let durability_seal = UsrRollbackCandidatePreserveDurabilitySeal::new();
    let durable = match durability_ready {
        CandidatePreserveDurabilityReady::Applied(authority) => {
            authority.complete_post_move_durability(&durability_seal, &journal)?
        }
        CandidatePreserveDurabilityReady::Finish(authority) => {
            match authority.into_post_move_durability_selection(&durability_seal, &journal)? {
                UsrRollbackCandidatePreserveFinishDurabilitySelection::NewState(authority) => {
                    authority.complete_post_move_durability(&durability_seal, &journal)?
                }
                UsrRollbackCandidatePreserveFinishDurabilitySelection::Unsupported => {
                    drop(journal);
                    return Err(UsrRollbackCandidatePreserveDispatchError::Unsupported);
                }
            }
        }
    };

    persist_usr_rollback_candidate_preserve_and_reopen(journal, durable)
        .map_err(UsrRollbackCandidatePreserveDispatchError::from)
}

fn require_exact_source(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackCandidatePreserveDispatchError> {
    let actual = journal
        .load()
        .map_err(UsrRollbackCandidatePreserveDispatchError::JournalRead)?;
    match actual {
        Some(actual) if actual == *expected && actual.phase == Phase::CandidatePreserveIntent => Ok(()),
        Some(actual) => Err(UsrRollbackCandidatePreserveDispatchError::UnexpectedSource {
            expected: Box::new(expected.clone()),
            actual: Some(Box::new(actual)),
        }),
        None => Err(UsrRollbackCandidatePreserveDispatchError::UnexpectedSource {
            expected: Box::new(expected.clone()),
            actual: None,
        }),
    }
}

fn return_exact_unchanged_source(
    journal: TransitionJournalStore,
    expected: TransitionRecord,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackCandidatePreserveDispatchError> {
    let actual = journal
        .load()
        .map_err(UsrRollbackCandidatePreserveDispatchError::JournalRead)?;
    match actual {
        Some(actual) if actual == expected && actual.phase == Phase::CandidatePreserveIntent => Ok((journal, actual)),
        Some(actual) => {
            drop(journal);
            Err(UsrRollbackCandidatePreserveDispatchError::UnexpectedSource {
                expected: Box::new(expected),
                actual: Some(Box::new(actual)),
            })
        }
        None => {
            drop(journal);
            Err(UsrRollbackCandidatePreserveDispatchError::UnexpectedSource {
                expected: Box::new(expected),
                actual: None,
            })
        }
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackCandidatePreserveDispatchError {
    #[error("read the exact CandidatePreserveIntent source around a preparation-only effect")]
    JournalRead(#[source] StorageError),
    #[error("candidate-preservation leaf was paired with an unexpected canonical source record")]
    UnexpectedSource {
        expected: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
    #[error("consume and reconcile exact NewState candidate-preservation authority")]
    Authority(#[from] UsrRollbackCandidatePreserveAuthorityError),
    #[error("persist exact durable NewState candidate-preservation outcome")]
    Persistence(#[from] UsrRollbackCandidatePreservePersistenceError),
    #[error("one-shot NewState candidate-preservation attempt was not applied")]
    NotApplied,
    #[error("one-shot NewState candidate-preservation attempt has ambiguous namespace evidence")]
    Ambiguous,
    #[error("candidate-preservation authority selected an unsupported operation family")]
    Unsupported,
}
