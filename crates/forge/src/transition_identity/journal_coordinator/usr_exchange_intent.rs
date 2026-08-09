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
    StatefulTransitionCoordinator, StatefulTransitionCoordinatorError, TransactionTriggerReadiness,
    TransactionTriggersCompleteCoordinator, candidate_preparation::PreparedArchivedIsolationCoordinator,
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
    pub(super) readiness: UsrExchangeReadiness,
}

#[derive(Debug)]
pub(super) enum UsrExchangeReadiness {
    TransactionTriggers(TransactionTriggerReadiness),
    /// Archived activation runs no transaction triggers, but still runs system
    /// triggers, which need an isolation root. It acquires its own rather than
    /// accepting one from the client, so the retained-capability guarantee is
    /// structural for all three operations rather than two
    /// (`plans/close_out.md`, decided 2026-07-29).
    Archived(super::transaction_isolation::RetainedTransactionIsolationAbi),
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
            readiness,
        } = self;
        begin_usr_exchange_intent(
            coordinator,
            metadata,
            provenance,
            UsrExchangeReadiness::TransactionTriggers(readiness),
            Phase::TransactionTriggersComplete,
        )
    }
}

impl PreparedArchivedIsolationCoordinator {
    /// Persist `/usr` exchange intent directly from archived
    /// `CandidatePrepared`; archived activation never runs transaction
    /// triggers, but carries the isolation ABI its system triggers need.
    pub(super) fn begin_usr_exchange_intent(self) -> Result<UsrExchangeIntentCoordinator, UsrExchangeIntentFailure> {
        let Self {
            coordinator,
            metadata,
            provenance,
            isolation,
        } = self;
        begin_usr_exchange_intent(
            coordinator,
            metadata,
            provenance,
            UsrExchangeReadiness::Archived(isolation),
            Phase::CandidatePrepared,
        )
    }
}

fn begin_usr_exchange_intent(
    mut coordinator: StatefulTransitionCoordinator,
    metadata: CandidateMetadataProof,
    provenance: db::state::MetadataProvenance,
    readiness: UsrExchangeReadiness,
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
    require_usr_exchange_intent_sandwich(&coordinator, candidate, &metadata, &provenance, &readiness)
        .map_err(preflight)?;

    if let Err(source) = coordinator
        .identity
        .retained_journal()
        .advance(&coordinator.record, &intent)
    {
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
        readiness,
    })
}

fn require_usr_exchange_intent_sandwich(
    coordinator: &StatefulTransitionCoordinator,
    candidate: crate::state::Id,
    metadata: &CandidateMetadataProof,
    provenance: &db::state::MetadataProvenance,
    readiness: &UsrExchangeReadiness,
) -> Result<(), StatefulTransitionCoordinatorError> {
    coordinator.seal_prepared_candidate()?;
    coordinator.require_prepared_metadata_sandwich(candidate, metadata, provenance)?;
    readiness.require_staged(&coordinator.identity)?;
    coordinator.require_prepared_metadata_sandwich(candidate, metadata, provenance)?;
    readiness.require_staged(&coordinator.identity)
}

impl UsrExchangeReadiness {
    pub(super) fn require_staged(
        &self,
        identity: &super::super::StatefulTreeIdentity,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        match self {
            Self::TransactionTriggers(readiness) => readiness.require_staged(identity),
            // Was `Ok(())` when this variant carried nothing. Now that archived
            // activation retains its own isolation ABI, it is revalidated on the
            // same schedule as every other operation's.
            Self::Archived(isolation) => isolation.require_staged(identity),
        }
    }

    pub(super) fn require_live(
        &self,
        identity: &super::super::StatefulTreeIdentity,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        match self {
            Self::TransactionTriggers(readiness) => readiness.require_live(identity),
            Self::Archived(isolation) => isolation.require_live(identity),
        }
    }
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
