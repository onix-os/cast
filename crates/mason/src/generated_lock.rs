//! Bounded, race-resistant reads of generated declaration lock files.
//!
//! The owning declaration codec supplies its exact source-byte limit. Paths
//! are inspected before allocation and the opened descriptor is checked again
//! before any bytes are read. `O_NOFOLLOW` and `O_NONBLOCK` ensure a path
//! replacement cannot turn the read into a symlink traversal or a blocking
//! FIFO/device read.

use std::{
    fmt,
    io::{self, Read},
    os::unix::fs::FileTypeExt as _,
    path::{Path, PathBuf},
};

use fs_err as fs;
use fs_err::os::unix::fs::OpenOptionsExt as _;
use thiserror::Error;

/// Read one generated lock without following links or allocating from an
/// untrusted file length.
pub(crate) fn read(path: &Path, source_byte_limit: usize) -> Result<Vec<u8>, ReadError> {
    let path_metadata = fs::symlink_metadata(path).map_err(|source| ReadError::Inspect {
        path: path.to_owned(),
        source,
    })?;
    require_regular(path, &path_metadata)?;
    require_within_limit(path, path_metadata.len(), source_byte_limit)?;

    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| ReadError::Open {
            path: path.to_owned(),
            source,
        })?;
    let opened_metadata = file.metadata().map_err(|source| ReadError::InspectOpened {
        path: path.to_owned(),
        source,
    })?;
    require_regular(path, &opened_metadata)?;
    require_within_limit(path, opened_metadata.len(), source_byte_limit)?;

    let initial_capacity = usize::try_from(opened_metadata.len())
        .unwrap_or(source_byte_limit)
        .min(source_byte_limit);
    let mut bytes = Vec::with_capacity(initial_capacity);
    file.by_ref()
        .take(
            u64::try_from(source_byte_limit)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(|source| ReadError::Read {
            path: path.to_owned(),
            source,
        })?;
    if bytes.len() > source_byte_limit {
        return Err(ReadError::TooLarge {
            path: path.to_owned(),
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            limit: source_byte_limit,
        });
    }
    Ok(bytes)
}

fn require_regular(path: &Path, metadata: &std::fs::Metadata) -> Result<(), ReadError> {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        Ok(())
    } else {
        Err(ReadError::NotRegular {
            path: path.to_owned(),
            kind: FileKind::from_file_type(&file_type),
        })
    }
}

fn require_within_limit(path: &Path, size: u64, limit: usize) -> Result<(), ReadError> {
    if size <= u64::try_from(limit).unwrap_or(u64::MAX) {
        Ok(())
    } else {
        Err(ReadError::TooLarge {
            path: path.to_owned(),
            size,
            limit,
        })
    }
}

/// The structural inode kind found where a regular generated lock was
/// required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Symlink,
    Directory,
    Fifo,
    Socket,
    BlockDevice,
    CharacterDevice,
    Other,
}

impl FileKind {
    fn from_file_type(file_type: &std::fs::FileType) -> Self {
        if file_type.is_symlink() {
            Self::Symlink
        } else if file_type.is_dir() {
            Self::Directory
        } else if file_type.is_fifo() {
            Self::Fifo
        } else if file_type.is_socket() {
            Self::Socket
        } else if file_type.is_block_device() {
            Self::BlockDevice
        } else if file_type.is_char_device() {
            Self::CharacterDevice
        } else {
            Self::Other
        }
    }
}

impl fmt::Display for FileKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Symlink => "symbolic link",
            Self::Directory => "directory",
            Self::Fifo => "FIFO",
            Self::Socket => "socket",
            Self::BlockDevice => "block device",
            Self::CharacterDevice => "character device",
            Self::Other => "non-regular file",
        })
    }
}

/// Failure to safely read a generated lock.
#[derive(Debug, Error)]
pub enum ReadError {
    #[error("inspect generated lock {path:?}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("generated lock {path:?} must be a regular file, found {kind}")]
    NotRegular { path: PathBuf, kind: FileKind },
    #[error("generated lock {path:?} is {size} bytes, exceeding the {limit}-byte limit")]
    TooLarge { path: PathBuf, size: u64, limit: usize },
    #[error("open generated lock {path:?} without following links")]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("inspect opened generated lock {path:?}")]
    InspectOpened {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read generated lock {path:?}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl ReadError {
    pub(crate) fn is_not_found(&self) -> bool {
        // Only absence at the initial path inspection is a missing lock. Once
        // an inode was observed, disappearance before open is a race and must
        // fail closed instead of being reclassified as an optional lock.
        matches!(self, Self::Inspect { source, .. } if source.kind() == io::ErrorKind::NotFound)
    }

    pub(crate) fn into_io_error(self) -> io::Error {
        let kind = match &self {
            Self::Inspect { source, .. }
            | Self::Open { source, .. }
            | Self::InspectOpened { source, .. }
            | Self::Read { source, .. } => source.kind(),
            Self::NotRegular { .. } => io::ErrorKind::InvalidInput,
            Self::TooLarge { .. } => io::ErrorKind::InvalidData,
        };
        io::Error::new(kind, self)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use nix::sys::stat::Mode;

    use super::*;

    const TEST_SOURCE_BYTE_LIMIT: usize = 1_024;

    #[test]
    fn reads_regular_locks_with_the_evaluator_source_limit() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sources.lock.glu");
        fs::write(&path, b"lock").unwrap();

        assert_eq!(read(&path, TEST_SOURCE_BYTE_LIMIT).unwrap(), b"lock");
    }

    #[test]
    fn only_initial_absence_is_classified_as_a_missing_lock() {
        let path = PathBuf::from("sources.lock.glu");
        let missing = ReadError::Inspect {
            path: path.clone(),
            source: io::Error::from(io::ErrorKind::NotFound),
        };
        let vanished_after_inspection = ReadError::Open {
            path,
            source: io::Error::from(io::ErrorKind::NotFound),
        };

        assert!(missing.is_not_found());
        assert!(!vanished_after_inspection.is_not_found());
    }

    #[test]
    fn rejects_dense_and_sparse_oversized_locks_before_unbounded_allocation() {
        let root = tempfile::tempdir().unwrap();
        let limit = TEST_SOURCE_BYTE_LIMIT;
        let dense = root.path().join("dense.lock.glu");
        fs::write(&dense, vec![b'x'; limit + 1]).unwrap();

        let error = read(&dense, limit).unwrap_err();
        assert!(
            matches!(error, ReadError::TooLarge { size, limit: found, .. } if size == limit as u64 + 1 && found == limit)
        );

        let sparse = root.path().join("sparse.lock.glu");
        fs::File::create(&sparse)
            .unwrap()
            .set_len(16 * 1024 * 1024 * 1024)
            .unwrap();

        let error = read(&sparse, limit).unwrap_err();
        assert!(
            matches!(error, ReadError::TooLarge { size, limit: found, .. } if size == 16 * 1024 * 1024 * 1024 && found == limit)
        );
    }

    #[test]
    fn rejects_symlinks_without_reading_the_target() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let link = root.path().join("sources.lock.glu");
        fs::write(&target, b"lock").unwrap();
        symlink(&target, &link).unwrap();

        let error = read(&link, TEST_SOURCE_BYTE_LIMIT).unwrap_err();
        assert!(matches!(
            error,
            ReadError::NotRegular {
                kind: FileKind::Symlink,
                ..
            }
        ));
    }

    #[test]
    fn rejects_directories_and_fifos_structurally_without_opening_them() {
        let root = tempfile::tempdir().unwrap();

        let error = read(root.path(), TEST_SOURCE_BYTE_LIMIT).unwrap_err();
        assert!(matches!(
            error,
            ReadError::NotRegular {
                kind: FileKind::Directory,
                ..
            }
        ));

        let fifo = root.path().join("build.lock.glu");
        nix::unistd::mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let error = read(&fifo, TEST_SOURCE_BYTE_LIMIT).unwrap_err();
        assert!(matches!(
            error,
            ReadError::NotRegular {
                kind: FileKind::Fifo,
                ..
            }
        ));
    }
}
