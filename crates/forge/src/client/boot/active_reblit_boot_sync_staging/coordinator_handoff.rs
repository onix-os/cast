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
        Client, CoordinatorActiveStateReservation, active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
    },
    db::state::Database,
    transition_identity::{
        ActiveReblitBootSyncHandoffFailure, ActiveReblitBootSyncHandoffSeal, NewStateUnarchivedBootSyncHandoffSeal,
        PreviousArchivedBootSyncHandoffSeal, SystemTriggersCompleteCoordinator,
    },
    transition_journal::{Operation, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::{ActiveReblitBootSyncStagingError, StagedActiveReblitBootSync, stage_with_retained_stores_and_reservation};

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

/// The booted state is always the record's candidate. How it relates to the
/// predecessor depends on the operation: ActiveReblit repairs a state in place
/// (`candidate == previous`), whereas NewState boots a fresh candidate. NewState
/// has two legitimate shapes — replacing an active state, whose predecessor was
/// archived to a distinct slot, and a first install, which has no predecessor at
/// all and therefore archives nothing.
fn boot_previous_matches_operation(record: &TransitionRecord, boot_state: Option<i32>) -> bool {
    match record.operation {
        Operation::ActiveReblit => record.previous.id == boot_state,
        Operation::NewState => {
            if record.options.archive_previous {
                record.previous.id.is_some() && record.previous.id != record.candidate.id
            } else {
                // First install: nothing preceded the candidate.
                record.previous.id.is_none()
            }
        }
        Operation::ActivateArchived => record.previous.id.is_some() && record.previous.id != record.candidate.id,
    }
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

    /// NewState boot handoff: the boot candidate is the freshly created state,
    /// distinct from the archived predecessor. The operation-aware state gate
    /// admits this shape.
    #[allow(clippy::too_many_arguments)]
    /// Hand off for a first install: a NewState candidate with no predecessor,
    /// so boot publication is entered directly from `SystemTriggersComplete`
    /// without an archive phase (`plans/future_impl.md` §1.1a).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_unarchived_system_triggers_complete(
        _seal: NewStateUnarchivedBootSyncHandoffSeal,
        record: TransitionRecord,
        record_binding: TransitionJournalRecordBinding,
        journal: TransitionJournalStore,
        database: Database,
        installation: Installation,
        boot_candidate: State,
        active_state_reservation: CoordinatorActiveStateReservation,
    ) -> Self {
        Self {
            record,
            record_binding,
            journal,
            database,
            installation,
            active_reblit: boot_candidate,
            active_state_reservation,
        }
    }

    pub(crate) fn from_previous_archived(
        _seal: PreviousArchivedBootSyncHandoffSeal,
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

    fn require_client(&self, client: &Client) -> Result<(), ActiveReblitCoordinatorBootSyncStagingError> {
        if !self.database.same_instance(&client.state_db)
            || !std::ptr::eq(self.installation.root_directory(), client.installation.root_directory())
        {
            return Err(ActiveReblitCoordinatorBootSyncStagingError::ClientCapabilityMismatch);
        }
        let boot_state = Some(i32::from(self.active_reblit.id));
        if boot_state != self.record.candidate.id || !boot_previous_matches_operation(&self.record, boot_state) {
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
            || !boot_previous_matches_operation(&self.record, plan_state_record_id)
        {
            return Err(ActiveReblitCoordinatorBootSyncStagingError::PlanActiveStateMismatch);
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
            || !std::ptr::eq(self.installation.root_directory(), installation.root_directory())
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
            BoundActiveReblitBlsPublicationPlan<'input, 'topology_view, 'topology_authority, 'attempt, 'stone, 'roots>,
        >,
        ActiveReblitCoordinatorBootSyncStagingError,
    > {
        let handoff = coordinator.into_active_reblit_boot_sync_handoff()?;
        stage_active_reblit_boot_sync_from_handoff(self, plan, inventory, handoff)
    }

    /// Stage an already-produced NewState boot handoff into receipt-bearing
    /// `BootSyncStarted`. The predecessor is already archived; the handoff was
    /// minted by `PreviousArchivedCoordinator::into_new_state_boot_sync_handoff`.
    /// Staging itself is operation-neutral — it consumes the handoff's retained
    /// stores and reservation exactly as for ActiveReblit.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::client) fn stage_new_state_boot_sync_from_handoff<
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
        handoff: CoordinatorActiveReblitBootSyncHandoff,
    ) -> Result<
        StagedActiveReblitBootSync<
            'plan,
            'inventory,
            BoundActiveReblitBlsPublicationPlan<'input, 'topology_view, 'topology_authority, 'attempt, 'stone, 'roots>,
        >,
        ActiveReblitCoordinatorBootSyncStagingError,
    > {
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
        BoundActiveReblitBlsPublicationPlan<'input, 'topology_view, 'topology_authority, 'attempt, 'stone, 'roots>,
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
        BoundActiveReblitBlsPublicationPlan<'input, 'topology_view, 'topology_authority, 'attempt, 'stone, 'roots>,
    >,
    ActiveReblitCoordinatorBootSyncStagingError,
> {
    stage_active_reblit_boot_sync_from_handoff(client, plan, inventory, handoff)
}
