//! Exact ActiveReblit no-boot handoff from system-trigger completion.
//!
//! This journal-only boundary performs no boot, cleanup, database, namespace,
//! trigger, retry, or live-dispatch action.

use thiserror::Error;

use crate::{
    client::{
        ActiveReblitNoBootTailError, CoordinatorActiveStateReservation,
        FinalizedActiveReblitNoBoot, finish_active_reblit_no_boot,
    },
    db,
    Installation,
    state::TransitionId,
    transition_identity::StatefulTreeIdentity,
    transition_journal::{Operation, Phase, TransitionJournalStore, TransitionRecord},
};

use super::{
    BoundSystemTriggerAdvanceFailure, SystemTriggersCompleteCoordinator,
    advance_bound_system_trigger_record, require_system_trigger_same_store_evidence,
};
use super::super::{StatefulTransitionCoordinator, StatefulTransitionCoordinatorError};

const COMMIT_ACTIVE_REBLIT_WITHOUT_BOOT: &str = "commit active reblit without boot synchronization";

/// Unforgeable token accepted only by the client-owned no-boot terminal tail.
pub(crate) struct ActiveReblitNoBootTailSeal {
    _private: (),
}

/// Continuously locked handoff after exact no-boot commit decision.
/// Journal-first field order releases the journal before the writer lease.
pub(in crate::transition_identity::journal_coordinator) struct ActiveReblitNoBootCommitDecisionHandoff {
    journal: TransitionJournalStore,
    state_database: db::state::Database,
    installation: Installation,
    record: TransitionRecord,
    _active_state_reservation: CoordinatorActiveStateReservation,
}

#[derive(Debug, Error)]
pub(in crate::transition_identity::journal_coordinator) enum ActiveReblitNoBootCommitDecisionFailure {
    #[error("transition {transition_id} is not exact ActiveReblit SystemTriggersComplete generation 10 no-boot authority")]
    SourceContract { transition_id: TransitionId },
    #[error("transition {transition_id} failed no-boot commit-decision preflight")]
    Preflight {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error("transition {transition_id} derived {actual_phase:?} generation {actual_generation} instead of CommitDecided generation 11")]
    SuccessorContract {
        transition_id: TransitionId,
        actual_phase: Phase,
        actual_generation: u64,
    },
    #[error("transition {transition_id} could not durably publish no-boot commit decision; SystemTriggersComplete or CommitDecided is exact after fresh reopen when classifiable")]
    Persistence {
        transition_id: TransitionId,
        #[source]
        source: BoundSystemTriggerAdvanceFailure,
    },
    #[error("transition {transition_id} failed final no-boot CommitDecided retained evidence")]
    FinalEvidence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
}

impl SystemTriggersCompleteCoordinator {
    /// Consume exact no-boot system-trigger completion through the existing
    /// cleanup, terminal finalization, and clean-admission suffix.
    pub(crate) fn complete_active_reblit_without_boot(
        self,
    ) -> Result<FinalizedActiveReblitNoBoot, ActiveReblitNoBootCompletionFailure> {
        let handoff = self
            .commit_active_reblit_without_boot()
            .map_err(ActiveReblitNoBootCompletionFailureKind::CommitDecision)?;
        let ActiveReblitNoBootCommitDecisionHandoff {
            journal,
            state_database,
            installation,
            record,
            _active_state_reservation: active_state_reservation,
        } = handoff;
        finish_active_reblit_no_boot(
            ActiveReblitNoBootTailSeal { _private: () },
            installation,
            state_database,
            journal,
            record,
            active_state_reservation,
        )
        .map_err(ActiveReblitNoBootCompletionFailureKind::TerminalTail)
        .map_err(Into::into)
    }

    pub(in crate::transition_identity::journal_coordinator) fn commit_active_reblit_without_boot(
        self,
    ) -> Result<ActiveReblitNoBootCommitDecisionHandoff, ActiveReblitNoBootCommitDecisionFailure> {
        let Self {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
        } = self;
        let transition_id = coordinator.record.transition_id.clone();
        if !exact_active_reblit_no_boot_source(&coordinator.record) {
            return Err(ActiveReblitNoBootCommitDecisionFailure::SourceContract {
                transition_id,
            });
        }

        let preflight = |source| ActiveReblitNoBootCommitDecisionFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };
        coordinator
            .require_phase(Phase::SystemTriggersComplete, COMMIT_ACTIVE_REBLIT_WITHOUT_BOOT)
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

        let successor = coordinator
            .record
            .forward_successor(None)
            .map_err(StatefulTransitionCoordinatorError::from)
            .map_err(preflight)?;
        if successor.phase != Phase::CommitDecided || successor.generation != 11 {
            return Err(ActiveReblitNoBootCommitDecisionFailure::SuccessorContract {
                transition_id,
                actual_phase: successor.phase,
                actual_generation: successor.generation,
            });
        }

        let (coordinator, record_binding) = advance_bound_system_trigger_record(
            coordinator,
            &metadata,
            &provenance,
            &authority,
            &readiness,
            record_binding,
            successor,
        )
        .map_err(|source| ActiveReblitNoBootCommitDecisionFailure::Persistence {
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
        .map_err(|source| ActiveReblitNoBootCommitDecisionFailure::FinalEvidence {
            transition_id,
            source,
        })?;

        let installation = authority.installation().clone();
        let active_state_reservation = authority.into_active_state_reservation();
        drop(record_binding);
        drop(metadata);
        let _ = provenance;
        drop(readiness);
        let StatefulTransitionCoordinator { identity, record } = coordinator;
        let StatefulTreeIdentity {
            journal,
            state_database,
            ..
        } = identity;
        Ok(ActiveReblitNoBootCommitDecisionHandoff {
            journal,
            state_database,
            installation,
            record,
            _active_state_reservation: active_state_reservation,
        })
    }
}

#[derive(Debug, Error)]
#[error(transparent)]
pub(crate) struct ActiveReblitNoBootCompletionFailure(
    #[from] ActiveReblitNoBootCompletionFailureKind,
);

#[derive(Debug, Error)]
enum ActiveReblitNoBootCompletionFailureKind {
    #[error("persist exact no-boot ActiveReblit commit decision")]
    CommitDecision(#[source] ActiveReblitNoBootCommitDecisionFailure),
    #[error("complete exact no-boot ActiveReblit terminal tail")]
    TerminalTail(#[source] ActiveReblitNoBootTailError),
}

fn exact_active_reblit_no_boot_source(record: &TransitionRecord) -> bool {
    record.operation == Operation::ActiveReblit
        && record.phase == Phase::SystemTriggersComplete
        && record.generation == 10
        && record.rollback.is_none()
        && record.options.run_system_triggers
        && !record.options.archive_previous
        && !record.options.run_boot_sync
        && record.boot_publication_receipts.is_none()
        && record.candidate.id.is_some()
        && record.candidate.id == record.previous.id
}

impl ActiveReblitNoBootCommitDecisionHandoff {
    #[cfg(test)]
    pub(in crate::transition_identity::journal_coordinator) fn record(&self) -> &TransitionRecord {
        &self.record
    }

    #[cfg(test)]
    pub(in crate::transition_identity::journal_coordinator) fn journal(&self) -> &TransitionJournalStore {
        &self.journal
    }
}
