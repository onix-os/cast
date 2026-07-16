//! Internal, intentionally unwired transaction-trigger sequencing contract.
//!
//! The callback proves journal ordering, retained identity, database evidence,
//! and post-effect durability, but it does not yet own the generated candidate
//! metadata proof used by the live client. Keep this module's authority,
//! failure, and runner visibility scoped to `journal_coordinator`. Widening it
//! is unsafe until candidate preparation supplies an owned metadata token and
//! this boundary requires that token before intent and after the effect.

use std::{error::Error as StdError, fs::File, path::Path};

use thiserror::Error;

use crate::{
    state::{self, TransitionId},
    transition_journal::{Operation, Phase},
};

use super::super::prejournal_inventory::{CandidateInventoryLimits, seal_existing_marked_candidate};
use super::{StatefulTransitionCoordinator, StatefulTransitionCoordinatorError};

const RUN_TRANSACTION_TRIGGERS: &str = "run stateful transaction triggers";
const COMPLETE_TRANSACTION_TRIGGERS: &str = "complete stateful transaction triggers";

/// Narrow, callback-scoped authority for one stateful transaction-trigger run.
///
/// The authority borrows the coordinator's exact retained candidate and does
/// not expose the journal, database, previous tree, or any lifecycle mutation
/// method. It is neither cloneable nor copyable. The runner requires a
/// `'static` effect error, so the borrowed authority itself cannot escape
/// through a returned failure. Trusted effect code can still duplicate the
/// exposed file descriptor; this contract does not claim to revoke such a
/// deliberately derived descriptor.
#[derive(Debug)]
pub(super) struct StatefulTransactionTriggerAuthority<'authority> {
    transition_id: &'authority TransitionId,
    candidate_state: state::Id,
    candidate_usr: &'authority File,
    candidate_usr_path: &'authority Path,
}

impl<'authority> StatefulTransactionTriggerAuthority<'authority> {
    pub(super) fn transition_id(&self) -> &'authority TransitionId {
        self.transition_id
    }

    pub(super) fn candidate_state(&self) -> state::Id {
        self.candidate_state
    }

    /// Borrow the exact candidate descriptor selected before journal creation.
    /// The path is diagnostic only and must never replace descriptor-rooted
    /// trigger discovery or container binding.
    pub(super) fn retained_candidate_usr(&self) -> (&'authority File, &'authority Path) {
        (self.candidate_usr, self.candidate_usr_path)
    }
}

/// Fail-stop result of attempting one stateful transaction-trigger boundary.
///
/// No variant stores any coordinator-owned identity, journal store, database
/// capability, or trigger authority. The generic effect source is required to
/// be `'static` and therefore cannot borrow the callback authority. Returning
/// any failure releases the coordinator and its journal/database locks before
/// a caller can begin startup assessment; it does not revoke a file descriptor
/// which trusted effect code explicitly duplicated while the callback ran.
#[derive(Debug, Error)]
pub(super) enum StatefulTransactionTriggerFailure<E>
where
    E: StdError + 'static,
{
    #[error("transition {transition_id} operation {operation:?} has no stateful transaction-trigger phase")]
    NotApplicable {
        transition_id: TransitionId,
        operation: Operation,
    },
    #[error("transition {transition_id} failed transaction-trigger preflight")]
    Preflight {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error(
        "transition {transition_id} could not durably publish transaction-trigger intent; CandidatePrepared or TransactionTriggersStarted may be canonical"
    )]
    IntentPersistence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error(
        "transition {transition_id} transaction-trigger effect failed after durable intent; external effects may remain"
    )]
    Effect {
        transition_id: TransitionId,
        #[source]
        source: E,
    },
    #[error("transition {transition_id} failed post-trigger evidence or durability proof; external effects may remain")]
    PostEffectEvidence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error(
        "transition {transition_id} could not durably publish transaction-trigger completion; TransactionTriggersStarted or TransactionTriggersComplete may be canonical"
    )]
    CompletionPersistence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
}

impl StatefulTransitionCoordinator {
    /// Persist transaction-trigger intent, invoke one authorized effect, seal
    /// its exact candidate result, and persist completion.
    ///
    /// `NewState` and `ActiveReblit` are the only operations whose journal
    /// successor is `TransactionTriggersStarted`. `ActivateArchived` is
    /// rejected without advancing its record or invoking `effect`; its legal
    /// successor belongs to the later `/usr`-exchange slice.
    pub(super) fn run_transaction_triggers<E, F>(
        mut self,
        effect: F,
    ) -> Result<Self, StatefulTransactionTriggerFailure<E>>
    where
        E: StdError + 'static,
        F: for<'authority> FnOnce(StatefulTransactionTriggerAuthority<'authority>) -> Result<(), E>,
    {
        let transition_id = self.record.transition_id.clone();
        if let Err(source) = self.require_phase(Phase::CandidatePrepared, RUN_TRANSACTION_TRIGGERS) {
            return Err(StatefulTransactionTriggerFailure::Preflight { transition_id, source });
        }

        let candidate = match self.candidate_state() {
            Ok(candidate) => candidate,
            Err(source) => {
                return Err(StatefulTransactionTriggerFailure::Preflight { transition_id, source });
            }
        };
        let started = match self.record.forward_successor(None) {
            Ok(started) => started,
            Err(source) => {
                return Err(StatefulTransactionTriggerFailure::Preflight {
                    transition_id,
                    source: source.into(),
                });
            }
        };
        if started.phase != Phase::TransactionTriggersStarted {
            return Err(StatefulTransactionTriggerFailure::NotApplicable {
                transition_id,
                operation: self.record.operation,
            });
        }

        if let Err(source) = self.require_transaction_trigger_evidence(candidate) {
            return Err(StatefulTransactionTriggerFailure::Preflight { transition_id, source });
        }
        if let Err(source) = self.seal_transaction_candidate() {
            return Err(StatefulTransactionTriggerFailure::Preflight { transition_id, source });
        }
        if let Err(source) = self.require_transaction_trigger_evidence(candidate) {
            return Err(StatefulTransactionTriggerFailure::Preflight { transition_id, source });
        }

        if let Err(source) = self.identity.journal.advance(&self.record, &started) {
            return Err(StatefulTransactionTriggerFailure::IntentPersistence {
                transition_id,
                source: source.into(),
            });
        }
        self.record = started;

        let authority = StatefulTransactionTriggerAuthority {
            transition_id: &self.record.transition_id,
            candidate_state: candidate,
            candidate_usr: self.identity.candidate.store.retained_directory(),
            candidate_usr_path: self.identity.candidate.store.display_path(),
        };
        if let Err(source) = effect(authority) {
            return Err(StatefulTransactionTriggerFailure::Effect { transition_id, source });
        }

        if let Err(source) = self.require_transaction_trigger_evidence(candidate) {
            return Err(StatefulTransactionTriggerFailure::PostEffectEvidence { transition_id, source });
        }
        if let Err(source) = self.seal_transaction_candidate() {
            return Err(StatefulTransactionTriggerFailure::PostEffectEvidence { transition_id, source });
        }
        if let Err(source) = self.require_transaction_trigger_evidence(candidate) {
            return Err(StatefulTransactionTriggerFailure::PostEffectEvidence { transition_id, source });
        }

        let complete = match self.record.forward_successor(None) {
            Ok(complete) => complete,
            Err(source) => {
                return Err(StatefulTransactionTriggerFailure::PostEffectEvidence {
                    transition_id,
                    source: source.into(),
                });
            }
        };
        if complete.phase != Phase::TransactionTriggersComplete {
            return Err(StatefulTransactionTriggerFailure::PostEffectEvidence {
                transition_id,
                source: StatefulTransitionCoordinatorError::UnexpectedPhase {
                    action: COMPLETE_TRANSACTION_TRIGGERS,
                    expected: Phase::TransactionTriggersComplete,
                    actual: complete.phase,
                },
            });
        }
        if let Err(source) = self.identity.journal.advance(&self.record, &complete) {
            return Err(StatefulTransactionTriggerFailure::CompletionPersistence {
                transition_id,
                source: source.into(),
            });
        }
        self.record = complete;
        Ok(self)
    }

    fn candidate_state(&self) -> Result<state::Id, StatefulTransitionCoordinatorError> {
        self.record
            .candidate
            .id
            .map(state::Id::from)
            .ok_or(StatefulTransitionCoordinatorError::CandidateStateMissing {
                phase: self.record.phase,
            })
    }

    fn require_transaction_trigger_evidence(
        &self,
        candidate: state::Id,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        self.require_record_runtime_evidence()?;
        self.require_transaction_tree_names(candidate)?;
        self.require_transaction_database_evidence(candidate)?;
        self.require_record_runtime_evidence()?;
        self.require_transaction_tree_names(candidate)?;
        self.require_transaction_database_evidence(candidate)
    }

    fn require_transaction_tree_names(&self, candidate: state::Id) -> Result<(), StatefulTransitionCoordinatorError> {
        self.identity.require_existing_candidate_state(candidate)?;
        let candidate_path = self.identity.candidate.store.display_path();
        let previous_path = self.identity.previous.store.display_path();
        self.identity
            .candidate
            .verify_named_with_state_id(candidate_path)
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
        self.identity
            .previous
            .verify_named_read_only(previous_path)
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
        self.identity
            .candidate
            .verify_named_with_state_id(candidate_path)
            .map_err(StatefulTransitionCoordinatorError::Identity)
    }

    fn require_transaction_database_evidence(
        &self,
        candidate: state::Id,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        match self.record.operation {
            Operation::NewState => self.require_fresh_allocation_ownership(candidate),
            Operation::ActiveReblit => self.identity.require_existing_candidate_database_ownership(
                self.record.operation,
                candidate,
                &self.record.transition_id,
            ),
            Operation::ActivateArchived => Err(StatefulTransitionCoordinatorError::UnexpectedOperation {
                action: RUN_TRANSACTION_TRIGGERS,
                expected: Operation::ActiveReblit,
                actual: Operation::ActivateArchived,
            }),
        }?;
        let previous = self.record.previous.id.map(state::Id::from);
        self.identity.require_previous_state_database_ownership(
            self.record.operation,
            previous,
            Some(candidate),
            &self.record.transition_id,
        )?;
        let expected = match self.record.operation {
            Operation::NewState => Some((candidate, &self.record.transition_id)),
            Operation::ActiveReblit => None,
            Operation::ActivateArchived => unreachable!("archived activation was rejected before trigger evidence"),
        };
        self.identity
            .require_global_transition_audit(self.record.operation, expected)
    }

    fn seal_transaction_candidate(&self) -> Result<(), StatefulTransitionCoordinatorError> {
        seal_existing_marked_candidate(
            self.identity.candidate.store.retained_directory(),
            self.identity.candidate.store.display_path(),
            CandidateInventoryLimits::default(),
        )
        .map_err(StatefulTransitionCoordinatorError::TransactionCandidateDurability)
    }
}
