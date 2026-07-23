use std::time::Instant;

use thiserror::Error;

#[derive(Debug, Error, Eq, PartialEq)]
pub(crate) enum ProductionRawDirectoryInventoryError {
    #[error("raw directory inventory limit {field} must be nonzero and within its production ceiling")]
    InvalidLimit { field: &'static str },
    #[error("raw directory inventory exceeded deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("raw directory source failed while {action}")]
    SourceFailed { action: &'static str },
    #[error("raw directory source reported {found} bytes for a {capacity}-byte buffer")]
    SourceProtocolViolation { capacity: usize, found: usize },
    #[error("raw directory read-call limit {limit} was exceeded")]
    ReadCallLimitExceeded { limit: usize },
    #[error("raw directory terminal-probe limit {limit} was exceeded")]
    EndProbeLimitExceeded { limit: usize },
    #[error("raw directory read-byte limit {limit} was exceeded")]
    ReadByteLimitExceeded { limit: usize },
    #[error("raw directory work limit {limit} was exceeded while {action}")]
    WorkLimitExceeded { limit: usize, action: &'static str },
    #[error("raw directory record limit {limit} was exceeded")]
    RecordLimitExceeded { limit: usize },
    #[error("raw directory name-byte limit {limit} was exceeded")]
    NameByteLimitExceeded { limit: usize },
    #[error("raw directory allocation-attempt limit {limit} was exceeded while {action}")]
    AllocationAttemptLimitExceeded { limit: usize, action: &'static str },
    #[error("raw directory allocation-byte limit {limit} was exceeded while {action}")]
    AllocationByteLimitExceeded { limit: usize, action: &'static str },
    #[error("raw directory allocation failed while {action}")]
    AllocationFailed { action: &'static str },
    #[error("raw directory record at byte {offset} has only {remaining} header bytes")]
    TruncatedRecordHeader { offset: usize, remaining: usize },
    #[error("raw directory record at byte {offset} has invalid length {found}")]
    RecordLengthTooSmall { offset: usize, found: usize },
    #[error("raw directory record at byte {offset} has unaligned length {found}")]
    RecordLengthUnaligned { offset: usize, found: usize },
    #[error("raw directory record at byte {offset} length {found} exceeds remaining chunk bytes {remaining}")]
    RecordOverrun {
        offset: usize,
        found: usize,
        remaining: usize,
    },
    #[error("raw directory record at byte {offset} has no terminating NUL")]
    MissingNameTerminator { offset: usize },
    #[error("raw directory record at byte {offset} has an empty name")]
    EmptyName { offset: usize },
    #[error("raw directory record at byte {offset} has name length {found} above {limit}")]
    NameTooLong { offset: usize, limit: usize, found: usize },
    #[error("raw directory record at byte {offset} contains a slash in its name")]
    NameContainsSlash { offset: usize },
}
