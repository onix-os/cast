//! Active-selection proof retained beneath the startup writer reservation.

use crate::{Installation, state};

use super::{ActiveStateReservation, ActiveStateSnapshot, capture, revalidate_proof, revalidate_proof_unpinned};

impl ActiveStateReservation {
    /// Capture live-selection evidence for a forward boot completion, pinned to
    /// the candidate the transition published rather than to the selection the
    /// installation was discovered with.
    ///
    /// A NewState transition changes the active state by design — that is the
    /// exchange it just performed — so comparing against the discovery-time
    /// value refuses the very transition being completed. The caller must
    /// already require the live selection to be its candidate, which is the
    /// stronger assertion.
    pub(in crate::client) fn capture_for_forward_boot_completion(
        &self,
        installation: &Installation,
        expected: state::Id,
    ) -> Result<ActiveStateSnapshot, super::super::Error> {
        let _retained_coordinator = &self.coordinator;
        let captured = capture(installation)?;
        let actual = captured.active;
        if actual != Some(expected) {
            return Err(super::super::Error::ActiveStateSnapshotChanged {
                expected: Some(expected),
                actual,
            });
        }
        Ok(ActiveStateSnapshot {
            active: actual,
            proof: captured.proof,
        })
    }

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

    /// Revalidate for a forward boot completion, which legitimately changed the
    /// selection this snapshot proves.
    pub(in crate::client) fn revalidate_for_forward_boot_completion(
        &self,
        installation: &Installation,
    ) -> Result<(), super::super::Error> {
        revalidate_proof_unpinned(self.active, &self.proof, installation)
    }
}
