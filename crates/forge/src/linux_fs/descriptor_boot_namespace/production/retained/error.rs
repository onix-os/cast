use std::{io, time::Instant};

use thiserror::Error;

use super::super::super::error::BootNamespaceAssessmentError;
use super::super::error::ProductionRawDirectoryInventoryError;

#[derive(Debug, Error)]
pub(crate) enum RetainedBootNamespaceAssessmentError {
    #[error("retained boot namespace live limit {field} must be nonzero and within its hard ceiling")]
    InvalidLiveLimit { field: &'static str },
    #[error("retained boot namespace expected stream count {found} does not match request count {expected}")]
    ExpectedCountMismatch { expected: usize, found: usize },
    #[error("retained boot namespace expected stream {request_index} has length {found}, declared {expected}")]
    ExpectedLengthMismatch {
        request_index: usize,
        expected: u64,
        found: usize,
    },
    #[error("retained boot namespace expected stream {request_index} does not match its declared digest")]
    ExpectedDigestMismatch { request_index: usize },
    #[error("retained boot namespace live {field} budget {limit} was exceeded while {action}")]
    LiveBudgetExceeded {
        field: &'static str,
        limit: u64,
        action: &'static str,
    },
    #[error("retained boot namespace exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("retained boot namespace filesystem observation failed while {action}: {source}")]
    Filesystem {
        action: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("retained boot namespace raw inventory failed: {source}")]
    RawInventory {
        #[source]
        source: ProductionRawDirectoryInventoryError,
    },
    #[error("retained boot namespace observer protocol failed: {reason}")]
    ObserverProtocol { reason: &'static str },
    #[error("retained boot namespace allocation failed while {action}: {source}")]
    Allocation {
        action: &'static str,
        #[source]
        source: std::collections::TryReserveError,
    },
    #[error(transparent)]
    Namespace(#[from] BootNamespaceAssessmentError),
}
