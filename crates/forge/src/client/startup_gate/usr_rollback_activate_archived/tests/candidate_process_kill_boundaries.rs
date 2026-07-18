//! ActivateArchived candidate-preservation process-death boundaries.

use crate::client::startup_reconciliation::{
    arm_before_archived_candidate_preserve_durable_post_revalidation_capture,
    arm_before_archived_candidate_preserve_move_reconciliation_capture,
    arm_before_archived_candidate_preserve_post_candidate_sync,
    arm_before_archived_candidate_preserve_post_final_capture,
    arm_before_archived_candidate_preserve_post_roots_parent_sync,
    arm_before_archived_candidate_preserve_post_staging_parent_sync,
    arm_before_archived_candidate_preserve_post_target_parent_sync,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateProcessKillBoundary {
    PostMovePreRecapture,
    BeforeCandidateSync,
    BeforeStagingParentSync,
    BeforeTargetParentSync,
    BeforeRootsParentSync,
    BeforeFinalPostCapture,
    BeforeDurablePostRevalidation,
}

impl CandidateProcessKillBoundary {
    pub(super) const ALL: [Self; 7] = [
        Self::PostMovePreRecapture,
        Self::BeforeCandidateSync,
        Self::BeforeStagingParentSync,
        Self::BeforeTargetParentSync,
        Self::BeforeRootsParentSync,
        Self::BeforeFinalPostCapture,
        Self::BeforeDurablePostRevalidation,
    ];

    pub(super) fn parse(value: &str) -> Self {
        match value {
            "post-move-pre-recapture" => Self::PostMovePreRecapture,
            "before-candidate-sync" => Self::BeforeCandidateSync,
            "before-staging-parent-sync" => Self::BeforeStagingParentSync,
            "before-target-parent-sync" => Self::BeforeTargetParentSync,
            "before-roots-parent-sync" => Self::BeforeRootsParentSync,
            "before-final-post-capture" => Self::BeforeFinalPostCapture,
            "before-durable-post-revalidation" => Self::BeforeDurablePostRevalidation,
            other => panic!("invalid ActivateArchived candidate process-kill boundary {other:?}"),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::PostMovePreRecapture => "post-move-pre-recapture",
            Self::BeforeCandidateSync => "before-candidate-sync",
            Self::BeforeStagingParentSync => "before-staging-parent-sync",
            Self::BeforeTargetParentSync => "before-target-parent-sync",
            Self::BeforeRootsParentSync => "before-roots-parent-sync",
            Self::BeforeFinalPostCapture => "before-final-post-capture",
            Self::BeforeDurablePostRevalidation => "before-durable-post-revalidation",
        }
    }

    pub(super) fn arm(self, kill: fn()) {
        match self {
            Self::PostMovePreRecapture => arm_before_archived_candidate_preserve_move_reconciliation_capture(kill),
            Self::BeforeCandidateSync => arm_before_archived_candidate_preserve_post_candidate_sync(kill),
            Self::BeforeStagingParentSync => arm_before_archived_candidate_preserve_post_staging_parent_sync(kill),
            Self::BeforeTargetParentSync => arm_before_archived_candidate_preserve_post_target_parent_sync(kill),
            Self::BeforeRootsParentSync => arm_before_archived_candidate_preserve_post_roots_parent_sync(kill),
            Self::BeforeFinalPostCapture => arm_before_archived_candidate_preserve_post_final_capture(kill),
            Self::BeforeDurablePostRevalidation => {
                arm_before_archived_candidate_preserve_durable_post_revalidation_capture(kill)
            }
        }
    }
}
