//! Namespace bridge into the shared preserved-candidate durability suffix.
//!
//! Freshly applied and independently admitted POST evidence remain distinct
//! until each has produced the same low-level consuming capability.

use crate::{Installation, transition_journal::TransitionRecord};

use super::UsrRollbackNewStateCandidatePreserveAppliedNamespace;
use crate::client::startup_reconciliation::activation_namespace::{
    UsrRollbackCandidatePreserveNamespaceError, UsrRollbackCandidatePreserveNamespaceProof,
    UsrRollbackCandidatePreserveTopology,
    capture::{DurableNewStateCandidatePreservePostMoveNamespace, PendingNewStateCandidatePreservePostMoveDurability},
};

/// Exact preserved NewState namespace admitted without a move attempt in this
/// startup entry.
#[must_use = "already-preserved NewState evidence still requires post-move durability"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackNewStateCandidatePreserveAlreadySatisfiedNamespace {
    pending: PendingNewStateCandidatePreservePostMoveDurability,
}

/// Common namespace proof after either origin completed every POST barrier.
#[must_use = "durable NewState candidate-preservation namespace evidence must remain sealed"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackNewStateCandidatePreserveDurableNamespace {
    _namespace: DurableNewStateCandidatePreservePostMoveNamespace,
}

impl UsrRollbackCandidatePreserveNamespaceProof {
    /// Consume only exact already-preserved NewState evidence into the common
    /// POST suffix input. Archived and ActiveReblit proofs cannot call this.
    pub(in crate::client::startup_reconciliation) fn into_new_state_preserved_durability_evidence(
        self,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackNewStateCandidatePreserveAlreadySatisfiedNamespace, UsrRollbackCandidatePreserveNamespaceError>
    {
        let Self {
            before: _,
            after,
            topology,
        } = self;
        if topology != UsrRollbackCandidatePreserveTopology::NewStatePreserved {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }
        let pending = PendingNewStateCandidatePreservePostMoveDurability::capture_preserved(after, record)?;
        Ok(UsrRollbackNewStateCandidatePreserveAlreadySatisfiedNamespace { pending })
    }
}

impl UsrRollbackNewStateCandidatePreserveAppliedNamespace {
    /// Consume freshly applied POST evidence through the shared suffix.
    pub(in crate::client::startup_reconciliation) fn complete_post_move_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackNewStateCandidatePreserveDurableNamespace, UsrRollbackCandidatePreserveNamespaceError> {
        let pending = self._reconciliation.into_post_move_durability();
        let namespace = pending.complete(installation, record)?;
        Ok(UsrRollbackNewStateCandidatePreserveDurableNamespace { _namespace: namespace })
    }
}

impl UsrRollbackNewStateCandidatePreserveAlreadySatisfiedNamespace {
    /// Consume independently admitted POST evidence through the identical
    /// candidate and parent barrier sequence.
    pub(in crate::client::startup_reconciliation) fn complete_post_move_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackNewStateCandidatePreserveDurableNamespace, UsrRollbackCandidatePreserveNamespaceError> {
        let namespace = self.pending.complete(installation, record)?;
        Ok(UsrRollbackNewStateCandidatePreserveDurableNamespace { _namespace: namespace })
    }
}
