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

/// Opaque continuation of the cooperating-writer lease after an intentional
/// `/usr` exchange invalidates the old-live namespace proof.
///
/// This type deliberately exposes no revalidation or refresh operation.  It
/// keeps the writer mutex held for the next coordinator phase without letting
/// post-effect code accidentally treat the displaced live-tree proof as
/// current authority.
pub(super) struct AppliedActiveStateWriterAuthority(ActiveStateLease);

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

    pub(super) fn into_applied_writer_authority(self) -> AppliedActiveStateWriterAuthority {
        AppliedActiveStateWriterAuthority(self.0)
    }
}

impl std::fmt::Debug for AppliedActiveStateWriterAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _lease = &self.0;
        formatter.write_str("AppliedActiveStateWriterAuthority")
    }
}

impl SuspendedActiveStateAuthority {
    pub(super) fn resume(self, installation: &Installation) -> Result<ActiveStateAuthority, super::Error> {
        self.0.resume(installation).map(ActiveStateAuthority)
    }
}
