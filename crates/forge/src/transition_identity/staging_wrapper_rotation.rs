//! Fixed staging-wrapper lifecycle split by authority domain.
//!
//! `legacy_lifecycle` retains the established no-journal activation contract.
//! Journal-aware reservation lives separately so it cannot weaken clean-
//! baseline guards merely to share a namespace primitive.

use super::*;

mod fault_injection;
mod journal_reservation;
mod legacy_lifecycle;
mod model;
mod state_snapshot;

#[cfg(test)]
pub(crate) use fault_injection::{
    arm_before_staging_wrapper_exchange, arm_before_staging_wrapper_final_preparation_revalidation,
    arm_before_staging_wrapper_journal_validation, arm_staging_wrapper_rotation_faults,
};
pub(super) use journal_reservation::{
    ActiveReblitReservationError, RetainedActiveReblitReservation, RetainedActiveReblitReservationEvidenceFailure,
};
pub(super) use model::RetainedStagingWrapperRotation;
#[cfg(test)]
pub(crate) use model::RetainedStagingWrapperRotationFaultPoint;
pub(crate) use model::{RetainedStagingWrapperRotationFailure, RetainedStagingWrapperRotationOutcome};
#[cfg(test)]
pub(super) use model::{StagingWrapperPreparationEvidenceStage, StagingWrapperPreparationFailure};
