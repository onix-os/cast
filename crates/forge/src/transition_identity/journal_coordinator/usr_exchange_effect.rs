//! Coordinator-owned, intentionally unwired `/usr` exchange effect.
//!
//! The intent module remains proof-only.  This private boundary consumes the
//! exact client capabilities, performs one forward exchange attempt, always
//! reconciles both retained tree names, completes both parent durability
//! barriers for an applied layout, and only then records `UsrExchanged`.
//!
//! ActiveReblit replacement reservation and second-link parking are completed
//! by the sealed pre-trigger typestate. This boundary only revalidates that
//! exact aggregate reservation in staged and live directions; it never repeats
//! either monotonic namespace effect.

use thiserror::Error;

use crate::{
    client::{AppliedJournalUsrExchangeAuthority, JournalUsrExchangeAuthority},
    db,
    state::{self, TransitionId},
    transition_journal::Phase,
};

use super::super::{CandidateMetadataProof, RetainedExchangeFailure, RetainedExchangeOutcome};
use super::{
    StatefulTransitionCoordinator, StatefulTransitionCoordinatorError, UsrExchangeEffectSeal,
    UsrExchangeIntentCoordinator, usr_exchange_intent::UsrExchangeReadiness,
};

const EXECUTE_USR_EXCHANGE: &str = "execute coordinator-owned /usr exchange";

/// Sole in-process owner after an applied exchange and durable journal
/// completion.  Every proof and client capability is mandatory.  The active
/// state field inside `authority` is only an opaque writer guard now; root ABI
/// publication remains a later phase.
#[derive(Debug)]
pub(crate) struct UsrExchangedCoordinator {
    coordinator: StatefulTransitionCoordinator,
    metadata: CandidateMetadataProof,
    provenance: db::state::MetadataProvenance,
    authority: AppliedJournalUsrExchangeAuthority,
    readiness: UsrExchangeReadiness,
}

/// Fail-stop result of the one-shot exchange effect.  No error retains an
/// authority or coordinator, so a caller cannot retry, reverse, or clean up in
/// process after an uncertain namespace result.
#[derive(Debug, Error)]
pub(super) enum UsrExchangeEffectFailure {
    #[error("transition {transition_id} failed /usr exchange preflight")]
    Preflight {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error("transition {transition_id} one-shot /usr exchange reconciled as {outcome:?}")]
    Exchange {
        transition_id: TransitionId,
        outcome: RetainedExchangeOutcome,
        #[source]
        source: RetainedExchangeFailure,
    },
    #[error("transition {transition_id} applied /usr exchange failed final retained evidence")]
    PostEffectEvidence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error(
        "transition {transition_id} could not durably publish /usr exchange completion; UsrExchangeIntent or UsrExchanged may be canonical"
    )]
    CompletionPersistence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
}

impl UsrExchangeIntentCoordinator {
    /// Consume intent plus every client capability and attempt the exchange
    /// exactly once.  This method is private to the unwired contract module.
    pub(super) fn execute_usr_exchange(
        self,
        authority: JournalUsrExchangeAuthority,
    ) -> Result<UsrExchangedCoordinator, UsrExchangeEffectFailure> {
        let Self {
            mut coordinator,
            metadata,
            provenance,
            readiness,
        } = self;
        let transition_id = coordinator.record.transition_id.clone();
        let seal = UsrExchangeEffectSeal { _private: () };

        if let Err(source) =
            require_pre_exchange_sandwich(&coordinator, &metadata, &provenance, &readiness, &authority, &seal)
        {
            return Err(UsrExchangeEffectFailure::Preflight { transition_id, source });
        }

        let installation = authority.installation();
        let exchange = coordinator
            .identity
            .exchange_forward_with_journal(installation, &seal, &|| {
                require_pre_exchange_sandwich(&coordinator, &metadata, &provenance, &readiness, &authority, &seal)
            });
        if let Err(source) = exchange {
            return Err(UsrExchangeEffectFailure::Exchange {
                transition_id,
                outcome: source.outcome(),
                source,
            });
        }

        let authority = authority.into_applied();
        if let Err(source) =
            require_applied_exchange_sandwich(&coordinator, &metadata, &provenance, &readiness, &authority, &seal)
        {
            return Err(UsrExchangeEffectFailure::PostEffectEvidence { transition_id, source });
        }

        let complete = coordinator
            .record
            .forward_successor(None)
            .map_err(StatefulTransitionCoordinatorError::from)
            .map_err(|source| UsrExchangeEffectFailure::PostEffectEvidence {
                transition_id: transition_id.clone(),
                source,
            })?;
        if complete.phase != Phase::UsrExchanged {
            return Err(UsrExchangeEffectFailure::PostEffectEvidence {
                transition_id,
                source: StatefulTransitionCoordinatorError::UnexpectedPhase {
                    action: EXECUTE_USR_EXCHANGE,
                    expected: Phase::UsrExchanged,
                    actual: complete.phase,
                },
            });
        }
        if let Err(source) = coordinator.identity.journal.advance(&coordinator.record, &complete) {
            return Err(UsrExchangeEffectFailure::CompletionPersistence {
                transition_id,
                source: source.into(),
            });
        }
        coordinator.record = complete;
        Ok(UsrExchangedCoordinator {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
        })
    }
}

fn require_pre_exchange_sandwich(
    coordinator: &StatefulTransitionCoordinator,
    metadata: &CandidateMetadataProof,
    provenance: &db::state::MetadataProvenance,
    readiness: &UsrExchangeReadiness,
    authority: &JournalUsrExchangeAuthority,
    seal: &UsrExchangeEffectSeal,
) -> Result<(), StatefulTransitionCoordinatorError> {
    coordinator.require_phase(Phase::UsrExchangeIntent, EXECUTE_USR_EXCHANGE)?;
    let candidate = coordinator.candidate_state()?;
    let previous = coordinator.record.previous.id.map(state::Id::from);
    authority.require_pre_exchange(coordinator.record.operation, candidate, previous)?;
    coordinator.seal_prepared_candidate()?;
    coordinator.require_prepared_metadata_sandwich(candidate, metadata, provenance)?;
    readiness.require_staged(&coordinator.identity)?;
    require_active_reblit_snapshot(
        coordinator,
        authority.active_reblit(),
        authority.installation(),
        seal,
        false,
    )?;
    authority.require_pre_exchange(coordinator.record.operation, candidate, previous)?;
    coordinator.require_prepared_metadata_sandwich(candidate, metadata, provenance)?;
    readiness.require_staged(&coordinator.identity)
}

pub(super) fn require_applied_exchange_sandwich(
    coordinator: &StatefulTransitionCoordinator,
    metadata: &CandidateMetadataProof,
    provenance: &db::state::MetadataProvenance,
    readiness: &UsrExchangeReadiness,
    authority: &AppliedJournalUsrExchangeAuthority,
    seal: &UsrExchangeEffectSeal,
) -> Result<(), StatefulTransitionCoordinatorError> {
    coordinator.seal_prepared_candidate()?;
    require_applied_exchange_evidence(coordinator, metadata, provenance, readiness, authority, seal)?;
    require_applied_exchange_evidence(coordinator, metadata, provenance, readiness, authority, seal)
}

fn require_applied_exchange_evidence(
    coordinator: &StatefulTransitionCoordinator,
    metadata: &CandidateMetadataProof,
    provenance: &db::state::MetadataProvenance,
    readiness: &UsrExchangeReadiness,
    authority: &AppliedJournalUsrExchangeAuthority,
    seal: &UsrExchangeEffectSeal,
) -> Result<(), StatefulTransitionCoordinatorError> {
    coordinator.require_canonical_record()?;
    coordinator.require_record_runtime_evidence()?;
    authority.require_post_exchange()?;
    let candidate = coordinator.candidate_state()?;
    let installation = authority.installation();
    coordinator
        .identity
        .verify_candidate_named_with_state_id(&installation.root.join("usr"))
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    coordinator
        .identity
        .previous
        .verify_named_read_only(&installation.staging_path("usr"))
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    coordinator.require_candidate_database_evidence(candidate)?;
    coordinator
        .identity
        .state_database
        .require_exact_metadata_provenance(candidate, provenance)?;
    metadata.require_same_candidate(
        coordinator.identity.candidate.store.retained_directory(),
        &installation.root.join("usr"),
    )?;
    let (os_release, system_model) = metadata.policy_output_bytes();
    provenance.require_outputs(candidate, os_release, system_model)?;
    require_active_reblit_snapshot(coordinator, authority.active_reblit(), installation, seal, true)?;
    readiness.require_live(&coordinator.identity)?;
    authority.require_post_exchange()?;
    coordinator.require_record_runtime_evidence()?;
    coordinator.require_canonical_record()?;
    readiness.require_live(&coordinator.identity)
}

pub(super) fn require_active_reblit_snapshot(
    coordinator: &StatefulTransitionCoordinator,
    active_reblit: Option<&crate::State>,
    installation: &crate::Installation,
    seal: &UsrExchangeEffectSeal,
    live: bool,
) -> Result<(), StatefulTransitionCoordinatorError> {
    if let Some(active_reblit) = active_reblit {
        coordinator
            .identity
            .verify_journal_active_reblit_snapshot(seal, installation, active_reblit, live)
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
    }
    Ok(())
}

impl UsrExchangedCoordinator {
    pub(super) fn into_root_abi_publication_parts(
        self,
    ) -> (
        StatefulTransitionCoordinator,
        CandidateMetadataProof,
        db::state::MetadataProvenance,
        AppliedJournalUsrExchangeAuthority,
        UsrExchangeReadiness,
    ) {
        (
            self.coordinator,
            self.metadata,
            self.provenance,
            self.authority,
            self.readiness,
        )
    }

    #[cfg(test)]
    pub(crate) fn record(&self) -> &crate::transition_journal::TransitionRecord {
        let _metadata = &self.metadata;
        let _provenance = &self.provenance;
        let _authority = &self.authority;
        let _readiness = &self.readiness;
        &self.coordinator.record
    }

    #[cfg(test)]
    pub(crate) fn revalidate_retained_authorities(&self) -> Result<(), StatefulTransitionCoordinatorError> {
        let seal = UsrExchangeEffectSeal { _private: () };
        require_applied_exchange_sandwich(
            &self.coordinator,
            &self.metadata,
            &self.provenance,
            &self.readiness,
            &self.authority,
            &seal,
        )
    }
}
