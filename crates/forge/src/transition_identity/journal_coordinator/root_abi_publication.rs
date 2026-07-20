//! Coordinator-owned publication of the public merged-/usr root ABI.
//!
//! This intentionally unwired boundary consumes the exact client authority
//! captured before journal creation.  It binds the canonical `UsrExchanged`
//! record inode before the monotonic link effect, publishes through the
//! original retained root preflight, revalidates the complete exchanged
//! namespace through descriptor-pinned root-ABI evidence, and conditionally
//! advances only that bound record to `RootLinksComplete`.
//!
//! No error returns a coordinator, journal binding, writer lease, or root-ABI
//! capability.  Partial publication and uncertain journal completion are left
//! to startup reconciliation; this module never retries, reverses, triggers,
//! archives, repairs boot state, or cleans up in process.

use thiserror::Error;

use crate::{
    client::PublishedJournalRootAbiAuthority,
    db,
    state::TransitionId,
    transition_journal::{Operation, Phase, TransitionJournalRecordBinding, TransitionRecord},
};

use super::super::{CandidateMetadataProof, Error as IdentityError};
use super::{
    StatefulTransitionCoordinator, StatefulTransitionCoordinatorError, UsrExchangeEffectSeal,
    UsrExchangedCoordinator,
    usr_exchange_effect::{require_active_reblit_snapshot, require_applied_exchange_sandwich},
    usr_exchange_intent::UsrExchangeReadiness,
};

const PUBLISH_ROOT_ABI: &str = "publish coordinator-owned merged-/usr root ABI";

/// Sole in-process owner after durable `RootLinksComplete` publication.
///
/// The successor keeps the exact canonical-record inode returned by the bound
/// journal advance, the descriptor-pinned public root ABI, and every earlier
/// metadata, database, transaction-readiness, and writer authority.  A later
/// phase must consume and revalidate this complete aggregate rather than
/// reconstructing authority from public paths or equal journal bytes.
#[derive(Debug)]
pub(crate) struct RootLinksCompleteCoordinator {
    pub(super) coordinator: StatefulTransitionCoordinator,
    pub(super) metadata: CandidateMetadataProof,
    pub(super) provenance: db::state::MetadataProvenance,
    pub(super) authority: PublishedJournalRootAbiAuthority,
    pub(super) readiness: UsrExchangeReadiness,
    pub(super) record_binding: TransitionJournalRecordBinding,
}

/// Fail-stop result of public root-ABI publication and its durable completion.
#[derive(Debug, Error)]
pub(super) enum RootAbiPublicationFailure {
    #[error("transition {transition_id} failed merged-/usr root-ABI preflight")]
    Preflight {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error(
        "transition {transition_id} derived {actual_phase:?} generation {actual_generation} instead of the sole {expected_phase:?} generation {expected_generation} successor for {operation:?}"
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
        "transition {transition_id} could not publish the public merged-/usr root ABI; UsrExchanged remains canonical and publication may be partial"
    )]
    Publication {
        transition_id: TransitionId,
        #[source]
        source: crate::client::Error,
    },
    #[error("transition {transition_id} published root links but failed final retained root-ABI evidence")]
    PostEffectEvidence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error(
        "transition {transition_id} could not durably publish root-link completion; UsrExchanged or RootLinksComplete may be canonical"
    )]
    CompletionPersistence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error(
        "transition {transition_id} durably published RootLinksComplete but failed final successor-binding or retained root-ABI evidence"
    )]
    FinalEvidence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
}

impl UsrExchangedCoordinator {
    /// Consume the exchanged typestate, publish the retained public root ABI,
    /// and advance only the exact predecessor record inode captured before the
    /// physical effect.  This remains private to the unwired contract module.
    pub(super) fn publish_root_abi(self) -> Result<RootLinksCompleteCoordinator, RootAbiPublicationFailure> {
        let (mut coordinator, metadata, provenance, authority, readiness) =
            self.into_root_abi_publication_parts();
        let transition_id = coordinator.record.transition_id.clone();
        let preflight = |source| RootAbiPublicationFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };

        coordinator
            .require_phase(Phase::UsrExchanged, PUBLISH_ROOT_ABI)
            .map_err(preflight)?;
        let complete = exact_root_links_successor(&coordinator.record)?;
        let seal = UsrExchangeEffectSeal { _private: () };

        // This full sandwich is intentionally repeated from the exchange
        // boundary. The retained proofs may have been held for arbitrary time.
        // The exact record binding is captured after the sandwich and is the
        // final admission operation before the monotonic physical effect.
        require_applied_exchange_sandwich(
            &coordinator,
            &metadata,
            &provenance,
            &readiness,
            &authority,
            &seal,
        )
        .map_err(preflight)?;
        let cast = authority
            .installation()
            .retained_mutable_cast_directory()
            .map_err(IdentityError::from)
            .map_err(StatefulTransitionCoordinatorError::Identity)
            .map_err(preflight)?;
        let record_binding = coordinator
            .identity
            .journal
            .record_binding(cast, &coordinator.record)
            .map_err(StatefulTransitionCoordinatorError::from)
            .map_err(preflight)?;

        let authority = authority.publish_root_abi().map_err(|source| {
            RootAbiPublicationFailure::Publication {
                transition_id: transition_id.clone(),
                source,
            }
        })?;

        require_published_root_abi_sandwich(
            &coordinator,
            &metadata,
            &provenance,
            &readiness,
            &authority,
            &seal,
        )
        .map_err(|source| RootAbiPublicationFailure::PostEffectEvidence {
            transition_id: transition_id.clone(),
            source,
        })?;

        // The consuming store primitive authenticates the retained predecessor
        // inode under the complete operation lock, publishes its sole legal
        // successor, and returns the exact successor inode. An error is
        // deliberately uncertain and cannot recover either consumed binding.
        let cast = authority
            .installation()
            .retained_mutable_cast_directory()
            .map_err(IdentityError::from)
            .map_err(StatefulTransitionCoordinatorError::Identity)
            .map_err(|source| RootAbiPublicationFailure::CompletionPersistence {
                transition_id: transition_id.clone(),
                source,
            })?;
        let record_binding = coordinator
            .identity
            .journal
            .advance_record_binding(cast, record_binding, &complete)
            .map_err(StatefulTransitionCoordinatorError::from)
            .map_err(|source| RootAbiPublicationFailure::CompletionPersistence {
                transition_id,
                source,
            })?;
        coordinator.record = complete;

        // A successful bound update is not by itself sufficient to return a
        // reusable typestate. Reauthenticate the exact returned successor
        // inode on both sides of the complete Published/RetainedRootAbi
        // evidence sandwich while RootLinksComplete is canonical.
        require_root_links_complete_sandwich(
            &coordinator,
            &metadata,
            &provenance,
            &readiness,
            &authority,
            &record_binding,
            &seal,
        )
        .map_err(|source| RootAbiPublicationFailure::FinalEvidence {
            transition_id: coordinator.record.transition_id.clone(),
            source,
        })?;

        Ok(RootLinksCompleteCoordinator {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
        })
    }
}

fn exact_root_links_successor(
    record: &TransitionRecord,
) -> Result<TransitionRecord, RootAbiPublicationFailure> {
    let transition_id = record.transition_id.clone();
    let operation = record.operation;
    let expected_generation = match operation {
        Operation::NewState => 10,
        Operation::ActiveReblit => 8,
        Operation::ActivateArchived => 6,
    };
    let complete = record
        .forward_successor(None)
        .map_err(StatefulTransitionCoordinatorError::from)
        .map_err(|source| RootAbiPublicationFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        })?;
    if complete.phase != Phase::RootLinksComplete || complete.generation != expected_generation {
        return Err(RootAbiPublicationFailure::SuccessorContract {
            transition_id,
            operation,
            expected_phase: Phase::RootLinksComplete,
            expected_generation,
            actual_phase: complete.phase,
            actual_generation: complete.generation,
        });
    }
    Ok(complete)
}

fn require_published_root_abi_sandwich(
    coordinator: &StatefulTransitionCoordinator,
    metadata: &CandidateMetadataProof,
    provenance: &db::state::MetadataProvenance,
    readiness: &UsrExchangeReadiness,
    authority: &PublishedJournalRootAbiAuthority,
    seal: &UsrExchangeEffectSeal,
) -> Result<(), StatefulTransitionCoordinatorError> {
    coordinator.seal_prepared_candidate()?;
    require_published_root_abi_evidence(coordinator, metadata, provenance, readiness, authority, seal)?;
    require_published_root_abi_evidence(coordinator, metadata, provenance, readiness, authority, seal)
}

fn require_root_links_complete_sandwich(
    coordinator: &StatefulTransitionCoordinator,
    metadata: &CandidateMetadataProof,
    provenance: &db::state::MetadataProvenance,
    readiness: &UsrExchangeReadiness,
    authority: &PublishedJournalRootAbiAuthority,
    record_binding: &TransitionJournalRecordBinding,
    seal: &UsrExchangeEffectSeal,
) -> Result<(), StatefulTransitionCoordinatorError> {
    require_exact_record_binding(coordinator, authority, record_binding)?;
    require_published_root_abi_sandwich(coordinator, metadata, provenance, readiness, authority, seal)?;
    require_exact_record_binding(coordinator, authority, record_binding)
}

fn require_exact_record_binding(
    coordinator: &StatefulTransitionCoordinator,
    authority: &PublishedJournalRootAbiAuthority,
    record_binding: &TransitionJournalRecordBinding,
) -> Result<(), StatefulTransitionCoordinatorError> {
    let cast = authority
        .installation()
        .retained_mutable_cast_directory()
        .map_err(IdentityError::from)
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    if coordinator
        .identity
        .journal
        .has_record_binding(cast, record_binding, &coordinator.record)?
    {
        Ok(())
    } else {
        Err(StatefulTransitionCoordinatorError::CanonicalRecordBindingChanged {
            transition_id: coordinator.record.transition_id.clone(),
            expected_phase: coordinator.record.phase,
        })
    }
}

fn require_published_root_abi_evidence(
    coordinator: &StatefulTransitionCoordinator,
    metadata: &CandidateMetadataProof,
    provenance: &db::state::MetadataProvenance,
    readiness: &UsrExchangeReadiness,
    authority: &PublishedJournalRootAbiAuthority,
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
    require_active_reblit_snapshot(
        coordinator,
        authority.active_reblit(),
        installation,
        seal,
        true,
    )?;
    readiness.require_live(&coordinator.identity)?;
    coordinator.require_record_runtime_evidence()?;
    coordinator.require_canonical_record()?;

    // Make descriptor-pinned public root-ABI evidence, not a reopened root
    // pathname or the consumed preflight, the final observation in each pass.
    authority.require_post_exchange()?;
    Ok(())
}

impl RootLinksCompleteCoordinator {
    #[cfg(test)]
    pub(crate) fn record(&self) -> &TransitionRecord {
        &self.coordinator.record
    }

    #[cfg(test)]
    pub(crate) fn revalidate_retained_authorities(&self) -> Result<(), StatefulTransitionCoordinatorError> {
        let seal = UsrExchangeEffectSeal { _private: () };
        require_root_links_complete_sandwich(
            &self.coordinator,
            &self.metadata,
            &self.provenance,
            &self.readiness,
            &self.authority,
            &self.record_binding,
            &seal,
        )
    }
}
