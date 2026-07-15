use crate::{Installation, state};

use super::{ActiveStateProof, capture, revalidate_proof};

/// Exact live-state evidence retained by a read-only client.
///
/// Unlike `ActiveStateLease`, this proof never acquires the process-local
/// writer coordinator. The explicit Installation snapshot already holds the
/// global shared lock, so taking the coordinator after the journal lock would
/// add an unnecessary reverse lock order to a non-mutating client.
pub(in crate::client) struct ReadOnlyActiveStateSnapshot {
    active: Option<state::Id>,
    proof: ActiveStateProof,
}

impl ReadOnlyActiveStateSnapshot {
    pub(in crate::client) fn capture(installation: &Installation) -> Result<Self, super::super::Error> {
        installation.revalidate_read_only_snapshot()?;
        let captured = capture(installation)?;
        let actual = captured.active;
        let expected = installation.active_state;
        if actual != expected {
            return Err(super::super::Error::ActiveStateSnapshotChanged { expected, actual });
        }
        let snapshot = Self {
            active: actual,
            proof: captured.proof,
        };
        snapshot.revalidate(installation)?;
        installation.revalidate_read_only_snapshot()?;
        Ok(snapshot)
    }

    pub(in crate::client) fn active(&self) -> Option<state::Id> {
        self.active
    }

    pub(in crate::client) fn revalidate(&self, installation: &Installation) -> Result<(), super::super::Error> {
        installation.revalidate_read_only_snapshot()?;
        revalidate_proof(self.active, &self.proof, installation)?;
        installation.revalidate_read_only_snapshot()?;
        Ok(())
    }
}
