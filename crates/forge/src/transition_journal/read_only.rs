//! Existing-only proof that no state transition requires recovery.

use std::{
    ffi::CStr,
    io,
    os::fd::AsRawFd as _,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::{Installation, installation, state::TransitionId};

use super::{
    CANONICAL_NAME, CodecError, DirectoryPolicy, JOURNAL_DIRECTORY, LOCK_NAME, TEMPORARY_PREFIX, controlled_resolution,
    decode, directory_entries, inode_identity, openat2_file, read_bounded, require_directory,
    require_safe_regular_file, require_safe_stale_temporary, require_same_directory, valid_temporary_name,
};

#[cfg(test)]
mod tests;

/// Retained evidence that the transition journal was clean when a read-only
/// client opened. No directory, lockfile, temporary, or canonical record is
/// ever created, repaired, normalized, synced, or removed by this boundary.
#[derive(Debug)]
pub(crate) struct CleanReadOnlyJournal {
    cast: std::fs::File,
    state: JournalState,
    cast_path: PathBuf,
    journal_path: PathBuf,
}

#[derive(Debug)]
enum JournalState {
    Absent,
    Present {
        directory: std::fs::File,
        lock: std::fs::File,
        lock_identity: super::InodeIdentity,
    },
}

impl CleanReadOnlyJournal {
    /// Inspect the journal below the retained installation-root capability.
    /// An absent journal directory is clean only while it remains absent. A
    /// present directory must contain exactly its pre-existing safe lockfile.
    pub(crate) fn inspect(installation: &Installation) -> Result<Self, ReadOnlyJournalError> {
        installation.revalidate_read_only_snapshot()?;
        let cast_path = installation.root.join(".cast");
        let journal_path = cast_path.join("journal");
        let cast = open_existing_read_only_directory(
            installation.root_directory(),
            c".cast",
            &cast_path,
            DirectoryPolicy::Controlled,
        )
        .map_err(|source| ReadOnlyJournalError::OpenCast {
            path: cast_path.clone(),
            source,
        })?;

        let state = match open_optional_journal(&cast, &journal_path)? {
            None => JournalState::Absent,
            Some(directory) => {
                require_exact_clean_entries(&directory, &journal_path)?;
                let (lock, lock_identity) = open_shared_lock(&directory, &journal_path)?;
                require_exact_clean_entries(&directory, &journal_path)?;
                JournalState::Present {
                    directory,
                    lock,
                    lock_identity,
                }
            }
        };
        let journal = Self {
            cast,
            state,
            cast_path,
            journal_path,
        };
        journal.revalidate(installation)?;
        Ok(journal)
    }

    /// Revalidate the exact clean observation around every public query.
    pub(crate) fn revalidate(&self, installation: &Installation) -> Result<(), ReadOnlyJournalError> {
        installation.revalidate_read_only_snapshot()?;
        let named_cast = open_existing_read_only_directory(
            installation.root_directory(),
            c".cast",
            &self.cast_path,
            DirectoryPolicy::Controlled,
        )
        .map_err(|source| ReadOnlyJournalError::OpenCast {
            path: self.cast_path.clone(),
            source,
        })?;
        require_same_directory(&self.cast, &named_cast, &self.cast_path).map_err(|source| {
            ReadOnlyJournalError::JournalChanged {
                path: self.cast_path.clone(),
                source,
            }
        })?;

        match &self.state {
            JournalState::Absent => {
                if open_optional_journal(&self.cast, &self.journal_path)?.is_some() {
                    return Err(ReadOnlyJournalError::JournalAppeared {
                        path: self.journal_path.clone(),
                    });
                }
            }
            JournalState::Present {
                directory,
                lock,
                lock_identity,
            } => {
                let named = open_optional_journal(&self.cast, &self.journal_path)?.ok_or_else(|| {
                    ReadOnlyJournalError::JournalDisappeared {
                        path: self.journal_path.clone(),
                    }
                })?;
                require_same_directory(directory, &named, &self.journal_path).map_err(|source| {
                    ReadOnlyJournalError::JournalChanged {
                        path: self.journal_path.clone(),
                        source,
                    }
                })?;
                require_safe_regular_file(lock, &self.journal_path.join("state-transition.lock"))
                    .map_err(|source| ReadOnlyJournalError::ValidateLock { source })?;
                require_named_lock(directory, *lock_identity)?;
                require_exact_clean_entries(directory, &self.journal_path)?;
            }
        }
        installation.revalidate_read_only_snapshot()?;
        Ok(())
    }
}

fn open_optional_journal(cast: &std::fs::File, path: &Path) -> Result<Option<std::fs::File>, ReadOnlyJournalError> {
    match open_existing_read_only_directory(cast, JOURNAL_DIRECTORY, path, DirectoryPolicy::ExactPrivate) {
        Ok(directory) => Ok(Some(directory)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ReadOnlyJournalError::OpenJournal {
            path: path.to_owned(),
            source,
        }),
    }
}

fn open_existing_read_only_directory(
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
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
    )?;
    require_directory(&directory, path, policy)?;
    require_same_directory(&pinned, &directory, path)?;
    Ok(directory)
}

fn require_exact_clean_entries(directory: &std::fs::File, path: &Path) -> Result<(), ReadOnlyJournalError> {
    // A duplicated directory descriptor shares its readdir offset. Reopen `.`
    // as a fresh file description so every bounded proof scans from the start
    // without mutating or trusting an ambient pathname.
    let scan = open_existing_read_only_directory(directory, c".", path, DirectoryPolicy::ExactPrivate)
        .map_err(|source| ReadOnlyJournalError::Enumerate { source })?;
    let mut entries = directory_entries(&scan).map_err(|source| ReadOnlyJournalError::Enumerate { source })?;
    entries.sort_by(|left, right| left.to_bytes().cmp(right.to_bytes()));
    for entry in &entries {
        match entry.to_bytes() {
            b"state-transition.lock" => {}
            b"state-transition" => {
                let transition = load_transition(directory, path)?;
                return Err(ReadOnlyJournalError::UnresolvedTransition {
                    transition: transition.transition_id,
                });
            }
            name if valid_temporary_name(name) => {
                authenticate_temporary(directory, path, entry)?;
                return Err(ReadOnlyJournalError::InterruptedTemporary {
                    name: entry.to_string_lossy().into_owned(),
                });
            }
            name if name.starts_with(TEMPORARY_PREFIX) => {
                return Err(ReadOnlyJournalError::MalformedTemporary {
                    name: entry.to_string_lossy().into_owned(),
                });
            }
            _ => {
                return Err(ReadOnlyJournalError::UnexpectedEntry(
                    entry.to_string_lossy().into_owned(),
                ));
            }
        }
    }
    if entries.len() != 1 || entries[0].as_c_str() != LOCK_NAME {
        return Err(ReadOnlyJournalError::MissingLock);
    }
    Ok(())
}

fn load_transition(directory: &std::fs::File, path: &Path) -> Result<super::TransitionRecord, ReadOnlyJournalError> {
    let canonical_path = path.join("state-transition");
    let mut file = openat2_file(
        directory.as_raw_fd(),
        CANONICAL_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
    )
    .map_err(|source| ReadOnlyJournalError::OpenCanonical { source })?;
    require_safe_regular_file(&file, &canonical_path)
        .map_err(|source| ReadOnlyJournalError::ValidateCanonical { source })?;
    let identity = inode_identity(&file).map_err(|source| ReadOnlyJournalError::ValidateCanonical { source })?;
    let framed = read_bounded(&mut file).map_err(|source| ReadOnlyJournalError::ReadCanonical { source })?;
    require_safe_regular_file(&file, &canonical_path)
        .map_err(|source| ReadOnlyJournalError::ValidateCanonical { source })?;

    let named = openat2_file(
        directory.as_raw_fd(),
        CANONICAL_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
    )
    .map_err(|source| ReadOnlyJournalError::OpenCanonical { source })?;
    require_safe_regular_file(&named, &canonical_path)
        .map_err(|source| ReadOnlyJournalError::ValidateCanonical { source })?;
    if inode_identity(&named).map_err(|source| ReadOnlyJournalError::ValidateCanonical { source })? != identity {
        return Err(ReadOnlyJournalError::CanonicalChanged);
    }
    decode(&framed).map_err(ReadOnlyJournalError::Decode)
}

fn authenticate_temporary(directory: &std::fs::File, path: &Path, name: &CStr) -> Result<(), ReadOnlyJournalError> {
    let display = name.to_string_lossy().into_owned();
    let file = openat2_file(
        directory.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .map_err(|source| ReadOnlyJournalError::ValidateTemporary {
        name: display.clone(),
        source,
    })?;
    require_safe_stale_temporary(&file, &path.join(&display))
        .map_err(|source| ReadOnlyJournalError::ValidateTemporary { name: display, source })
}

fn open_shared_lock(
    directory: &std::fs::File,
    path: &Path,
) -> Result<(std::fs::File, super::InodeIdentity), ReadOnlyJournalError> {
    let lock = openat2_file(
        directory.as_raw_fd(),
        LOCK_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
    )
    .map_err(|source| ReadOnlyJournalError::OpenLock { source })?;
    require_safe_regular_file(&lock, &path.join("state-transition.lock"))
        .map_err(|source| ReadOnlyJournalError::ValidateLock { source })?;
    let identity = inode_identity(&lock).map_err(|source| ReadOnlyJournalError::ValidateLock { source })?;
    flock_shared_nonblocking(&lock).map_err(|source| ReadOnlyJournalError::AcquireSharedLock { source })?;
    require_named_lock(directory, identity)?;
    Ok((lock, identity))
}

fn require_named_lock(directory: &std::fs::File, expected: super::InodeIdentity) -> Result<(), ReadOnlyJournalError> {
    let named = openat2_file(
        directory.as_raw_fd(),
        LOCK_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
    )
    .map_err(|source| ReadOnlyJournalError::OpenLock { source })?;
    require_safe_regular_file(&named, Path::new("state-transition.lock"))
        .map_err(|source| ReadOnlyJournalError::ValidateLock { source })?;
    let actual = inode_identity(&named).map_err(|source| ReadOnlyJournalError::ValidateLock { source })?;
    if actual != expected {
        return Err(ReadOnlyJournalError::LockChanged);
    }
    Ok(())
}

fn flock_shared_nonblocking(file: &std::fs::File) -> io::Result<()> {
    loop {
        // SAFETY: flock operates on the retained live lock-file descriptor.
        if unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_SH | nix::libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum ReadOnlyJournalError {
    #[error("revalidate explicit read-only installation snapshot")]
    Installation(#[from] installation::Error),
    #[error("open existing Cast directory `{}`", path.display())]
    OpenCast {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open existing transition-journal directory `{}`", path.display())]
    OpenJournal {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("transition-journal directory appeared during the read-only snapshot: {}", path.display())]
    JournalAppeared { path: PathBuf },
    #[error("transition-journal directory disappeared during the read-only snapshot: {}", path.display())]
    JournalDisappeared { path: PathBuf },
    #[error("retained transition-journal namespace changed: {}", path.display())]
    JournalChanged {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("enumerate bounded transition-journal entries")]
    Enumerate {
        #[source]
        source: io::Error,
    },
    #[error("pre-existing transition-journal lockfile is missing")]
    MissingLock,
    #[error("open pre-existing transition-journal lockfile")]
    OpenLock {
        #[source]
        source: io::Error,
    },
    #[error("validate pre-existing transition-journal lockfile")]
    ValidateLock {
        #[source]
        source: io::Error,
    },
    #[error("acquire nonblocking shared transition-journal lock")]
    AcquireSharedLock {
        #[source]
        source: io::Error,
    },
    #[error("pre-existing transition-journal lockfile changed")]
    LockChanged,
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
    #[error("canonical state-transition journal changed while reading")]
    CanonicalChanged,
    #[error("decode canonical state-transition journal")]
    Decode(#[source] CodecError),
    #[error("state transition `{transition}` requires recovery before read-only queries")]
    UnresolvedTransition { transition: TransitionId },
    #[error("interrupted transition-journal temporary `{name}` requires recovery")]
    InterruptedTemporary { name: String },
    #[error("malformed transition-journal temporary `{name}` requires manual recovery")]
    MalformedTemporary { name: String },
    #[error("validate interrupted transition-journal temporary `{name}`")]
    ValidateTemporary {
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("transition journal contains unexpected entry `{0}`")]
    UnexpectedEntry(String),
}
