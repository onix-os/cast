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

use super::super::{StatefulTransitionCoordinator, StatefulTransitionCoordinatorError};
use super::{SystemTriggersCompleteCoordinator, require_system_trigger_same_store_evidence};

const HAND_OFF_ACTIVE_REBLIT_BOOT_SYNC: &str = "hand off active reblit boot synchronization";

/// Unforgeable origin proof for the sole production handoff constructor.
pub(crate) struct ActiveReblitBootSyncHandoffSeal {
    _private: (),
}

#[derive(Debug, Error)]
pub(crate) enum ActiveReblitBootSyncHandoffFailure {
    #[error("transition {transition_id} is not exact ActiveReblit SystemTriggersComplete generation 10 boot authority")]
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
            || active_reblit.as_ref().map(|state| i32::from(state.id)) != coordinator.record.candidate.id
        {
            return Err(ActiveReblitBootSyncHandoffFailure::SourceContract { transition_id });
        }
        let active_reblit = active_reblit.expect("exact ActiveReblit source retained its state");

        let preflight = |source| ActiveReblitBootSyncHandoffFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };
        coordinator
            .require_phase(Phase::SystemTriggersComplete, HAND_OFF_ACTIVE_REBLIT_BOOT_SYNC)
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

/// Unforgeable proof that a first-install NewState transition reached the exact
/// durable `SystemTriggersComplete` record with nothing to archive, and may hand
/// its retained stores into boot publication.
pub(crate) struct NewStateUnarchivedBootSyncHandoffSeal {
    _private: (),
}

impl SystemTriggersCompleteCoordinator {
    /// Enter boot publication for a NewState candidate that has **no
    /// predecessor**, straight from `SystemTriggersComplete`.
    ///
    /// A first install archives nothing, so the `PreviousArchiveIntent` and
    /// `PreviousArchived` phases never occur and boot is entered one step
    /// earlier than the replace-an-active-state route
    /// (`plans/future_impl.md` §1.1a).
    // Forward scaffolding: consumed by the coordinated first-install route.
    #[allow(dead_code)] // consumed by the coordinated NewState route (first install)
    pub(crate) fn into_new_state_unarchived_boot_sync_handoff(
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

        if !exact_new_state_unarchived_boot_source(&coordinator.record) {
            return Err(ActiveReblitBootSyncHandoffFailure::SourceContract { transition_id });
        }
        let preflight = |source| ActiveReblitBootSyncHandoffFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };
        coordinator
            .require_phase(Phase::SystemTriggersComplete, HAND_OFF_NEW_STATE_UNARCHIVED_BOOT)
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

        let candidate_id = coordinator
            .record
            .candidate
            .id
            .map(crate::state::Id::from)
            .ok_or_else(|| ActiveReblitBootSyncHandoffFailure::SourceContract {
                transition_id: transition_id.clone(),
            })?;
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
        let boot_candidate = state_database.get(candidate_id).map_err(|_| {
            ActiveReblitBootSyncHandoffFailure::SourceContract {
                transition_id: transition_id.clone(),
            }
        })?;
        Ok(
            CoordinatorActiveReblitBootSyncHandoff::from_unarchived_system_triggers_complete(
                NewStateUnarchivedBootSyncHandoffSeal { _private: () },
                record,
                record_binding,
                journal,
                state_database,
                installation,
                boot_candidate,
                active_state_reservation,
            ),
        )
    }
}

const HAND_OFF_NEW_STATE_UNARCHIVED_BOOT: &str = "hand off unarchived new state boot sync";

/// A first-install NewState transition ready to publish boot entries: nothing
/// preceded it, so nothing was archived.
fn exact_new_state_unarchived_boot_source(record: &TransitionRecord) -> bool {
    record.operation == Operation::NewState
        && record.phase == Phase::SystemTriggersComplete
        && record.rollback.is_none()
        && record.options.run_system_triggers
        && !record.options.archive_previous
        && record.options.run_boot_sync
        && record.boot_publication_receipts.is_none()
        && record.candidate.id.is_some()
        && record.previous.id.is_none()
}
