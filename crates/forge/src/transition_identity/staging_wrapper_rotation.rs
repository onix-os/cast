//! Fixed staging-wrapper lifecycle split by authority domain.
//!
//! `legacy_lifecycle` retains the established no-journal activation contract.
//! Journal-aware reservation lives separately so it cannot weaken clean-
//! baseline guards merely to share a namespace primitive.

use super::*;

mod fault_injection;
mod legacy_lifecycle;
mod state_snapshot;

#[cfg(test)]
pub(crate) use fault_injection::{arm_before_staging_wrapper_exchange, arm_staging_wrapper_rotation_faults};
pub(super) use legacy_lifecycle::RetainedStagingWrapperRotation;
#[cfg(test)]
pub(crate) use legacy_lifecycle::RetainedStagingWrapperRotationFaultPoint;
pub(crate) use legacy_lifecycle::{RetainedStagingWrapperRotationFailure, RetainedStagingWrapperRotationOutcome};
