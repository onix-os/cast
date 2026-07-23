//! Descriptor-bound durability for the parent of staged `/usr`.
//!
//! This capability is deliberately narrower than transition execution. It
//! syncs the exact retained `.cast/root/staging` directory and performs no
//! namespace mutation, path reopen, exchange, or journal operation.

use std::io;

use super::{CaptureError, UsrRollbackDecisionNamespaceError, UsrRollbackDecisionNamespaceProof};

impl UsrRollbackDecisionNamespaceProof {
    /// Sync the exact retained staging parent, sandwiching the durability
    /// barrier between complete retained-namespace revalidations.
    pub(in crate::client::startup_reconciliation) fn sync_retained_staging_parent(
        &self,
        before_sync: impl FnOnce() -> io::Result<()>,
    ) -> Result<(u64, u64), UsrRollbackDecisionNamespaceError> {
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_inventories(self)?;

        let (staging, path, (device, inode)) = self.after.retained_staging_parent()?;
        before_sync().map_err(|source| CaptureError::Io {
            operation: "sync retained staging parent during startup /usr durability normalization",
            path: path.clone(),
            source,
        })?;
        staging.sync_all().map_err(|source| CaptureError::Io {
            operation: "sync retained staging parent during startup /usr durability normalization",
            path,
            source,
        })?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_inventories(self)?;
        Ok((device, inode))
    }
}

fn require_matching_inventories(
    proof: &UsrRollbackDecisionNamespaceProof,
) -> Result<(), UsrRollbackDecisionNamespaceError> {
    if proof.before.fingerprint() == proof.after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackDecisionNamespaceError::NamespaceChanged)
    }
}
