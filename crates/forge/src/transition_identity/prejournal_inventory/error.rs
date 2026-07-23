use std::{io, path::PathBuf};

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CandidateInventoryBoundary {
    EntryCount,
    Depth,
    NameBytes,
    RegularBytes,
    OperationCount,
}

impl CandidateInventoryBoundary {
    fn as_str(self) -> &'static str {
        match self {
            Self::EntryCount => "entry-count",
            Self::Depth => "depth",
            Self::NameBytes => "name-byte-count",
            Self::RegularBytes => "regular-file-byte-count",
            Self::OperationCount => "operation-count",
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum CandidateInventoryError {
    #[error(
        "candidate pre-journal inventory exceeded its {} limit of {limit} at `{}`",
        boundary.as_str(),
        path.display()
    )]
    Boundary {
        boundary: CandidateInventoryBoundary,
        limit: u64,
        path: PathBuf,
    },
    #[error("candidate pre-journal inventory exceeded its deadline at `{}`", path.display())]
    Deadline { path: PathBuf },
    #[error("candidate pre-journal inventory deadline cannot be represented")]
    InvalidDeadline,
    #[error("candidate pre-journal inventory could not allocate bounded {resource} at `{}`", path.display())]
    Allocation { resource: &'static str, path: PathBuf },
    #[error("retained candidate root is not a directory at `{}`", path.display())]
    RootNotDirectory { path: PathBuf },
    #[error(
        "candidate inventory inode at `{}` is owned by uid {owner}, expected effective uid {expected}",
        path.display()
    )]
    UnexpectedOwner { path: PathBuf, owner: u32, expected: u32 },
    #[error("candidate inventory inode has unsafe mode {mode:04o} at `{}`", path.display())]
    UnsafeMode { path: PathBuf, mode: u32 },
    #[error(
        "candidate inventory inode carries {name_bytes} bytes of extended-attribute names at `{}`",
        path.display()
    )]
    ExtendedAttributes { path: PathBuf, name_bytes: usize },
    #[error("candidate inventory encountered a mounted or cross-device entry at `{}`", path.display())]
    MountedEntry { path: PathBuf },
    #[error("candidate inventory encountered special inode type {kind:#o} at `{}`", path.display())]
    SpecialInode { path: PathBuf, kind: u32 },
    #[error("candidate inventory encountered inode with unexpected link count {links} at `{}`", path.display())]
    UnexpectedHardlink { path: PathBuf, links: u64 },
    #[error(
        "candidate inventory encountered duplicate inode ({device}, {inode}) at `{}`",
        path.display()
    )]
    DuplicateInode { path: PathBuf, device: u64, inode: u64 },
    #[error("candidate inventory metadata field `{field}` changed at `{}`", path.display())]
    EntryChanged { path: PathBuf, field: &'static str },
    #[error("candidate inventory raw symlink target changed at `{}`", path.display())]
    SymlinkTargetChanged { path: PathBuf },
    #[error("candidate inventory sorted child-name set changed at `{}`", path.display())]
    ChildNamesChanged { path: PathBuf },
    #[error("canonical tree marker is missing after publication at `{}`", path.display())]
    MarkerMissingAfterPublication { path: PathBuf },
    #[error(
        "unsafe post-publication canonical tree marker at `{}` (type={kind:#o}, uid={owner}, mode={mode:04o}, links={links}, length={length})",
        path.display()
    )]
    UnsafeMarker {
        path: PathBuf,
        kind: u32,
        owner: u32,
        mode: u32,
        links: u64,
        length: u64,
    },
    #[error("post-publication canonical tree marker changed at `{}`", path.display())]
    MarkerChanged { path: PathBuf },
    #[error("{operation} candidate inventory entry `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub(super) fn inventory_io(
    operation: &'static str,
    path: impl Into<PathBuf>,
    source: io::Error,
) -> CandidateInventoryError {
    CandidateInventoryError::Io {
        operation,
        path: path.into(),
        source,
    }
}
