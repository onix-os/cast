//! Exact, intentionally unwired system-trigger journal suffix.
//!
//! This boundary is available only to new-state and active-reblit transitions
//! which carried their descriptor-pinned transaction-isolation ABI through the
//! `/usr` exchange. Archived activation remains an explicit unsupported path
//! until it gains the same retained isolation authority. The runner consumes
//! exact record-inode bindings for both journal advances, invokes one callback
//! at most once, and reopens the canonical journal after each successful
//! advance before returning reusable typestate authority.

mod boot_sync_handoff;
mod no_boot_commit_decision;

use std::error::Error as StdError;

use thiserror::Error;

use crate::{
    Installation, db,
    state::{self, TransitionId},
    transition_journal::{
        Operation, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore,
        TransitionRecord,
    },
};

use super::super::{CandidateMetadataProof, Error as IdentityError, StatefulTreeIdentity};
use super::{
    RootLinksCompleteCoordinator, StatefulTransitionCoordinator, StatefulTransitionCoordinatorError,
    UsrExchangeEffectSeal,
    root_abi_publication::require_published_root_abi_sandwich,
    usr_exchange_intent::UsrExchangeReadiness,
};

#[cfg(test)]
pub(super) use no_boot_commit_decision::ActiveReblitNoBootCommitDecisionFailure;
pub(crate) use boot_sync_handoff::{
    ActiveReblitBootSyncHandoffFailure, ActiveReblitBootSyncHandoffSeal,
};

const RUN_SYSTEM_TRIGGERS: &str = "run stateful system triggers";

/// Sole in-process owner after a proven system-trigger effect and durable
/// `SystemTriggersComplete` publication.
#[derive(Debug)]
pub(crate) struct SystemTriggersCompleteCoordinator {
    coordinator: StatefulTransitionCoordinator,
    metadata: CandidateMetadataProof,
    provenance: db::state::MetadataProvenance,
    authority: crate::client::PublishedJournalRootAbiAuthority,
    readiness: UsrExchangeReadiness,
    record_binding: TransitionJournalRecordBinding,
}

/// Callback-scoped view of the exact live candidate and its retained private
/// isolation root. It exposes no journal, database, previous tree, root-link
/// publisher, or lifecycle mutation method.
#[derive(Debug)]
pub(super) struct StatefulSystemTriggerAuthority<'authority> {
    transition_id: &'authority TransitionId,
    candidate_state: state::Id,
    installation: &'authority Installation,
    candidate_usr: &'authority std::fs::File,
    isolation_root: &'authority crate::client::RetainedRootAbi,
}

impl<'authority> StatefulSystemTriggerAuthority<'authority> {
    pub(super) fn transition_id(&self) -> &'authority TransitionId {
        self.transition_id
    }

    pub(super) fn candidate_state(&self) -> state::Id {
        self.candidate_state
    }

    pub(super) fn retained_view(
        &self,
    ) -> (
        &'authority Installation,
        &'authority std::fs::File,
        &'authority crate::client::RetainedRootAbi,
    ) {
        (self.installation, self.candidate_usr, self.isolation_root)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DurableSystemTriggerRecord {
    Predecessor,
    Successor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BoundValidationStage {
    SameStore,
    CanonicalReopen,
}

/// Failure from one consuming record-inode advance and its mandatory fresh
/// canonical reopen. When storage is ambiguous, the reopened exact record is
/// classified as only the predecessor or sole successor whenever possible.
#[derive(Debug, Error)]
pub(super) enum BoundSystemTriggerAdvanceFailure {
    #[error(
        "bound journal advance failed; fresh canonical evidence proves the durable record is {durable:?}"
    )]
    Advance {
        durable: DurableSystemTriggerRecord,
        #[source]
        source: StorageError,
    },
    #[error("bound journal advance failed and fresh canonical evidence could not classify predecessor or successor")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: StatefulTransitionCoordinatorError,
    },
    #[error(
        "bound journal successor validation failed at {stage:?}; a fresh canonical observation classified the record bytes as {durable:?}"
    )]
    Validation {
        durable: DurableSystemTriggerRecord,
        stage: BoundValidationStage,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error("bound journal successor validation failed at {stage:?} and fresh canonical evidence also failed")]
    ValidationAndReopen {
        stage: BoundValidationStage,
        validation: StatefulTransitionCoordinatorError,
        #[source]
        reopen: StatefulTransitionCoordinatorError,
    },
}

/// Fail-stop result of the one-shot system-trigger boundary.
///
/// No failure stores a coordinator, record binding, installation, candidate
/// descriptor, isolation ABI, or callback authority. The effect error is
/// `'static`, so it cannot retain the borrowed callback view.
#[derive(Debug, Error)]
pub(super) enum StatefulSystemTriggerFailure<E>
where
    E: StdError + 'static,
{
    #[error(
        "transition {transition_id} cannot run coordinator system triggers for archived activation before archived retained-isolation authority exists"
    )]
    ArchivedIsolationUnsupported { transition_id: TransitionId },
    #[error("transition {transition_id} failed system-trigger preflight")]
    Preflight {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error(
        "transition {transition_id} derived {actual_phase:?} generation {actual_generation} instead of {expected_phase:?} generation {expected_generation} for {operation:?}"
    )]
    SuccessorContract {
        transition_id: TransitionId,
        operation: Operation,
        expected_phase: Phase,
        expected_generation: u64,
        actual_phase: Phase,
        actual_generation: u64,
    },
    #[error(
        "transition {transition_id} could not durably publish system-trigger intent; RootLinksComplete or SystemTriggersStarted is exact after fresh reopen when classifiable"
    )]
    IntentPersistence {
        transition_id: TransitionId,
        #[source]
        source: BoundSystemTriggerAdvanceFailure,
    },
    #[error(
        "transition {transition_id} failed final pre-effect evidence after durable system-trigger intent; the callback was not invoked"
    )]
    PreEffectEvidence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error("transition {transition_id} system-trigger callback failed after durable intent")]
    Effect {
        transition_id: TransitionId,
        #[source]
        source: E,
    },
    #[error("transition {transition_id} failed post-system-trigger retained evidence")]
    PostEffectEvidence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error(
        "transition {transition_id} could not durably publish system-trigger completion; SystemTriggersStarted or SystemTriggersComplete is exact after fresh reopen when classifiable"
    )]
    CompletionPersistence {
        transition_id: TransitionId,
        #[source]
        source: BoundSystemTriggerAdvanceFailure,
    },
    #[error("transition {transition_id} failed final SystemTriggersComplete retained evidence")]
    FinalEvidence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
}

impl RootLinksCompleteCoordinator {
    /// Persist exact system-trigger intent, invoke one callback with the
    /// retained live-candidate view, and persist exact completion.
    pub(super) fn run_system_triggers<E, F>(
        self,
        effect: F,
    ) -> Result<SystemTriggersCompleteCoordinator, StatefulSystemTriggerFailure<E>>
    where
        E: StdError + 'static,
        F: for<'authority> FnOnce(StatefulSystemTriggerAuthority<'authority>) -> Result<(), E>,
    {
        let Self {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
        } = self;
        let transition_id = coordinator.record.transition_id.clone();
        if matches!(readiness, UsrExchangeReadiness::Archived) {
            return Err(StatefulSystemTriggerFailure::ArchivedIsolationUnsupported {
                transition_id,
            });
        }

        let preflight = |source| StatefulSystemTriggerFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };
        coordinator
            .require_phase(Phase::RootLinksComplete, RUN_SYSTEM_TRIGGERS)
            .map_err(preflight)?;
        require_system_trigger_same_store_evidence(
            &coordinator,
            &metadata,
            &provenance,
            &authority,
            &readiness,
            &record_binding,
        )
        .map_err(preflight)?;
        let started = exact_system_trigger_successor(
            &coordinator.record,
            Phase::SystemTriggersStarted,
            &transition_id,
        )?;
        let (coordinator, record_binding) = advance_bound_system_trigger_record(
            coordinator,
            &metadata,
            &provenance,
            &authority,
            &readiness,
            record_binding,
            started,
        )
        .map_err(|source| StatefulSystemTriggerFailure::IntentPersistence {
            transition_id: transition_id.clone(),
            source,
        })?;

        require_system_trigger_same_store_evidence(
            &coordinator,
            &metadata,
            &provenance,
            &authority,
            &readiness,
            &record_binding,
        )
        .map_err(|source| StatefulSystemTriggerFailure::PreEffectEvidence {
            transition_id: transition_id.clone(),
            source,
        })?;

        let (installation, isolation_root) = system_trigger_isolation_view(&readiness);
        let candidate_state = coordinator.candidate_state().map_err(|source| {
            StatefulSystemTriggerFailure::PreEffectEvidence {
                transition_id: transition_id.clone(),
                source,
            }
        })?;
        let callback_authority = StatefulSystemTriggerAuthority {
            transition_id: &coordinator.record.transition_id,
            candidate_state,
            installation,
            candidate_usr: coordinator.identity.candidate.store.retained_directory(),
            isolation_root,
        };
        effect(callback_authority).map_err(|source| StatefulSystemTriggerFailure::Effect {
            transition_id: transition_id.clone(),
            source,
        })?;

        require_system_trigger_same_store_evidence(
            &coordinator,
            &metadata,
            &provenance,
            &authority,
            &readiness,
            &record_binding,
        )
        .map_err(|source| StatefulSystemTriggerFailure::PostEffectEvidence {
            transition_id: transition_id.clone(),
            source,
        })?;
        let complete = exact_system_trigger_successor(
            &coordinator.record,
            Phase::SystemTriggersComplete,
            &transition_id,
        )?;
        let (coordinator, record_binding) = advance_bound_system_trigger_record(
            coordinator,
            &metadata,
            &provenance,
            &authority,
            &readiness,
            record_binding,
            complete,
        )
        .map_err(|source| StatefulSystemTriggerFailure::CompletionPersistence {
            transition_id: transition_id.clone(),
            source,
        })?;

        require_system_trigger_same_store_evidence(
            &coordinator,
            &metadata,
            &provenance,
            &authority,
            &readiness,
            &record_binding,
        )
        .map_err(|source| StatefulSystemTriggerFailure::FinalEvidence {
            transition_id,
            source,
        })?;
        Ok(SystemTriggersCompleteCoordinator {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
        })
    }
}

fn exact_system_trigger_successor<E>(
    record: &TransitionRecord,
    expected_phase: Phase,
    transition_id: &TransitionId,
) -> Result<TransitionRecord, StatefulSystemTriggerFailure<E>>
where
    E: StdError + 'static,
{
    let successor = record
        .forward_successor(None)
        .map_err(StatefulTransitionCoordinatorError::from)
        .map_err(|source| StatefulSystemTriggerFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        })?;
    let expected_generation = match (record.operation, expected_phase) {
        (Operation::NewState, Phase::SystemTriggersStarted) => 11,
        (Operation::NewState, Phase::SystemTriggersComplete) => 12,
        (Operation::ActiveReblit, Phase::SystemTriggersStarted) => 9,
        (Operation::ActiveReblit, Phase::SystemTriggersComplete) => 10,
        (Operation::ActivateArchived, Phase::SystemTriggersStarted) => 7,
        (Operation::ActivateArchived, Phase::SystemTriggersComplete) => 8,
        _ => record.generation.saturating_add(1),
    };
    if successor.phase != expected_phase || successor.generation != expected_generation {
        return Err(StatefulSystemTriggerFailure::SuccessorContract {
            transition_id: transition_id.clone(),
            operation: record.operation,
            expected_phase,
            expected_generation,
            actual_phase: successor.phase,
            actual_generation: successor.generation,
        });
    }
    Ok(successor)
}

fn advance_bound_system_trigger_record(
    mut coordinator: StatefulTransitionCoordinator,
    metadata: &CandidateMetadataProof,
    provenance: &db::state::MetadataProvenance,
    authority: &crate::client::PublishedJournalRootAbiAuthority,
    readiness: &UsrExchangeReadiness,
    predecessor_binding: TransitionJournalRecordBinding,
    successor: TransitionRecord,
) -> Result<(StatefulTransitionCoordinator, TransitionJournalRecordBinding), BoundSystemTriggerAdvanceFailure> {
    let predecessor = coordinator.record.clone();
    let cast = authority
        .installation()
        .retained_mutable_cast_directory()
        .map_err(IdentityError::from)
        .map_err(StatefulTransitionCoordinatorError::Identity)
        .map_err(|source| BoundSystemTriggerAdvanceFailure::Validation {
            durable: DurableSystemTriggerRecord::Predecessor,
            stage: BoundValidationStage::SameStore,
            source,
        })?;
    let successor_binding = match coordinator
        .identity
        .journal
        .advance_record_binding(cast, predecessor_binding, &successor)
    {
        Ok(binding) => binding,
        Err(advance) => {
            return match reopen_and_classify(
                coordinator,
                metadata,
                provenance,
                authority,
                readiness,
                &predecessor,
                &successor,
                None,
            ) {
                Ok(ReopenedAdvance::Proven { durable, .. }) => {
                    Err(BoundSystemTriggerAdvanceFailure::Advance { durable, source: advance })
                }
                Ok(ReopenedAdvance::Failed { source, .. }) | Err(source) => {
                    Err(BoundSystemTriggerAdvanceFailure::AdvanceAndReopen {
                        advance,
                        reopen: source,
                    })
                }
            };
        }
    };
    coordinator.record = successor.clone();
    before_bound_successor_same_store_validation(successor.phase);
    if let Err(validation) = require_system_trigger_same_store_evidence(
        &coordinator,
        metadata,
        provenance,
        authority,
        readiness,
        &successor_binding,
    ) {
        return match reopen_and_classify(
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            &predecessor,
            &successor,
            Some(&successor_binding),
        ) {
            Ok(ReopenedAdvance::Proven { durable, .. }) => Err(BoundSystemTriggerAdvanceFailure::Validation {
                durable,
                stage: BoundValidationStage::SameStore,
                source: validation,
            }),
            Ok(ReopenedAdvance::Failed { source: reopen, .. }) | Err(reopen) => {
                Err(BoundSystemTriggerAdvanceFailure::ValidationAndReopen {
                    stage: BoundValidationStage::SameStore,
                    validation,
                    reopen,
                })
            }
        };
    }

    after_bound_successor_same_store_validation(successor.phase);
    match reopen_and_classify(
        coordinator,
        metadata,
        provenance,
        authority,
        readiness,
        &predecessor,
        &successor,
        Some(&successor_binding),
    ) {
        Ok(ReopenedAdvance::Proven {
            coordinator,
            binding,
            durable: DurableSystemTriggerRecord::Successor,
        }) => Ok((coordinator, binding)),
        Ok(ReopenedAdvance::Proven {
            durable: DurableSystemTriggerRecord::Predecessor,
            ..
        }) => Err(BoundSystemTriggerAdvanceFailure::Validation {
            durable: DurableSystemTriggerRecord::Predecessor,
            stage: BoundValidationStage::CanonicalReopen,
            source: StatefulTransitionCoordinatorError::CanonicalRecordChanged {
                transition_id: successor.transition_id.clone(),
                expected_phase: successor.phase,
                actual: Some(predecessor),
            },
        }),
        Ok(ReopenedAdvance::Failed { durable, source }) => {
            Err(BoundSystemTriggerAdvanceFailure::Validation {
                durable,
                stage: BoundValidationStage::CanonicalReopen,
                source,
            })
        }
        Err(reopen) => Err(BoundSystemTriggerAdvanceFailure::ValidationAndReopen {
            stage: BoundValidationStage::CanonicalReopen,
            validation: StatefulTransitionCoordinatorError::CanonicalRecordBindingChanged {
                transition_id: successor.transition_id.clone(),
                expected_phase: successor.phase,
            },
            reopen,
        }),
    }
}

enum ReopenedAdvance {
    Proven {
        coordinator: StatefulTransitionCoordinator,
        binding: TransitionJournalRecordBinding,
        durable: DurableSystemTriggerRecord,
    },
    Failed {
        durable: DurableSystemTriggerRecord,
        source: StatefulTransitionCoordinatorError,
    },
}

fn reopen_and_classify(
    coordinator: StatefulTransitionCoordinator,
    metadata: &CandidateMetadataProof,
    provenance: &db::state::MetadataProvenance,
    authority: &crate::client::PublishedJournalRootAbiAuthority,
    readiness: &UsrExchangeReadiness,
    predecessor: &TransitionRecord,
    successor: &TransitionRecord,
    old_successor_binding: Option<&TransitionJournalRecordBinding>,
) -> Result<ReopenedAdvance, StatefulTransitionCoordinatorError> {
    let installation = authority.installation();
    let identity = reopen_identity_journal(coordinator.identity, installation, successor.phase)?;
    let actual = identity.journal.load()?;
    let (record, durable) = match actual {
        Some(actual) if actual == *predecessor => (actual, DurableSystemTriggerRecord::Predecessor),
        Some(actual) if actual == *successor => (actual, DurableSystemTriggerRecord::Successor),
        actual => {
            return Err(StatefulTransitionCoordinatorError::CanonicalRecordChanged {
                transition_id: successor.transition_id.clone(),
                expected_phase: successor.phase,
                actual,
            });
        }
    };
    let coordinator = StatefulTransitionCoordinator { identity, record };
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(IdentityError::from)
        .map_err(StatefulTransitionCoordinatorError::Identity)?;

    if durable == DurableSystemTriggerRecord::Successor
        && let Some(old_binding) = old_successor_binding
        && !coordinator
            .identity
            .journal
            .has_reopened_record_binding(cast, old_binding, &coordinator.record)?
    {
        return Ok(ReopenedAdvance::Failed {
            durable,
            source: StatefulTransitionCoordinatorError::CanonicalRecordBindingChanged {
                transition_id: coordinator.record.transition_id.clone(),
                expected_phase: coordinator.record.phase,
            },
        });
    }

    let binding = coordinator
        .identity
        .journal
        .record_binding(cast, &coordinator.record)?;
    before_reopened_fresh_binding_validation(coordinator.record.phase);
    if let Err(source) = require_system_trigger_same_store_evidence(
        &coordinator,
        metadata,
        provenance,
        authority,
        readiness,
        &binding,
    ) {
        return Ok(ReopenedAdvance::Failed { durable, source });
    }
    if durable == DurableSystemTriggerRecord::Successor
        && let Some(old_binding) = old_successor_binding
        && !coordinator
            .identity
            .journal
            .has_reopened_record_binding(cast, old_binding, &coordinator.record)?
    {
        return Ok(ReopenedAdvance::Failed {
            durable,
            source: StatefulTransitionCoordinatorError::CanonicalRecordBindingChanged {
                transition_id: coordinator.record.transition_id.clone(),
                expected_phase: coordinator.record.phase,
            },
        });
    }
    Ok(ReopenedAdvance::Proven {
        coordinator,
        binding,
        durable,
    })
}

fn reopen_identity_journal(
    identity: StatefulTreeIdentity,
    installation: &Installation,
    successor_phase: Phase,
) -> Result<StatefulTreeIdentity, StatefulTransitionCoordinatorError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(IdentityError::from)
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    let StatefulTreeIdentity {
        journal,
        state_database,
        candidate,
        candidate_state_id,
        previous,
        previous_classification,
        quarantine_attempt,
        previous_archive_attempt,
        archived_candidate_attempt,
        active_reblit_rotation,
        active_previous_slot_parking,
    } = identity;
    drop(journal);
    after_old_journal_drop_before_reopen(successor_phase);
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(IdentityError::from)
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    // The writer-first authority in `PublishedJournalRootAbiAuthority` is
    // still held. Never block behind a journal contender which may itself be
    // waiting for that writer lease.
    let journal = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(IdentityError::from)
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    Ok(StatefulTreeIdentity {
        journal,
        state_database,
        candidate,
        candidate_state_id,
        previous,
        previous_classification,
        quarantine_attempt,
        previous_archive_attempt,
        archived_candidate_attempt,
        active_reblit_rotation,
        active_previous_slot_parking,
    })
}

fn require_system_trigger_same_store_evidence(
    coordinator: &StatefulTransitionCoordinator,
    metadata: &CandidateMetadataProof,
    provenance: &db::state::MetadataProvenance,
    authority: &crate::client::PublishedJournalRootAbiAuthority,
    readiness: &UsrExchangeReadiness,
    binding: &TransitionJournalRecordBinding,
) -> Result<(), StatefulTransitionCoordinatorError> {
    let seal = UsrExchangeEffectSeal { _private: () };
    require_same_store_record_binding(coordinator, authority, binding)?;
    require_published_root_abi_sandwich(
        coordinator,
        metadata,
        provenance,
        readiness,
        authority,
        &seal,
    )?;
    require_same_store_record_binding(coordinator, authority, binding)
}

fn require_same_store_record_binding(
    coordinator: &StatefulTransitionCoordinator,
    authority: &crate::client::PublishedJournalRootAbiAuthority,
    binding: &TransitionJournalRecordBinding,
) -> Result<(), StatefulTransitionCoordinatorError> {
    let cast = authority
        .installation()
        .retained_mutable_cast_directory()
        .map_err(IdentityError::from)
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    if coordinator
        .identity
        .journal
        .has_record_binding(cast, binding, &coordinator.record)?
    {
        Ok(())
    } else {
        Err(StatefulTransitionCoordinatorError::CanonicalRecordBindingChanged {
            transition_id: coordinator.record.transition_id.clone(),
            expected_phase: coordinator.record.phase,
        })
    }
}

fn system_trigger_isolation_view(
    readiness: &UsrExchangeReadiness,
) -> (&Installation, &crate::client::RetainedRootAbi) {
    match readiness {
        UsrExchangeReadiness::TransactionTriggers(readiness) => readiness.isolation_view(),
        UsrExchangeReadiness::Archived => {
            unreachable!("archived isolation is rejected before system-trigger journal intent")
        }
    }
}

impl SystemTriggersCompleteCoordinator {
    #[cfg(test)]
    pub(crate) fn record(&self) -> &TransitionRecord {
        &self.coordinator.record
    }

    #[cfg(test)]
    pub(crate) fn revalidate_retained_authorities(&self) -> Result<(), StatefulTransitionCoordinatorError> {
        require_system_trigger_same_store_evidence(
            &self.coordinator,
            &self.metadata,
            &self.provenance,
            &self.authority,
            &self.readiness,
            &self.record_binding,
        )
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_SUCCESSOR_SAME_STORE: std::cell::RefCell<Option<(Phase, Box<dyn FnOnce()>)>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_SUCCESSOR_SAME_STORE: std::cell::RefCell<Option<(Phase, Box<dyn FnOnce()>)>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_REOPENED_FRESH_BINDING: std::cell::RefCell<Option<(Phase, Box<dyn FnOnce()>)>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_OLD_JOURNAL_DROP: std::cell::RefCell<Option<(Phase, Box<dyn FnOnce()>)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_before_bound_successor_same_store_validation(phase: Phase, hook: impl FnOnce() + 'static) {
    BEFORE_SUCCESSOR_SAME_STORE.with(|slot| {
        assert!(slot.borrow_mut().replace((phase, Box::new(hook))).is_none());
    });
}

#[cfg(test)]
pub(super) fn arm_after_bound_successor_same_store_validation(phase: Phase, hook: impl FnOnce() + 'static) {
    AFTER_SUCCESSOR_SAME_STORE.with(|slot| {
        assert!(slot.borrow_mut().replace((phase, Box::new(hook))).is_none());
    });
}

#[cfg(test)]
pub(super) fn arm_before_reopened_fresh_binding_validation(phase: Phase, hook: impl FnOnce() + 'static) {
    BEFORE_REOPENED_FRESH_BINDING.with(|slot| {
        assert!(slot.borrow_mut().replace((phase, Box::new(hook))).is_none());
    });
}

#[cfg(test)]
pub(super) fn arm_after_old_journal_drop_before_reopen(phase: Phase, hook: impl FnOnce() + 'static) {
    AFTER_OLD_JOURNAL_DROP.with(|slot| {
        assert!(slot.borrow_mut().replace((phase, Box::new(hook))).is_none());
    });
}

fn run_phase_hook(
    slot: &'static std::thread::LocalKey<std::cell::RefCell<Option<(Phase, Box<dyn FnOnce()>)>>>,
    phase: Phase,
) {
    #[cfg(test)]
    {
        let hook = slot.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|(expected, _)| *expected == phase) {
                slot.take().map(|(_, hook)| hook)
            } else {
                None
            }
        });
        if let Some(hook) = hook {
            hook();
        }
    }
    #[cfg(not(test))]
    let _ = (slot, phase);
}

fn before_bound_successor_same_store_validation(phase: Phase) {
    #[cfg(test)]
    run_phase_hook(&BEFORE_SUCCESSOR_SAME_STORE, phase);
    #[cfg(not(test))]
    let _ = phase;
}

fn after_bound_successor_same_store_validation(phase: Phase) {
    #[cfg(test)]
    run_phase_hook(&AFTER_SUCCESSOR_SAME_STORE, phase);
    #[cfg(not(test))]
    let _ = phase;
}

fn before_reopened_fresh_binding_validation(phase: Phase) {
    #[cfg(test)]
    run_phase_hook(&BEFORE_REOPENED_FRESH_BINDING, phase);
    #[cfg(not(test))]
    let _ = phase;
}

fn after_old_journal_drop_before_reopen(phase: Phase) {
    #[cfg(test)]
    run_phase_hook(&AFTER_OLD_JOURNAL_DROP, phase);
    #[cfg(not(test))]
    let _ = phase;
}
