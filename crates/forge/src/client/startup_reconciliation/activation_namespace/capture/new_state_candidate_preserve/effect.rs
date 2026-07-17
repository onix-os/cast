//! Pending semantic reconciliation after one target-durable NewState move.
//!
//! The raw syscall report is diagnostic only. It remains sealed behind a
//! pending-reconciliation value until a fresh namespace capture classifies the
//! actual effect.

mod reconciliation;

use std::io;

use super::{ProjectedNewStateCandidatePreserveNamespace, RetainedNewStateCandidatePreserveParents};
use crate::client::startup_reconciliation::activation_namespace::capture::NamespaceSnapshot;

#[cfg(test)]
pub(in crate::client) use reconciliation::arm_before_new_state_candidate_preserve_move_reconciliation_capture;
pub(in crate::client::startup_reconciliation::activation_namespace) use reconciliation::{
    AppliedNewStateCandidatePreserveMoveReconciliation, NewStateCandidatePreserveMoveReconciliation,
};

#[must_use = "a NewState candidate move attempt must be reconciled against a fresh namespace"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct PendingNewStateCandidatePreserveMoveReconciliation
{
    parents: RetainedNewStateCandidatePreserveParents,
    authenticated_pre: NamespaceSnapshot,
    authenticated_pre_projection: ProjectedNewStateCandidatePreserveNamespace,
    raw_report: io::Result<()>,
}

impl PendingNewStateCandidatePreserveMoveReconciliation {
    pub(super) fn new(
        parents: RetainedNewStateCandidatePreserveParents,
        authenticated_pre: NamespaceSnapshot,
        authenticated_pre_projection: ProjectedNewStateCandidatePreserveNamespace,
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
