//! Durable predecessor-archive advance for `archive_previous` transitions.
//!
//! NewState (and any future operation that displaces a real predecessor) must
//! move the exact staged previous tree into its authenticated per-state slot
//! between `SystemTriggersComplete` and boot publication. This boundary drives
//! the two intervening journal phases with the same bound-record discipline the
//! system-trigger runner uses: the `PreviousArchiveIntent` record is persisted
//! *before* the physical move, and `PreviousArchived` *after* it, so a crash at
//! any point leaves a durable record startup reconciliation can resume or
//! reverse. ActiveReblit repairs its state in place and never reaches here.

use thiserror::Error;

use super::{
    CandidateMetadataProof, Phase, StatefulTransitionCoordinator,
    StatefulTransitionCoordinatorError, SystemTriggersCompleteCoordinator, TransitionRecord,
    UsrExchangeReadiness, advance_bound_system_trigger_record, db,
    require_same_store_record_binding, require_system_trigger_same_store_evidence, state,
};
use crate::state::TransitionId;
use crate::transition_journal::TransitionJournalRecordBinding;

type BoxedAdvanceError = Box<dyn std::error::Error + Send + Sync + 'static>;

const ARCHIVE_PREVIOUS: &str = "archive the previous state tree";

/// Unforgeable proof that a journal-coordinated caller owns the exact durable
/// `PreviousArchiveIntent` record and may physically archive its predecessor
/// while its transition journal is retained. Every legacy archive entry point
/// continues to require journal absence.
pub(crate) struct PreviousArchiveEffectSeal {
    _private: (),
}

/// Sole in-process owner after the predecessor tree is durably archived, ready
/// to hand off into boot publication.
#[derive(Debug)]
pub(crate) struct PreviousArchivedCoordinator {
    coordinator: StatefulTransitionCoordinator,
    metadata: CandidateMetadataProof,
    provenance: db::state::MetadataProvenance,
    authority: crate::client::PublishedJournalRootAbiAuthority,
    readiness: UsrExchangeReadiness,
    record_binding: TransitionJournalRecordBinding,
}

#[derive(Debug, Error)]
pub(crate) enum PreviousArchiveFailure {
    #[error(
        "transition {transition_id} is not an archive-previous SystemTriggersComplete authority"
    )]
    SourceContract { transition_id: TransitionId },
    #[error("transition {transition_id} failed predecessor-archive preflight")]
    Preflight {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error("transition {transition_id} produced an unexpected {stage} archive successor")]
    SuccessorContract {
        transition_id: TransitionId,
        stage: &'static str,
    },
    #[error("transition {transition_id} failed to persist the {stage} archive record")]
    Advance {
        transition_id: TransitionId,
        stage: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("transition {transition_id} failed to physically archive its predecessor tree")]
    PhysicalArchive {
        transition_id: TransitionId,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

impl SystemTriggersCompleteCoordinator {
    /// Durably archive the displaced predecessor tree, advancing the journal
    /// through `PreviousArchiveIntent → PreviousArchived`.
    pub(crate) fn archive_previous_tree(
        self,
    ) -> Result<PreviousArchivedCoordinator, PreviousArchiveFailure> {
        let Self {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
        } = self;
        let transition_id = coordinator.record.transition_id.clone();

        if !exact_archive_previous_source(&coordinator.record) {
            return Err(PreviousArchiveFailure::SourceContract { transition_id });
        }
        let preflight = |source| PreviousArchiveFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };
        coordinator
            .require_phase(Phase::SystemTriggersComplete, ARCHIVE_PREVIOUS)
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

        // Intent is durable before the physical move: a crash after this record
        // leaves startup reconciliation to complete or reverse the archive.
        let intent = archive_successor(&coordinator.record, Phase::PreviousArchiveIntent, "intent")
            .map_err(|stage| PreviousArchiveFailure::SuccessorContract {
                transition_id: transition_id.clone(),
                stage,
            })?;
        let (coordinator, record_binding) = advance_bound_system_trigger_record(
            coordinator,
            &metadata,
            &provenance,
            &authority,
            &readiness,
            record_binding,
            intent,
        )
        .map_err(|source| PreviousArchiveFailure::Advance {
            transition_id: transition_id.clone(),
            stage: "intent",
            source: Box::new(source),
        })?;

        // Physical move of the exact staged predecessor into its state slot.
        let previous_id = coordinator
            .record
            .previous
            .id
            .map(state::Id::from)
            .ok_or_else(|| PreviousArchiveFailure::SourceContract {
                transition_id: transition_id.clone(),
            })?;
        let seal = PreviousArchiveEffectSeal { _private: () };
        coordinator
            .identity
            .archive_previous_with_journal(authority.installation(), previous_id, &seal)
            .map_err(|source| PreviousArchiveFailure::PhysicalArchive {
                transition_id: transition_id.clone(),
                source: Box::new(source),
            })?;

        // Completion is durable only after the move succeeded.
        let archived =
            archive_successor(&coordinator.record, Phase::PreviousArchived, "archived").map_err(
                |stage| PreviousArchiveFailure::SuccessorContract {
                    transition_id: transition_id.clone(),
                    stage,
                },
            )?;
        // The physical move deliberately relocated the previous tree out of
        // staging, so the tree/root-ABI same-store sandwich no longer holds by
        // design. Completion re-validates only the journal record binding (cast
        // and canonical inode), which the move did not touch.
        let (coordinator, record_binding) =
            advance_archive_completion_record(coordinator, &authority, record_binding, archived)
                .map_err(|source| PreviousArchiveFailure::Advance {
                    transition_id,
                    stage: "archived",
                    source,
                })?;

        Ok(PreviousArchivedCoordinator {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
        })
    }
}

impl PreviousArchivedCoordinator {
    #[cfg(test)]
    pub(crate) fn record(&self) -> &TransitionRecord {
        &self.coordinator.record
    }
}

fn exact_archive_previous_source(record: &TransitionRecord) -> bool {
    record.phase == Phase::SystemTriggersComplete
        && record.options.archive_previous
        && record.previous.id.is_some()
}

fn archive_successor(
    record: &TransitionRecord,
    expected_phase: Phase,
    stage: &'static str,
) -> Result<TransitionRecord, &'static str> {
    let successor = record.forward_successor(None).map_err(|_| stage)?;
    if successor.phase != expected_phase {
        return Err(stage);
    }
    Ok(successor)
}

/// Durably advance the record binding without re-running the tree/root-ABI
/// same-store sandwich. Used only for the post-move `PreviousArchived` advance,
/// where the predecessor tree has legitimately left staging; the journal cast
/// and canonical record inode — the only things this validates — are untouched.
fn advance_archive_completion_record(
    mut coordinator: StatefulTransitionCoordinator,
    authority: &crate::client::PublishedJournalRootAbiAuthority,
    predecessor_binding: TransitionJournalRecordBinding,
    successor: TransitionRecord,
) -> Result<(StatefulTransitionCoordinator, TransitionJournalRecordBinding), BoxedAdvanceError> {
    let cast = authority
        .installation()
        .retained_mutable_cast_directory()
        .map_err(crate::transition_identity::Error::from)?;
    let successor_binding = coordinator
        .identity
        .journal
        .advance_record_binding(&cast, predecessor_binding, &successor)?;
    coordinator.record = successor;
    require_same_store_record_binding(&coordinator, authority, &successor_binding)?;
    Ok((coordinator, successor_binding))
}
