//! ActiveReblit whole-wrapper exchange process-death boundaries.

use crate::client::startup_reconciliation::{
    arm_before_active_reblit_candidate_preserve_durable_post_revalidation_capture,
    arm_before_active_reblit_candidate_preserve_post_exchange_candidate_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_candidate_wrapper_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_final_post_capture,
    arm_before_active_reblit_candidate_preserve_post_exchange_quarantine_parent_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_reservation_wrapper_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_roots_parent_sync,
    arm_before_active_reblit_candidate_preserve_reconciliation_capture,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateWrapperExchangeKillBoundary {
    PostExchangePreRecapture,
    BeforeCandidateSync,
    BeforeCandidateWrapperSync,
    BeforeReservationWrapperSync,
    BeforeRootsParentSync,
    BeforeQuarantineParentSync,
    BeforeFinalPostCapture,
    BeforeDurablePostRevalidation,
}

impl CandidateWrapperExchangeKillBoundary {
    pub(super) const ALL: [Self; 8] = [
        Self::PostExchangePreRecapture,
        Self::BeforeCandidateSync,
        Self::BeforeCandidateWrapperSync,
        Self::BeforeReservationWrapperSync,
        Self::BeforeRootsParentSync,
        Self::BeforeQuarantineParentSync,
        Self::BeforeFinalPostCapture,
        Self::BeforeDurablePostRevalidation,
    ];

    pub(super) fn parse(value: &str) -> Self {
        match value {
            "post-exchange-pre-recapture" => Self::PostExchangePreRecapture,
            "before-candidate-sync" => Self::BeforeCandidateSync,
            "before-candidate-wrapper-sync" => Self::BeforeCandidateWrapperSync,
            "before-reservation-wrapper-sync" => Self::BeforeReservationWrapperSync,
            "before-roots-parent-sync" => Self::BeforeRootsParentSync,
            "before-quarantine-parent-sync" => Self::BeforeQuarantineParentSync,
            "before-final-post-capture" => Self::BeforeFinalPostCapture,
            "before-durable-post-revalidation" => Self::BeforeDurablePostRevalidation,
            other => panic!("invalid ActiveReblit wrapper-exchange kill boundary {other:?}"),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::PostExchangePreRecapture => "post-exchange-pre-recapture",
            Self::BeforeCandidateSync => "before-candidate-sync",
            Self::BeforeCandidateWrapperSync => "before-candidate-wrapper-sync",
            Self::BeforeReservationWrapperSync => "before-reservation-wrapper-sync",
            Self::BeforeRootsParentSync => "before-roots-parent-sync",
            Self::BeforeQuarantineParentSync => "before-quarantine-parent-sync",
            Self::BeforeFinalPostCapture => "before-final-post-capture",
            Self::BeforeDurablePostRevalidation => "before-durable-post-revalidation",
        }
    }

    pub(super) fn expected_event_prefix_len(self) -> usize {
        match self {
            Self::PostExchangePreRecapture | Self::BeforeCandidateSync => 0,
            Self::BeforeCandidateWrapperSync => 1,
            Self::BeforeReservationWrapperSync => 2,
            Self::BeforeRootsParentSync => 3,
            Self::BeforeQuarantineParentSync => 4,
            Self::BeforeFinalPostCapture => 5,
            Self::BeforeDurablePostRevalidation => 6,
        }
    }

    pub(super) fn arm(self, kill: fn()) {
        match self {
            Self::PostExchangePreRecapture => arm_before_active_reblit_candidate_preserve_reconciliation_capture(kill),
            Self::BeforeCandidateSync => arm_before_active_reblit_candidate_preserve_post_exchange_candidate_sync(kill),
            Self::BeforeCandidateWrapperSync => {
                arm_before_active_reblit_candidate_preserve_post_exchange_candidate_wrapper_sync(kill)
            }
            Self::BeforeReservationWrapperSync => {
                arm_before_active_reblit_candidate_preserve_post_exchange_reservation_wrapper_sync(kill)
            }
            Self::BeforeRootsParentSync => {
                arm_before_active_reblit_candidate_preserve_post_exchange_roots_parent_sync(kill)
            }
            Self::BeforeQuarantineParentSync => {
                arm_before_active_reblit_candidate_preserve_post_exchange_quarantine_parent_sync(kill)
            }
            Self::BeforeFinalPostCapture => {
                arm_before_active_reblit_candidate_preserve_post_exchange_final_post_capture(kill)
            }
            Self::BeforeDurablePostRevalidation => {
                arm_before_active_reblit_candidate_preserve_durable_post_revalidation_capture(kill)
            }
        }
    }
}
