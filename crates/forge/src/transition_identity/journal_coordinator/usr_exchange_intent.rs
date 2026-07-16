//! Proof-bearing, intentionally effect-free `/usr` exchange intent.
//!
//! New states and active reblits can enter this boundary only after their
//! transaction triggers are durably complete. Archived activation enters from
//! its distinct `CandidatePrepared` typestate and therefore cannot acquire
//! transaction-trigger authority. Both paths retain the exact metadata proof,
//! reseal the prepared candidate, repeat the complete journal/runtime/name/
//! state-ID/database evidence sandwich, and conditionally publish only
//! `UsrExchangeIntent`.
//!
//! This module deliberately owns no exchange syscall, installation root,
//! active-state lease, or merged-/usr root-link capability. Those authorities
//! are required by the later exchange-effect boundary. In particular, the
//! legacy no-journal exchange methods must not be reused or weakened here.

use thiserror::Error;

use crate::{db, state::TransitionId, transition_journal::Phase};

use super::super::CandidateMetadataProof;
use super::{
    PreparedArchivedTransitionCoordinator, StatefulTransitionCoordinator, StatefulTransitionCoordinatorError,
    TransactionTriggersCompleteCoordinator,
};

const BEGIN_USR_EXCHANGE_INTENT: &str = "begin /usr exchange intent";

/// Sole in-process owner of one durable `UsrExchangeIntent` record.
///
/// The private fields make this a journal-intent typestate, not a caller-
/// forgeable exchange capability. A later effect must add and revalidate the
/// retained installation, active-state, and root-ABI authorities before it can
/// mutate either public `/usr` name.
#[derive(Debug)]
pub(crate) struct UsrExchangeIntentCoordinator {
    pub(super) coordinator: StatefulTransitionCoordinator,
    pub(super) metadata: CandidateMetadataProof,
    pub(super) provenance: db::state::MetadataProvenance,
}

/// Fail-stop result of publishing the `/usr` exchange intent.
///
/// No variant retains the coordinator, metadata proof, filesystem descriptor,
/// database, or journal store. A returned error therefore releases every
/// coordinator-owned authority before startup assessment can reopen the exact
/// canonical record.
#[derive(Debug, Error)]
pub(super) enum UsrExchangeIntentFailure {
    #[error("transition {transition_id} failed /usr exchange-intent preflight from {predecessor:?}")]
    Preflight {
        transition_id: TransitionId,
        predecessor: Phase,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error(
        "transition {transition_id} could not durably publish /usr exchange intent; {predecessor:?} or UsrExchangeIntent may be canonical"
    )]
    IntentPersistence {
        transition_id: TransitionId,
        predecessor: Phase,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
}

impl TransactionTriggersCompleteCoordinator {
    /// Persist `/usr` exchange intent for a new state or active reblit without
    /// performing the exchange or exposing its retained authorities.
    pub(super) fn begin_usr_exchange_intent(self) -> Result<UsrExchangeIntentCoordinator, UsrExchangeIntentFailure> {
        let Self {
            coordinator,
            metadata,
            provenance,
        } = self;
        begin_usr_exchange_intent(coordinator, metadata, provenance, Phase::TransactionTriggersComplete)
    }
}

impl PreparedArchivedTransitionCoordinator {
    /// Persist `/usr` exchange intent directly from archived
    /// `CandidatePrepared`; archived activation never runs transaction
    /// triggers.
    pub(super) fn begin_usr_exchange_intent(self) -> Result<UsrExchangeIntentCoordinator, UsrExchangeIntentFailure> {
        let Self {
            coordinator,
            metadata,
            provenance,
        } = self;
        begin_usr_exchange_intent(coordinator, metadata, provenance, Phase::CandidatePrepared)
    }
}

fn begin_usr_exchange_intent(
    mut coordinator: StatefulTransitionCoordinator,
    metadata: CandidateMetadataProof,
    provenance: db::state::MetadataProvenance,
    predecessor: Phase,
) -> Result<UsrExchangeIntentCoordinator, UsrExchangeIntentFailure> {
    let transition_id = coordinator.record.transition_id.clone();
    let preflight = |source| UsrExchangeIntentFailure::Preflight {
        transition_id: transition_id.clone(),
        predecessor,
        source,
    };

    coordinator
        .require_phase(predecessor, BEGIN_USR_EXCHANGE_INTENT)
        .map_err(preflight)?;
    let candidate = coordinator.candidate_state().map_err(preflight)?;
    let intent = coordinator
        .record
        .forward_successor(None)
        .map_err(StatefulTransitionCoordinatorError::from)
        .map_err(preflight)?;
    if intent.phase != Phase::UsrExchangeIntent {
        return Err(preflight(StatefulTransitionCoordinatorError::UnexpectedPhase {
            action: BEGIN_USR_EXCHANGE_INTENT,
            expected: Phase::UsrExchangeIntent,
            actual: intent.phase,
        }));
    }

    // The earlier trigger-completion or archived-preparation proof may have
    // been retained for an arbitrary time. Reseal the exact candidate, then
    // make the metadata proof the final observation in the full evidence
    // sandwich immediately before the conditional journal update.
    coordinator.seal_prepared_candidate().map_err(preflight)?;
    coordinator
        .require_prepared_metadata_sandwich(candidate, &metadata, &provenance)
        .map_err(preflight)?;

    if let Err(source) = coordinator.identity.journal.advance(&coordinator.record, &intent) {
        return Err(UsrExchangeIntentFailure::IntentPersistence {
            transition_id,
            predecessor,
            source: source.into(),
        });
    }
    coordinator.record = intent;
    Ok(UsrExchangeIntentCoordinator {
        coordinator,
        metadata,
        provenance,
    })
}

impl UsrExchangeIntentCoordinator {
    #[cfg(test)]
    pub(crate) fn record(&self) -> &crate::transition_journal::TransitionRecord {
        // Borrowing the proof prevents this test-only accessor from becoming a
        // proof-free typestate shortcut.
        let _metadata = &self.metadata;
        let _provenance = &self.provenance;
        &self.coordinator.record
    }
}
