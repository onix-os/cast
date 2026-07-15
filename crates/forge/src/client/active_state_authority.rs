//! Strict retained authority for the live active-state selection.
//!
//! This layer deliberately has no missing or malformed `.stateID` fallback.
//! Recovery requires a restart-safe durable mapping between the exact live
//! `/usr` tree and its state row; until that mapping exists, construction and
//! verification fail closed on damaged active-state metadata.

use crate::{Installation, state};

use super::active_state_snapshot::{ActiveStateLease, ActiveStateSnapshot};

/// Non-cloneable strict authority retained across one stateful operation.
pub(super) struct ActiveStateAuthority(ActiveStateLease);

/// Exact authority snapshot which can cross an async cache operation without
/// retaining the process-local writer coordinator.
pub(super) struct SuspendedActiveStateAuthority(ActiveStateSnapshot);

impl ActiveStateAuthority {
    pub(super) fn acquire(installation: &Installation) -> Result<Self, super::Error> {
        ActiveStateLease::acquire(installation).map(Self)
    }

    pub(super) fn active(&self) -> Option<state::Id> {
        self.0.active()
    }

    pub(super) fn revalidate(&self, installation: &Installation) -> Result<(), super::Error> {
        self.0.revalidate(installation)
    }

    pub(super) fn refresh_after_tree_identity_preparation(
        &mut self,
        installation: &Installation,
    ) -> Result<(), super::Error> {
        self.0.refresh_after_tree_identity_preparation(installation)
    }

    pub(super) fn suspend(self, installation: &Installation) -> Result<SuspendedActiveStateAuthority, super::Error> {
        self.0.suspend(installation).map(SuspendedActiveStateAuthority)
    }
}

impl SuspendedActiveStateAuthority {
    pub(super) fn resume(self, installation: &Installation) -> Result<ActiveStateAuthority, super::Error> {
        self.0.resume(installation).map(ActiveStateAuthority)
    }
}
