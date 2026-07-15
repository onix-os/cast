use std::{io, path::PathBuf, time::Duration};

use thiserror::Error as ThisError;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TestPoint {
    AfterEntryHandle,
    AfterDirectoryOpen,
    AfterRegularOpen,
    AfterRegularHash,
}

#[cfg(not(test))]
#[derive(Clone, Copy)]
pub(super) enum TestPoint {
    AfterEntryHandle,
    AfterDirectoryOpen,
    AfterRegularOpen,
    AfterRegularHash,
}

#[derive(Debug, ThisError)]
pub enum Error {
    #[error("no matching path rule for {path}", path = path.display())]
    NoMatchingRule { path: PathBuf },
    #[error("package path {path} is not valid UTF-8", path = path.display())]
    NonUtf8Path { path: PathBuf },
    #[error("symlink target at {path} is not valid UTF-8", path = path.display())]
    NonUtf8SymlinkTarget { path: PathBuf },
    #[error("invalid collection rule pattern {pattern:?}: {detail}")]
    InvalidRulePattern { pattern: String, detail: String },
    #[error("package path {path} is outside collector root {root}", path = path.display(), root = root.display())]
    OutsideRoot { root: PathBuf, path: PathBuf },
    #[error("invalid package path {path}: {detail}", path = path.display())]
    InvalidPath { path: PathBuf, detail: &'static str },
    #[error("generated package path {path} was declared more than once", path = path.display())]
    DuplicateAdmission { path: PathBuf },
    #[error("generated package path {path} was already present in the initial inventory", path = path.display())]
    ExistingAdmission { path: PathBuf },
    #[error("cannot {operation} while package inventory is {phase}")]
    InvalidInventoryPhase {
        operation: &'static str,
        phase: &'static str,
    },
    #[error("package inventory is poisoned by an incomplete or failed transition")]
    InventoryPoisoned,
    #[error("package path {path} is not present in the authenticated inventory", path = path.display())]
    UnwitnessedPath { path: PathBuf },
    #[error("unsupported package entry {path}: {kind}", path = path.display())]
    UnsupportedFileType { path: PathBuf, kind: &'static str },
    #[error("{resource} {actual} exceeds limit {limit} at {path}", path = path.display())]
    LimitExceeded {
        resource: &'static str,
        limit: u64,
        actual: u64,
        path: PathBuf,
    },
    #[error("collection exceeded {limit:?} while processing {path}", path = path.display())]
    DurationExceeded { path: PathBuf, limit: Duration },
    #[error("package tree changed at {path}: {detail}", path = path.display())]
    TreeChanged { path: PathBuf, detail: &'static str },
    #[error("package content at {path} lacks verified collection identity", path = path.display())]
    UnverifiedContent { path: PathBuf },
    #[error("package content length changed at {path}: expected {expected}, got {actual}", path = path.display())]
    ContentLengthChanged { path: PathBuf, expected: u64, actual: u64 },
    #[error("package content hash changed at {path}: expected {expected:032x}, got {actual:032x}", path = path.display())]
    ContentHashChanged {
        path: PathBuf,
        expected: u128,
        actual: u128,
    },
    #[error("arithmetic overflow for {resource} at {path}", path = path.display())]
    ArithmeticOverflow { resource: &'static str, path: PathBuf },
    #[error("failed to reserve {requested} units for {resource}: {detail}")]
    Allocation {
        resource: &'static str,
        requested: usize,
        detail: String,
    },
    #[error("collection accounting lock was poisoned")]
    StatePoisoned,
    #[error("regular-file replacement failed and rollback was incomplete at {path}: primary={primary}; cleanup={cleanup}", path = path.display())]
    MutationRollback {
        path: PathBuf,
        primary: Box<Error>,
        cleanup: Box<Error>,
    },
    #[error("regular-file replacement committed at {path}, but finalization is ambiguous: {primary}", path = path.display())]
    MutationCommitAmbiguous { path: PathBuf, primary: Box<Error> },
    #[error("generated-path publication failed and rollback was incomplete at {path}: primary={primary}; cleanup={cleanup}", path = path.display())]
    GeneratedPublicationRollback {
        path: PathBuf,
        primary: Box<Error>,
        cleanup: Box<Error>,
    },
    #[error("generated paths were admitted at {path}, but finalization is ambiguous: {primary}", path = path.display())]
    GeneratedPublicationCommitAmbiguous { path: PathBuf, primary: Box<Error> },
    #[error("{operation} failed for {path}", path = path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
