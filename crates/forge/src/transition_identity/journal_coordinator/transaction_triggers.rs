//! Proof-bearing, intentionally unwired transaction-trigger sequencing.
//!
//! Only the operation-specific `NewState`/`ActiveReblit` typestate reaches this
//! runner. It owns the exact metadata proof created during candidate
//! preparation and sandwiches that proof between complete public-name,
//! journal, runtime, state-ID, and database evidence both before intent and
//! after the effect. Archived activation has a different typestate and no path
//! into this module.

use std::{error::Error as StdError, fs::File, path::Path};

use thiserror::Error;

use crate::{
    db,
    state::{self, TransitionId},
    transition_journal::Phase,
};

use super::super::CandidateMetadataProof;
use super::super::prejournal_inventory::{CandidateInventoryLimits, seal_existing_marked_candidate};
use super::{
    PreparedTransactionTriggerCoordinator, StatefulTransitionCoordinator, StatefulTransitionCoordinatorError,
    TransactionTriggersCompleteCoordinator,
};

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

impl PreparedTransactionTriggerCoordinator {
    /// Persist transaction-trigger intent, invoke one authorized effect, seal
    /// its exact candidate result, and persist completion.
    ///
    /// Construction of this wrapper proves that the operation is `NewState`
    /// or `ActiveReblit`; archived activation is unrepresentable here.
    pub(super) fn run_transaction_triggers<E, F>(
        self,
        effect: F,
    ) -> Result<TransactionTriggersCompleteCoordinator, StatefulTransactionTriggerFailure<E>>
    where
        E: StdError + 'static,
        F: for<'authority> FnOnce(StatefulTransactionTriggerAuthority<'authority>) -> Result<(), E>,
    {
        let Self {
            mut coordinator,
            metadata,
            provenance,
        } = self;
        let transition_id = coordinator.record.transition_id.clone();
        if let Err(source) = coordinator.require_phase(Phase::CandidatePrepared, RUN_TRANSACTION_TRIGGERS) {
            return Err(StatefulTransactionTriggerFailure::Preflight { transition_id, source });
        }

        let candidate = match coordinator.candidate_state() {
            Ok(candidate) => candidate,
            Err(source) => {
                return Err(StatefulTransactionTriggerFailure::Preflight { transition_id, source });
            }
        };
        let started = match coordinator.record.forward_successor(None) {
            Ok(started) => started,
            Err(source) => {
                return Err(StatefulTransactionTriggerFailure::Preflight {
                    transition_id,
                    source: source.into(),
                });
            }
        };
        if let Err(source) = coordinator.seal_prepared_candidate() {
            return Err(StatefulTransactionTriggerFailure::Preflight { transition_id, source });
        }
        if let Err(source) = coordinator.require_prepared_metadata_sandwich(candidate, &metadata, &provenance) {
            return Err(StatefulTransactionTriggerFailure::Preflight { transition_id, source });
        }

        if let Err(source) = coordinator.identity.journal.advance(&coordinator.record, &started) {
            return Err(StatefulTransactionTriggerFailure::IntentPersistence {
                transition_id,
                source: source.into(),
            });
        }
        coordinator.record = started;

        let authority = StatefulTransactionTriggerAuthority {
            transition_id: &coordinator.record.transition_id,
            candidate_state: candidate,
            candidate_usr: coordinator.identity.candidate.store.retained_directory(),
            candidate_usr_path: coordinator.identity.candidate.store.display_path(),
        };
        if let Err(source) = effect(authority) {
            return Err(StatefulTransactionTriggerFailure::Effect { transition_id, source });
        }

        if let Err(source) = coordinator.seal_prepared_candidate() {
            return Err(StatefulTransactionTriggerFailure::PostEffectEvidence { transition_id, source });
        }
        if let Err(source) = coordinator.require_prepared_metadata_sandwich(candidate, &metadata, &provenance) {
            return Err(StatefulTransactionTriggerFailure::PostEffectEvidence { transition_id, source });
        }

        let complete = match coordinator.record.forward_successor(None) {
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
        if let Err(source) = coordinator.identity.journal.advance(&coordinator.record, &complete) {
            return Err(StatefulTransactionTriggerFailure::CompletionPersistence {
                transition_id,
                source: source.into(),
            });
        }
        coordinator.record = complete;
        Ok(TransactionTriggersCompleteCoordinator {
            coordinator,
            metadata,
            provenance,
        })
    }
}

impl StatefulTransitionCoordinator {
    /// Make the owned metadata proof the final observation in a full evidence
    /// sandwich. Repeating public-name evidence after the first proof catches
    /// substitution performed while proof revalidation traverses metadata.
    pub(super) fn require_prepared_metadata_sandwich(
        &self,
        candidate: state::Id,
        metadata: &CandidateMetadataProof,
        provenance: &db::state::MetadataProvenance,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        self.require_prepared_candidate_evidence(candidate)?;
        self.identity
            .state_database
            .require_exact_metadata_provenance(candidate, provenance)?;
        metadata.require_same_candidate(
            self.identity.candidate.store.retained_directory(),
            self.identity.candidate.store.display_path(),
        )?;
        let (os_release, system_model) = metadata.policy_output_bytes();
        provenance.require_outputs(candidate, os_release, system_model)?;
        self.require_prepared_candidate_evidence(candidate)?;
        self.identity
            .state_database
            .require_exact_metadata_provenance(candidate, provenance)?;
        metadata.require_same_candidate(
            self.identity.candidate.store.retained_directory(),
            self.identity.candidate.store.display_path(),
        )?;
        let (os_release, system_model) = metadata.policy_output_bytes();
        provenance
            .require_outputs(candidate, os_release, system_model)
            .map_err(Into::into)
    }

    pub(super) fn seal_prepared_candidate(&self) -> Result<(), StatefulTransitionCoordinatorError> {
        seal_existing_marked_candidate(
            self.identity.candidate.store.retained_directory(),
            self.identity.candidate.store.display_path(),
            CandidateInventoryLimits::default(),
        )
        .map_err(StatefulTransitionCoordinatorError::PreparedCandidateDurability)
    }
}
