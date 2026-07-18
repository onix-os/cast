//! Pending semantic reconciliation after one archived-candidate child move.

mod reconciliation;

use std::io;

use super::{
    PendingArchivedCandidatePreservePostMoveDurability, ProjectedArchivedCandidatePreserveNamespace,
    RetainedArchivedCandidatePreserveParents,
};
use crate::client::startup_reconciliation::activation_namespace::capture::NamespaceSnapshot;

pub(in crate::client::startup_reconciliation::activation_namespace) use reconciliation::{
    AppliedArchivedCandidatePreserveMoveReconciliation, ArchivedCandidatePreserveMoveReconciliation,
};
#[cfg(test)]
pub(in crate::client) use reconciliation::{
    arm_before_archived_candidate_preserve_move_reconciliation_capture,
    arm_before_archived_candidate_preserve_move_reconciliation_closing,
};

#[must_use = "an archived candidate move attempt must be reconciled against a fresh namespace"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct PendingArchivedCandidatePreserveMoveReconciliation
{
    parents: RetainedArchivedCandidatePreserveParents,
    authenticated_pre: NamespaceSnapshot,
    authenticated_pre_projection: ProjectedArchivedCandidatePreserveNamespace,
    raw_report: io::Result<()>,
}

impl PendingArchivedCandidatePreserveMoveReconciliation {
    pub(super) fn new(
        parents: RetainedArchivedCandidatePreserveParents,
        authenticated_pre: NamespaceSnapshot,
        authenticated_pre_projection: ProjectedArchivedCandidatePreserveNamespace,
        raw_report: io::Result<()>,
    ) -> Self {
        Self {
            parents,
            authenticated_pre,
            authenticated_pre_projection,
            raw_report,
        }
    }
}

impl AppliedArchivedCandidatePreserveMoveReconciliation {
    /// Erase the raw syscall report and enter the common POST durability suffix.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn into_post_move_durability(
        self,
    ) -> PendingArchivedCandidatePreservePostMoveDurability {
        let Self {
            _parents: parents,
            _fresh_post: fresh_post,
            _fresh_post_projection: fresh_post_projection,
            _raw_report: _,
        } = self;
        PendingArchivedCandidatePreservePostMoveDurability::new(parents, fresh_post, fresh_post_projection)
    }
}
