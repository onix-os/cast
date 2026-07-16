//! Active-selection proof retained beneath the startup writer reservation.

use crate::{Installation, state};

use super::{ActiveStateReservation, ActiveStateSnapshot, capture, revalidate_proof};

impl ActiveStateReservation {
    /// Capture exact live-selection evidence while the startup reservation
    /// still excludes every cooperating active-state writer.
    ///
    /// This does not consume the reservation: startup may use the returned
    /// snapshot to authorize one narrowly-scoped recovery mutation and then
    /// consume the same reservation for ordinary post-gate discovery.
    pub(in crate::client) fn capture_for_startup_recovery(
        &self,
        installation: &Installation,
    ) -> Result<ActiveStateSnapshot, super::super::Error> {
        let _retained_coordinator = &self.coordinator;
        let captured = capture(installation)?;
        let actual = captured.active;
        let expected = installation.active_state;
        if actual != expected {
            return Err(super::super::Error::ActiveStateSnapshotChanged { expected, actual });
        }
        Ok(ActiveStateSnapshot {
            active: actual,
            proof: captured.proof,
        })
    }
}

impl ActiveStateSnapshot {
    pub(in crate::client) fn active(&self) -> Option<state::Id> {
        self.active
    }

    pub(in crate::client) fn revalidate(&self, installation: &Installation) -> Result<(), super::super::Error> {
        revalidate_proof(self.active, &self.proof, installation)
    }
}
