//! Strict, durable storage for the single in-flight state transition.
//!
//! This module deliberately contains no recovery policy. It defines only the
//! versioned record contract and the descriptor-relative durability boundary
//! which a later recovery state machine can consume.

use std::{
    ffi::{CStr, CString},
    io::{self, Read as _},
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd},
        unix::{
            ffi::OsStrExt as _,
            fs::{MetadataExt as _, PermissionsExt as _},
        },
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use nix::unistd::Uid;
use thiserror::Error;

use crate::linux_fs::chmod_path_descriptor;

#[cfg(test)]
use crate::state::TransitionId;

mod codec;
mod model;
mod recovery;
mod runtime_evidence;
mod store;
mod successors;
mod validation;

#[allow(dead_code)] // completed substrate; consumed by the next read-only-client slice
mod read_only;
#[allow(unused_imports)] // deliberate internal surface for the next read-only-client slice
pub(crate) use read_only::{CleanReadOnlyJournal, ReadOnlyJournalError};

pub(crate) use codec::{CodecError, MAX_CANONICAL_RECORD_BYTES, decode, encode};
pub(crate) use model::*;
#[allow(unused_imports)] // consumed by startup reconciliation in the next saved increment
pub(crate) use recovery::RecoveryDisposition;
pub(crate) use runtime_evidence::RuntimeEvidenceError;
pub(crate) use store::TransitionJournalStore;
#[allow(unused_imports)] // deliberate internal surface for the next durable coordinator slice
pub(crate) use successors::{InitialRollbackAction, RollbackActionOutcome, RollbackObservations};

#[cfg(test)]
use codec::{
    CHECKSUM_END, FRAME_VERSION, HEADER_SIZE, MAGIC, MAGIC_END, MAX_QUARANTINE_NAME_BYTES, PAYLOAD_FORMAT,
    PAYLOAD_VERSION, VERSION_END, checksum, enforce_record_size,
};
#[cfg(test)]
use store::{
    DurabilityCheckpoint, StorageFaultPoint, arm_storage_fault, assert_storage_fault_consumed,
    take_durability_checkpoints,
};

/// Arm the first temporary-file sync in the next journal update. Exposed only
/// to sibling contract tests so they can prove a consuming coordinator cannot
/// continue after a storage failure.
#[cfg(test)]
pub(crate) fn arm_next_temporary_sync_fault() {
    arm_storage_fault(StorageFaultPoint::TemporarySync);
}

#[cfg(test)]
pub(crate) fn assert_temporary_sync_fault_consumed() {
    assert_storage_fault_consumed();
}
#[cfg(test)]
use validation::{next_forward_phase, next_rollback_phase, rollback_allowed, validate_advance};

const JOURNAL_DIRECTORY: &CStr = c"journal";
const CANONICAL_NAME: &CStr = c"state-transition";
const LOCK_NAME: &CStr = c"state-transition.lock";
const JOURNAL_DIRECTORY_MODE: u32 = 0o700;
const JOURNAL_FILE_MODE: u32 = 0o600;
const MAX_STALE_TEMPORARIES: usize = 256;
const TEMPORARY_PREFIX: &[u8] = b".state-transition.tmp-";

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InodeIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Error)]
pub(crate) enum StorageError {
    #[error("the in-process transition-journal operation lock is poisoned")]
    OperationLockPoisoned,
    #[error("open transition-journal root `{}`", path.display())]
    OpenRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open owner-controlled Cast directory `{}`", path.display())]
    OpenCastDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open owner-controlled journal directory `{}`", path.display())]
    OpenJournalDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open canonical state-transition journal")]
    OpenCanonical {
        #[source]
        source: io::Error,
    },
    #[error("validate canonical state-transition journal")]
    ValidateCanonical {
        #[source]
        source: io::Error,
    },
    #[error("read canonical state-transition journal")]
    ReadCanonical {
        #[source]
        source: io::Error,
    },
    #[error("decode canonical state-transition journal")]
    Decode(#[source] CodecError),
    #[error("encode state-transition journal")]
    Encode(#[source] CodecError),
    #[error("the initial journal record must be generation 1 in preparing phase")]
    InvalidCreationRecord,
    #[error("canonical state-transition journal already exists")]
    CanonicalAlreadyExists,
    #[error("canonical state-transition journal is absent")]
    CanonicalMissing,
    #[error("canonical state-transition journal does not match the caller's exact expected record")]
    ExpectedRecordMismatch,
    #[error("invalid state-transition journal advance")]
    InvalidAdvance(#[source] CodecError),
    #[error("a nonterminal state-transition journal cannot be deleted")]
    DeleteNonterminal,
    #[error("create or open the internal transition-journal lock")]
    OpenLock {
        #[source]
        source: io::Error,
    },
    #[error("validate the internal transition-journal lock")]
    ValidateLock {
        #[source]
        source: io::Error,
    },
    #[error("acquire the internal exclusive transition-journal lock")]
    AcquireLock {
        #[source]
        source: io::Error,
    },
    #[error("journal directory contains unexpected entry `{0}`")]
    UnexpectedJournalEntry(String),
    #[error("journal contains more than {MAX_STALE_TEMPORARIES} stale temporary records")]
    TooManyStaleTemporaries,
    #[error("enumerate bounded transition-journal entries")]
    EnumerateJournal {
        #[source]
        source: io::Error,
    },
    #[error("validate stale transition-journal temporary `{name}`")]
    ValidateStaleTemporary {
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("remove authenticated transition-journal temporary `{name}`")]
    CleanupTemporary {
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("create an exclusive state-transition temporary file")]
    CreateTemporary {
        #[source]
        source: io::Error,
    },
    #[error("all bounded state-transition temporary names already exist")]
    TemporaryNamesExhausted,
    #[error("validate state-transition temporary file")]
    ValidateTemporary {
        #[source]
        source: io::Error,
    },
    #[error("write complete state-transition temporary file")]
    WriteTemporary {
        #[source]
        source: io::Error,
    },
    #[error("fdatasync state-transition temporary file")]
    SyncTemporary {
        #[source]
        source: io::Error,
    },
    #[error("atomically publish state-transition journal")]
    PublishCanonical {
        #[source]
        source: io::Error,
    },
    #[error("canonical state-transition inode changed during the operation")]
    CanonicalChanged,
    #[error("delete canonical state-transition journal")]
    DeleteCanonical {
        #[source]
        source: io::Error,
    },
    #[error("delete displaced state-transition journal")]
    DeleteDisplaced {
        #[source]
        source: io::Error,
    },
    #[error("fsync state-transition journal directory")]
    SyncJournalDirectory {
        #[source]
        source: io::Error,
    },
}

fn temporary_name() -> CString {
    let process = std::process::id();
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    CString::new(format!(".state-transition.tmp-{process:08x}-{sequence:016x}"))
        .expect("internally generated journal temporary name contains no NUL")
}

fn open_and_lock(directory: &std::fs::File, path: &Path) -> Result<std::fs::File, StorageError> {
    let create_flags = nix::libc::O_RDWR
        | nix::libc::O_CLOEXEC
        | nix::libc::O_NOFOLLOW
        | nix::libc::O_NONBLOCK
        | nix::libc::O_CREAT
        | nix::libc::O_EXCL;
    let file = match openat2_file(
        directory.as_raw_fd(),
        LOCK_NAME,
        create_flags,
        JOURNAL_FILE_MODE,
        controlled_resolution(),
    ) {
        Ok(file) => {
            // The create mode is only an upper bound under umask. Normalize
            // the already-open inode rather than its public name.
            file.set_permissions(std::fs::Permissions::from_mode(JOURNAL_FILE_MODE))
                .map_err(|source| StorageError::OpenLock { source })?;
            file
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            let pinned = openat2_file(
                directory.as_raw_fd(),
                LOCK_NAME,
                nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                0,
                controlled_resolution(),
            )
            .map_err(|source| StorageError::OpenLock { source })?;
            let mode = recoverable_private_lock_mode(&pinned, &path.join("state-transition.lock"))
                .map_err(|source| StorageError::ValidateLock { source })?;
            if mode != JOURNAL_FILE_MODE {
                chmod_path_descriptor(&pinned, JOURNAL_FILE_MODE)
                    .map_err(|source| StorageError::OpenLock { source })?;
            }
            require_safe_regular_file(&pinned, &path.join("state-transition.lock"))
                .map_err(|source| StorageError::ValidateLock { source })?;

            let file = openat2_file(
                directory.as_raw_fd(),
                LOCK_NAME,
                nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
                0,
                controlled_resolution(),
            )
            .map_err(|source| StorageError::OpenLock { source })?;
            require_same_inode(
                inode_identity(&pinned).map_err(|source| StorageError::ValidateLock { source })?,
                inode_identity(&file).map_err(|source| StorageError::ValidateLock { source })?,
            )?;
            file
        }
        Err(source) => return Err(StorageError::OpenLock { source }),
    };
    require_safe_regular_file(&file, &path.join("state-transition.lock"))
        .map_err(|source| StorageError::ValidateLock { source })?;
    flock_exclusive(&file).map_err(|source| StorageError::AcquireLock { source })?;

    let named = openat2_file(
        directory.as_raw_fd(),
        LOCK_NAME,
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )
    .map_err(|source| StorageError::ValidateLock { source })?;
    require_safe_regular_file(&named, &path.join("state-transition.lock"))
        .map_err(|source| StorageError::ValidateLock { source })?;
    if inode_identity(&file).map_err(|source| StorageError::ValidateLock { source })?
        != inode_identity(&named).map_err(|source| StorageError::ValidateLock { source })?
    {
        return Err(StorageError::CanonicalChanged);
    }
    // Persist exact mode and structural name even when completing recovery
    // from a previously exposed restrictive-umask inode.
    file.sync_all().map_err(|source| StorageError::OpenLock { source })?;
    directory
        .sync_all()
        .map_err(|source| StorageError::SyncJournalDirectory { source })?;
    Ok(file)
}

fn recoverable_private_lock_mode(file: &std::fs::File, path: &Path) -> io::Result<u32> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.uid() != Uid::effective().as_raw()
        || metadata.nlink() != 1
        || mode & !JOURNAL_FILE_MODE != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "journal lock is not one safely recoverable owner-private regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ));
    }
    Ok(mode)
}

fn flock_exclusive(file: &std::fs::File) -> io::Result<()> {
    loop {
        // SAFETY: flock operates on the live lock-file descriptor.
        if unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

fn valid_temporary_name(name: &[u8]) -> bool {
    let Some(tail) = name.strip_prefix(TEMPORARY_PREFIX) else {
        return false;
    };
    tail.len() == 8 + 1 + 16
        && tail[8] == b'-'
        && tail[..8]
            .iter()
            .chain(&tail[9..])
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn directory_entries(directory: &std::fs::File) -> io::Result<Vec<CString>> {
    // Duplicate because fdopendir takes ownership and closedir closes its fd.
    // SAFETY: fcntl receives one live directory descriptor.
    let duplicate = unsafe { nix::libc::fcntl(directory.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fdopendir takes ownership of the fresh duplicate on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume the duplicate.
        unsafe { nix::libc::close(duplicate) };
        return Err(source);
    }

    let result = (|| {
        let mut entries = Vec::new();
        loop {
            // SAFETY: Linux exposes thread-local errno through this pointer.
            unsafe { *nix::libc::__errno_location() = 0 };
            // SAFETY: stream remains live and is used by this thread only.
            let entry = unsafe { nix::libc::readdir(stream) };
            if entry.is_null() {
                let source = io::Error::last_os_error();
                if source.raw_os_error() == Some(0) {
                    break;
                }
                return Err(source);
            }
            // SAFETY: d_name is NUL terminated for the returned live dirent.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            entries.push(name.to_owned());
            // Admit one entry beyond the valid maximum so cleanup can report
            // the specific stale-temporary cap even when both durable names
            // are present. Anything larger still fails before allocation can
            // grow without a bound.
            if entries.len() > MAX_STALE_TEMPORARIES + 3 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "journal directory entry bound exceeded",
                ));
            }
        }
        Ok(entries)
    })();
    // SAFETY: stream was returned by fdopendir and has not been closed.
    let close_result = unsafe { nix::libc::closedir(stream) };
    if close_result == -1 && result.is_ok() {
        return Err(io::Error::last_os_error());
    }
    result
}

fn read_bounded(file: &mut std::fs::File) -> io::Result<Vec<u8>> {
    let initial = file.metadata()?;
    require_safe_regular_file_metadata(&initial, Path::new("state-transition"), Uid::effective().as_raw())?;
    let initial_len = usize::try_from(initial.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "journal length does not fit usize"))?;
    if initial_len > MAX_CANONICAL_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("journal exceeds {MAX_CANONICAL_RECORD_BYTES} bytes"),
        ));
    }

    let mut bytes = Vec::with_capacity(initial_len.min(MAX_CANONICAL_RECORD_BYTES));
    file.take((MAX_CANONICAL_RECORD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CANONICAL_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("journal exceeds {MAX_CANONICAL_RECORD_BYTES} bytes"),
        ));
    }
    let final_metadata = file.metadata()?;
    require_safe_regular_file_metadata(
        &final_metadata,
        Path::new("state-transition"),
        Uid::effective().as_raw(),
    )?;
    if bytes.len() != initial_len || final_metadata.len() != initial.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "journal size changed while it was read",
        ));
    }
    Ok(bytes)
}

fn require_safe_regular_file(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    require_safe_regular_file_metadata(&metadata, path, Uid::effective().as_raw())
}

fn require_safe_stale_temporary(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.uid() != Uid::effective().as_raw()
        || metadata.nlink() != 1
        || mode & !JOURNAL_FILE_MODE != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "stale journal temporary is not one owner-controlled regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ));
    }
    Ok(())
}

fn require_safe_regular_file_metadata(
    metadata: &std::fs::Metadata,
    path: &Path,
    expected_owner: u32,
) -> io::Result<()> {
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.uid() != expected_owner
        || metadata.nlink() != 1
        || mode != JOURNAL_FILE_MODE
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "journal is not one owner-controlled regular 0600 file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ));
    }
    Ok(())
}

fn inode_identity(file: &std::fs::File) -> io::Result<InodeIdentity> {
    let metadata = file.metadata()?;
    Ok(InodeIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn require_same_inode(expected: InodeIdentity, actual: InodeIdentity) -> Result<(), StorageError> {
    if expected != actual {
        return Err(StorageError::CanonicalChanged);
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum DirectoryPolicy {
    Controlled,
    ExactPrivate,
}

fn ensure_journal_directory(cast: &std::fs::File, path: &Path) -> io::Result<std::fs::File> {
    loop {
        // SAFETY: the directory descriptor and fixed NUL-terminated name live
        // for the call. mkdirat never follows the final component.
        if unsafe { nix::libc::mkdirat(cast.as_raw_fd(), JOURNAL_DIRECTORY.as_ptr(), JOURNAL_DIRECTORY_MODE) } == 0 {
            break;
        }
        let source = io::Error::last_os_error();
        match source.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::AlreadyExists => break,
            _ => return Err(source),
        }
    }

    let pinned = openat2_file(
        cast.as_raw_fd(),
        JOURNAL_DIRECTORY,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    let mode = recoverable_private_directory_mode(&pinned, path)?;
    if mode != JOURNAL_DIRECTORY_MODE {
        chmod_path_descriptor(&pinned, JOURNAL_DIRECTORY_MODE)?;
    }
    require_directory(&pinned, path, DirectoryPolicy::ExactPrivate)?;

    let directory = openat2_file(
        cast.as_raw_fd(),
        JOURNAL_DIRECTORY,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_directory(&directory, path, DirectoryPolicy::ExactPrivate)?;
    require_same_directory(&pinned, &directory, path)?;
    // Always persist both the exact mode and the structural name. This also
    // completes a prior crash that exposed a safe umask-restricted inode
    // before its normalization was synced.
    directory.sync_all()?;
    cast.sync_all()?;
    Ok(directory)
}

fn recoverable_private_directory_mode(file: &std::fs::File, path: &Path) -> io::Result<u32> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != Uid::effective().as_raw()
        || mode & !JOURNAL_DIRECTORY_MODE != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "journal directory is not a safely recoverable owner-private directory: {} (uid={}, mode={mode:04o})",
                path.display(),
                metadata.uid()
            ),
        ));
    }
    Ok(mode)
}

fn open_existing_directory(
    parent: &std::fs::File,
    name: &CStr,
    path: &Path,
    policy: DirectoryPolicy,
) -> io::Result<std::fs::File> {
    let pinned = openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    require_directory(&pinned, path, policy)?;
    let directory = openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_directory(&directory, path, policy)?;
    require_same_directory(&pinned, &directory, path)?;
    Ok(directory)
}

fn require_directory(file: &std::fs::File, path: &Path, policy: DirectoryPolicy) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    let controlled = metadata.file_type().is_dir()
        && metadata.uid() == Uid::effective().as_raw()
        && mode & 0o7000 == 0
        && mode & 0o700 == 0o700
        && mode & 0o022 == 0;
    let exact = !matches!(policy, DirectoryPolicy::ExactPrivate) || mode == JOURNAL_DIRECTORY_MODE;
    if !controlled || !exact {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "journal parent is not an owner-controlled directory: {} (uid={}, mode={mode:04o})",
                path.display(),
                metadata.uid()
            ),
        ));
    }
    Ok(())
}

fn require_same_directory(first: &std::fs::File, second: &std::fs::File, path: &Path) -> io::Result<()> {
    let first = first.metadata()?;
    let second = second.metadata()?;
    if (first.dev(), first.ino()) != (second.dev(), second.ino()) {
        return Err(io::Error::other(format!(
            "journal directory changed while opening: {}",
            path.display()
        )));
    }
    Ok(())
}

fn open_directory_path(path: &Path) -> io::Result<std::fs::File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "journal root contains NUL"))?;
    openat2_file(
        nix::libc::AT_FDCWD,
        &path,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )
}

fn openat2_file(dirfd: RawFd, path: &CStr, flags: i32, mode: u32, resolve: u64) -> io::Result<std::fs::File> {
    // SAFETY: zero is valid for all public open_how fields.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    let descriptor = loop {
        // SAFETY: all pointers and the directory descriptor remain live. A
        // successful openat2 returns one fresh descriptor owned below.
        let descriptor = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_openat2,
                dirfd,
                path.as_ptr(),
                &how,
                size_of::<nix::libc::open_how>(),
            )
        };
        if descriptor != -1 {
            break descriptor;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    };
    let descriptor = i32::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned this fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(std::fs::File::from(descriptor))
}

fn controlled_resolution() -> u64 {
    (nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV) as u64
}

fn renameat2(old_dir: RawFd, old: &CStr, new_dir: RawFd, new: &CStr, flags: u32) -> io::Result<()> {
    // SAFETY: both directory descriptors and names remain valid for the call.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            old_dir,
            old.as_ptr(),
            new_dir,
            new.as_ptr(),
            flags,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn unlinkat(directory: RawFd, name: &CStr) -> io::Result<()> {
    // SAFETY: the descriptor and single-component name remain live; flags 0
    // unlinks a non-directory entry without following its final symlink.
    if unsafe { nix::libc::unlinkat(directory, name.as_ptr(), 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!("transition_journal/tests/mod.rs");
}
