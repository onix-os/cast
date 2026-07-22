use std::{collections::TryReserveError, io, time::Instant};

use thiserror::Error;

use super::super::boot_file_publication::RetainedBootFilePublicationError;

#[derive(Debug, Error)]
pub(crate) enum RetainedBootLeafAssessmentError {
    #[error("boot-leaf assessment exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("boot-leaf assessment leaf is not one bounded canonical ASCII component")]
    InvalidCanonicalLeaf,
    #[error("boot-leaf assessment leaf enters the case-insensitive private mutation namespace")]
    ReservedPrivateLeaf,
    #[error("boot-leaf assessment requires at least one validated parent component")]
    EmptyParentComponents,
    #[error("boot-leaf assessment has {actual} parent components, exceeding the {limit}-component ceiling")]
    ParentComponentLimit { limit: usize, actual: usize },
    #[error("boot-leaf assessment parent component {index} is not one bounded raw component")]
    InvalidParentComponent { index: usize },
    #[error("boot-leaf assessment limit {field} must be nonzero and within its hard ceiling")]
    InvalidLimit { field: &'static str },
    #[error("boot-leaf assessment length {length} exceeds the admitted {limit}-byte ceiling")]
    LengthLimitExceeded { length: u64, limit: u64 },
    #[error("boot-leaf assessment requires {required} reads, exceeding the admitted {limit}-call ceiling")]
    ReadCallLimitExceeded { required: usize, limit: usize },
    #[error("copying the validated canonical boot leaf failed: {source}")]
    Allocation {
        #[source]
        source: TryReserveError,
    },
    #[error("retained boot-publication parent failed revalidation while {action}: {source}")]
    ParentRevalidation {
        action: &'static str,
        #[source]
        source: RetainedBootFilePublicationError,
    },
    #[error("boot-leaf assessment filesystem operation failed while {action}: {source}")]
    Filesystem {
        action: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("boot-leaf assessment found a symlink or nonregular canonical leaf")]
    UnsafeLeafType,
    #[error("boot-leaf assessment found regular canonical leaf link count {found}, expected exactly one")]
    UnsafeLinkCount { found: u64 },
    #[error("boot-leaf assessment parent component {index} is a symlink or non-directory")]
    UnsafeParentType { index: usize },
    #[error("boot-leaf assessment parent component {index} has unsafe ownership or permissions")]
    UnsafeParentPolicy { index: usize },
    #[error("boot-leaf assessment crossed or lost the retained parent attachment while {action}")]
    AttachmentIdentityChanged { action: &'static str },
    #[error("boot-leaf identity or metadata changed while {action}")]
    LeafIdentityChanged { action: &'static str },
}
