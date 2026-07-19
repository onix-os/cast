use std::time::Instant;

use thiserror::Error;

use super::observer::BootNamespaceNodeKind;

#[derive(Debug, Error)]
pub(crate) enum BootNamespaceAssessmentError {
    #[error("boot namespace assessment limit {field} must be nonzero and within its hard ceiling")]
    InvalidLimit { field: &'static str },
    #[error("boot namespace request count {found} exceeds limit {limit}")]
    RequestLimitExceeded { limit: usize, found: usize },
    #[error("boot namespace request {request_index} has a non-canonical relative path")]
    InvalidRequestPath { request_index: usize },
    #[error("boot namespace request {request_index} path length {found} exceeds limit {limit}")]
    RequestPathLimitExceeded {
        request_index: usize,
        limit: usize,
        found: usize,
    },
    #[error("boot namespace aggregate requested-path bytes {found} exceed limit {limit}")]
    TotalRequestPathBytesLimitExceeded { limit: usize, found: usize },
    #[error("boot namespace request {request_index} component count {found} exceeds limit {limit}")]
    RequestComponentLimitExceeded {
        request_index: usize,
        limit: usize,
        found: usize,
    },
    #[error("boot namespace request {request_index} component {component_index} length {found} exceeds limit {limit}")]
    RequestComponentNameLimitExceeded {
        request_index: usize,
        component_index: usize,
        limit: usize,
        found: usize,
    },
    #[error("boot namespace requests {first_request} and {second_request} collide in one ASCII-insensitive domain")]
    RequestCollision {
        first_request: usize,
        second_request: usize,
    },
    #[error("boot namespace requests {first_request} and {second_request} form a file/directory hierarchy collision")]
    RequestHierarchyCollision {
        first_request: usize,
        second_request: usize,
    },
    #[error("boot namespace root identity is zero or otherwise invalid")]
    InvalidRootIdentity,
    #[error("boot namespace inventory or lookup contains a zero or otherwise invalid identity")]
    InvalidObservedIdentity,
    #[error("boot namespace observer failed while {action}")]
    ObservationFailed { action: &'static str },
    #[error("boot namespace assessment exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("boot namespace work limit {limit} was exceeded while {action}")]
    WorkLimitExceeded { limit: usize, action: &'static str },
    #[error("boot namespace allocation limit {limit} was exceeded while {action}")]
    AllocationLimitExceeded { limit: usize, action: &'static str },
    #[error("boot namespace allocation failed while {action}")]
    AllocationFailed { action: &'static str },
    #[error(
        "boot namespace descriptor limit {limit} was exceeded at request {request_index} component {component_index}"
    )]
    DescriptorLimitExceeded {
        limit: usize,
        request_index: usize,
        component_index: usize,
    },
    #[error("boot namespace directory entry count {found} exceeds per-directory limit {limit}")]
    DirectoryEntryLimitExceeded { limit: usize, found: usize },
    #[error("boot namespace total entry count exceeds limit {limit}")]
    TotalEntryLimitExceeded { limit: usize },
    #[error("boot namespace raw name length {found} exceeds limit {limit}")]
    RawNameLimitExceeded { limit: usize, found: usize },
    #[error("boot namespace total raw-name bytes exceed limit {limit}")]
    TotalNameBytesLimitExceeded { limit: usize },
    #[error("boot namespace read bytes exceed limit {limit}")]
    ReadLimitExceeded { limit: u64 },
    #[error("boot namespace directory inventory contains an invalid raw name")]
    InvalidRawName,
    #[error("boot namespace directory inventory maps one raw name more than once")]
    DuplicateRawName,
    #[error("boot namespace directory inventory maps one identity from multiple raw names")]
    DuplicateIdentityMapping,
    #[error("request {request_index} component {component_index} collides with a raw ASCII-fold alias")]
    AsciiFoldAlias {
        request_index: usize,
        component_index: usize,
    },
    #[error("request {request_index} component {component_index} kernel lookup identity is absent from raw inventory")]
    LookupIdentityMissing {
        request_index: usize,
        component_index: usize,
    },
    #[error("request {request_index} component {component_index} is lookup-absent but present in raw inventory")]
    LookupAbsenceInventoryConflict {
        request_index: usize,
        component_index: usize,
    },
    #[error("request {request_index} component {component_index} resolves through a different raw name")]
    LookupRawNameMismatch {
        request_index: usize,
        component_index: usize,
    },
    #[error("request {request_index} component {component_index} lookup kind disagrees with raw inventory")]
    LookupKindMismatch {
        request_index: usize,
        component_index: usize,
    },
    #[error("request {request_index} component {component_index} absence was not stable")]
    UnstableAbsence {
        request_index: usize,
        component_index: usize,
    },
    #[error("request {request_index} component {component_index} lookup identity or kind changed")]
    LookupRace {
        request_index: usize,
        component_index: usize,
    },
    #[error("a requested parent directory inventory changed during assessment")]
    InventoryRace,
    #[error("request {request_index} component {component_index} crosses the retained root mount")]
    CrossMount {
        request_index: usize,
        component_index: usize,
    },
    #[error("request {request_index} component {component_index} is a symlink")]
    Symlink {
        request_index: usize,
        component_index: usize,
    },
    #[error("request {request_index} component {component_index} has kind {found:?}, expected {expected:?}")]
    WrongNodeKind {
        request_index: usize,
        component_index: usize,
        expected: BootNamespaceNodeKind,
        found: BootNamespaceNodeKind,
    },
    #[error("request {request_index} regular witness does not match its lookup identity")]
    RegularWitnessIdentityMismatch { request_index: usize },
    #[error("request {request_index} regular content witness changed during assessment")]
    RegularContentRace { request_index: usize },
    #[error("request {request_index} actual content stream disagrees with its stable witness")]
    ActualContentProtocolViolation { request_index: usize },
    #[error("request {request_index} expected content stream disagrees with declared length or digest")]
    ExpectedContentProtocolViolation { request_index: usize },
    #[error("request {request_index} {stream} content stream stopped before its declared length")]
    StreamStalled { request_index: usize, stream: &'static str },
    #[error("request {request_index} {stream} content stream returned more bytes than requested")]
    StreamOverflow { request_index: usize, stream: &'static str },
}
