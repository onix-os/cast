//! Exact ActiveReblit boot-staging handoff from system-trigger completion.
//!
//! This boundary advances no journal phase and performs no boot, database,
//! cleanup, namespace, trigger, retry, or live-dispatch action. It only moves
//! the exact phase-10 record, its non-cloneable binding and retained stores,
//! and the continuously held cooperating-writer reservation into the boot
//! staging owner.

use thiserror::Error;

use crate::{
    client::CoordinatorActiveReblitBootSyncHandoff,
    state::TransitionId,
    transition_identity::StatefulTreeIdentity,
    transition_journal::{Operation, Phase, TransitionRecord},
};

use super::{SystemTriggersCompleteCoordinator, require_system_trigger_same_store_evidence};
use super::super::{StatefulTransitionCoordinator, StatefulTransitionCoordinatorError};

const HAND_OFF_ACTIVE_REBLIT_BOOT_SYNC: &str = "hand off active reblit boot synchronization";

/// Unforgeable origin proof for the sole production handoff constructor.
pub(crate) struct ActiveReblitBootSyncHandoffSeal {
    _private: (),
}

#[derive(Debug, Error)]
pub(crate) enum ActiveReblitBootSyncHandoffFailure {
    #[error(
        "transition {transition_id} is not exact ActiveReblit SystemTriggersComplete generation 10 boot authority"
    )]
    SourceContract { transition_id: TransitionId },
    #[error("transition {transition_id} failed boot-staging handoff preflight")]
    Preflight {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
}

impl SystemTriggersCompleteCoordinator {
    pub(crate) fn into_active_reblit_boot_sync_handoff(
        self,
    ) -> Result<CoordinatorActiveReblitBootSyncHandoff, ActiveReblitBootSyncHandoffFailure> {
        let Self {
            coordinator,
            metadata,
            provenance,
            authority,
            readiness,
            record_binding,
        } = self;
        let transition_id = coordinator.record.transition_id.clone();
        let active_reblit = authority.active_reblit().cloned();
        if !exact_active_reblit_boot_source(&coordinator.record)
            || active_reblit
                .as_ref()
                .map(|state| i32::from(state.id))
                != coordinator.record.candidate.id
        {
            return Err(ActiveReblitBootSyncHandoffFailure::SourceContract {
                transition_id,
            });
        }
        let active_reblit = active_reblit.expect("exact ActiveReblit source retained its state");

        let preflight = |source| ActiveReblitBootSyncHandoffFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };
        coordinator
            .require_phase(
                Phase::SystemTriggersComplete,
                HAND_OFF_ACTIVE_REBLIT_BOOT_SYNC,
            )
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

        let installation = authority.installation().clone();
        let active_state_reservation = authority.into_active_state_reservation();
        drop(metadata);
        let _ = provenance;
        drop(readiness);
        let StatefulTransitionCoordinator { identity, record } = coordinator;
        let StatefulTreeIdentity {
            journal,
            state_database,
            ..
        } = identity;
        Ok(CoordinatorActiveReblitBootSyncHandoff::from_system_triggers_complete(
            ActiveReblitBootSyncHandoffSeal { _private: () },
            record,
            record_binding,
            journal,
            state_database,
            installation,
            active_reblit,
            active_state_reservation,
        ))
    }
}

fn exact_active_reblit_boot_source(record: &TransitionRecord) -> bool {
    record.operation == Operation::ActiveReblit
        && record.phase == Phase::SystemTriggersComplete
        && record.generation == 10
        && record.rollback.is_none()
        && record.options.run_system_triggers
        && !record.options.archive_previous
        && record.options.run_boot_sync
        && record.boot_publication_receipts.is_none()
        && record.candidate.id.is_some()
        && record.candidate.id == record.previous.id
}
