//! Coordinator-only entry into exact ActiveReblit boot staging.
//!
//! The handoff is constructible only from the system-trigger coordinator's
//! exact phase-10 source. It retains that coordinator's journal, record
//! binding, database, installation, state snapshot, and already-held writer
//! reservation until staging consumes all of them together.

use thiserror::Error;

use crate::{
    Installation, State,
    client::{
        Client, CoordinatorActiveStateReservation,
        active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
    },
    db::state::Database,
    transition_identity::{
        ActiveReblitBootSyncHandoffFailure, ActiveReblitBootSyncHandoffSeal,
        SystemTriggersCompleteCoordinator,
    },
    transition_journal::{TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    ActiveReblitBootSyncStagingError, StagedActiveReblitBootSync,
    stage_with_retained_stores_and_reservation,
};

/// Continuously locked transfer from exact system-trigger completion.
///
/// Declaration order releases the record binding before its journal, both
/// stores before the retained installation, and every capability before the
/// writer reservation.
pub(crate) struct CoordinatorActiveReblitBootSyncHandoff {
    record: TransitionRecord,
    record_binding: TransitionJournalRecordBinding,
    journal: TransitionJournalStore,
    database: Database,
    installation: Installation,
    active_reblit: State,
    active_state_reservation: CoordinatorActiveStateReservation,
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitCoordinatorBootSyncStagingError {
    #[error("admit exact ActiveReblit system-trigger completion for boot staging")]
    Handoff(#[from] ActiveReblitBootSyncHandoffFailure),
    #[error("the coordinator boot handoff belongs to a different client capability set")]
    ClientCapabilityMismatch,
    #[error("the coordinator boot handoff retained a different ActiveReblit state")]
    ActiveStateMismatch,
    #[error("the bound boot plan targets a different global state than the coordinator handoff")]
    PlanActiveStateMismatch,
    #[error("stage exact ActiveReblit boot synchronization")]
    Staging(#[from] ActiveReblitBootSyncStagingError),
}

impl CoordinatorActiveReblitBootSyncHandoff {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_system_triggers_complete(
        _seal: ActiveReblitBootSyncHandoffSeal,
        record: TransitionRecord,
        record_binding: TransitionJournalRecordBinding,
        journal: TransitionJournalStore,
        database: Database,
        installation: Installation,
        active_reblit: State,
        active_state_reservation: CoordinatorActiveStateReservation,
    ) -> Self {
        Self {
            record,
            record_binding,
            journal,
            database,
            installation,
            active_reblit,
            active_state_reservation,
        }
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_parts_for_test(
        record: TransitionRecord,
        record_binding: TransitionJournalRecordBinding,
        journal: TransitionJournalStore,
        database: Database,
        installation: Installation,
        active_reblit: State,
        active_state_reservation: CoordinatorActiveStateReservation,
    ) -> Self {
        Self {
            record,
            record_binding,
            journal,
            database,
            installation,
            active_reblit,
            active_state_reservation,
        }
    }

    fn require_client(
        &self,
        client: &Client,
    ) -> Result<(), ActiveReblitCoordinatorBootSyncStagingError> {
        if !self.database.same_instance(&client.state_db)
            || !std::ptr::eq(
                self.installation.root_directory(),
                client.installation.root_directory(),
            )
        {
            return Err(
                ActiveReblitCoordinatorBootSyncStagingError::ClientCapabilityMismatch,
            );
        }
        let active_state = Some(i32::from(self.active_reblit.id));
        if active_state != self.record.candidate.id
            || active_state != self.record.previous.id
        {
            return Err(ActiveReblitCoordinatorBootSyncStagingError::ActiveStateMismatch);
        }
        Ok(())
    }

    fn require_plan_state(
        &self,
        plan_state: crate::state::Id,
    ) -> Result<(), ActiveReblitCoordinatorBootSyncStagingError> {
        let plan_state_record_id = Some(i32::from(plan_state));
        if plan_state != self.active_reblit.id
            || self.record.candidate.id != plan_state_record_id
            || self.record.previous.id != plan_state_record_id
        {
            return Err(
                ActiveReblitCoordinatorBootSyncStagingError::PlanActiveStateMismatch,
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn record(&self) -> &TransitionRecord {
        &self.record
    }

    #[cfg(test)]
    pub(crate) fn retains_exact_source_for_test(
        &self,
        installation: &Installation,
        database: &Database,
        state: crate::state::Id,
    ) -> bool {
        if !self.database.same_instance(database)
            || !std::ptr::eq(
                self.installation.root_directory(),
                installation.root_directory(),
            )
            || self.active_reblit.id != state
        {
            return false;
        }
        let Ok(cast) = self.installation.retained_mutable_cast_directory() else {
            return false;
        };
        self.journal
            .has_record_binding(cast, &self.record_binding, &self.record)
            .unwrap_or(false)
    }

    #[cfg(test)]
    pub(crate) fn assert_writer_reservation_held_until_drop_for_test(self) {
        use std::{
            sync::mpsc::{self, RecvTimeoutError},
            thread,
            time::Duration,
        };

        let (reached_sender, reached_receiver) = mpsc::channel();
        let (acquired_sender, acquired_receiver) = mpsc::channel();
        let contender = thread::spawn(move || {
            crate::client::fixed_staging::arm_before_coordinator_lock(move || {
                reached_sender.send(()).unwrap();
            });
            let reservation = CoordinatorActiveStateReservation::acquire().unwrap();
            acquired_sender.send(()).unwrap();
            drop(reservation);
        });
        reached_receiver.recv_timeout(Duration::from_secs(120)).unwrap();
        assert!(matches!(
            acquired_receiver.recv_timeout(Duration::from_millis(100)),
            Err(RecvTimeoutError::Timeout)
        ));

        drop(self);
        acquired_receiver.recv_timeout(Duration::from_secs(120)).unwrap();
        contender.join().unwrap();
    }
}

impl Client {
    /// Consume the exact coordinator source directly into receipt-bearing
    /// `BootSyncStarted` staging without reopening or rebinding its journal.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::client) fn stage_active_reblit_boot_sync_from_coordinator<
        'plan,
        'inventory,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >(
        &self,
        plan: &'plan BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
        coordinator: SystemTriggersCompleteCoordinator,
    ) -> Result<
        StagedActiveReblitBootSync<
            'plan,
            'inventory,
            BoundActiveReblitBlsPublicationPlan<
                'input,
                'topology_view,
                'topology_authority,
                'attempt,
                'stone,
                'roots,
            >,
        >,
        ActiveReblitCoordinatorBootSyncStagingError,
    > {
        let handoff = coordinator.into_active_reblit_boot_sync_handoff()?;
        stage_active_reblit_boot_sync_from_handoff(self, plan, inventory, handoff)
    }
}

#[allow(clippy::too_many_arguments)]
fn stage_active_reblit_boot_sync_from_handoff<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    client: &Client,
    plan: &'plan BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
    handoff: CoordinatorActiveReblitBootSyncHandoff,
) -> Result<
    StagedActiveReblitBootSync<
        'plan,
        'inventory,
        BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
    >,
    ActiveReblitCoordinatorBootSyncStagingError,
> {
    handoff.require_client(client)?;
    handoff.require_plan_state(plan.global_state())?;
    let CoordinatorActiveReblitBootSyncHandoff {
        record,
        record_binding,
        journal,
        database,
        installation,
        active_reblit: _,
        active_state_reservation,
    } = handoff;
    stage_with_retained_stores_and_reservation(
        active_state_reservation,
        &client.installation,
        installation,
        database,
        plan,
        inventory,
        journal,
        record,
        record_binding,
    )
    .map_err(ActiveReblitCoordinatorBootSyncStagingError::Staging)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(in crate::client) fn stage_active_reblit_boot_sync_from_handoff_for_test<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    client: &Client,
    plan: &'plan BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
    handoff: CoordinatorActiveReblitBootSyncHandoff,
) -> Result<
    StagedActiveReblitBootSync<
        'plan,
        'inventory,
        BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
    >,
    ActiveReblitCoordinatorBootSyncStagingError,
> {
    stage_active_reblit_boot_sync_from_handoff(client, plan, inventory, handoff)
}
