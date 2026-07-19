use std::{collections::TryReserveError, time::Instant};

use thiserror::Error;

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitDesiredPublicationError {
    #[error("desired boot-publication inventory deadline {actual:?} differs from the bound plan deadline {expected:?}")]
    DeadlineMismatch { expected: Instant, actual: Instant },
    #[error("desired boot-publication inventory exceeded its retained deadline at {checkpoint}")]
    DeadlineExceeded { checkpoint: &'static str },
    #[error("desired boot-publication count {actual} exceeds limit {limit}")]
    PublicationCountLimit { limit: usize, actual: usize },
    #[error("desired boot-publication inventory retained {actual} outputs, expected {expected}")]
    PublicationCountMismatch { expected: usize, actual: usize },
    #[error("desired boot-publication path bytes {actual} exceed limit {limit}")]
    PathByteLimit { limit: usize, actual: usize },
    #[error("desired boot-publication path has {actual} bytes, exceeding limit {limit}")]
    SinglePathByteLimit { limit: usize, actual: usize },
    #[error("desired boot-publication logical bytes {actual} exceed limit {limit}")]
    LogicalByteLimit { limit: u64, actual: u64 },
    #[error("desired boot-publication canonical bytes {actual} exceed limit {limit}")]
    CanonicalByteLimit { limit: usize, actual: usize },
    #[error("desired boot-publication canonical work {actual} exceeds limit {limit}")]
    WorkLimit { limit: usize, actual: usize },
    #[error("desired boot-publication path bytes differ from the bound publication plan")]
    PreparedPathByteMismatch,
    #[error("desired boot-publication logical bytes differ from the bound publication plan")]
    PreparedLogicalByteMismatch,
    #[error("desired boot-publication {field} length is not representable as u64")]
    ScalarNotRepresentable { field: &'static str },
    #[error("allocate {resource} for the desired boot-publication inventory")]
    Allocation {
        resource: &'static str,
        #[source]
        source: TryReserveError,
    },
}
