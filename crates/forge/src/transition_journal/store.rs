use std::{
    ffi::{CStr, CString},
    io::{self, Write as _},
    os::{fd::AsRawFd as _, unix::fs::PermissionsExt as _},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use super::{
    CANONICAL_NAME, DirectoryPolicy, InodeIdentity, JOURNAL_FILE_MODE, MAX_STALE_TEMPORARIES, Phase, StorageError,
    TransitionRecord, controlled_resolution, decode, directory_entries, encode, ensure_journal_directory,
    inode_identity, open_and_lock, open_directory_path, open_existing_directory, openat2_file, read_bounded, renameat2,
    require_safe_regular_file, require_safe_stale_temporary, require_same_inode, temporary_name, try_open_and_lock,
    unlinkat, valid_temporary_name, validation::validate_advance,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DurabilityCheckpoint {
    TemporaryFullySynced,
    CanonicalPublished,
    CanonicalExchanged,
    CanonicalUnlinked,
    DisplacedUnlinked,
    JournalDirectorySynced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StorageFaultPoint {
    TemporarySync,
    InitialRename,
    InitialDirectorySync,
    UpdateExchange,
    UpdateFirstDirectorySync,
    DisplacedUnlink,
    UpdateFinalDirectorySync,
    CanonicalUnlink,
    DeleteDirectorySync,
}

#[cfg(test)]
std::thread_local! {
    static DURABILITY_CHECKPOINTS: std::cell::RefCell<Vec<DurabilityCheckpoint>> = const { std::cell::RefCell::new(Vec::new()) };
    static STORAGE_FAULT: std::cell::Cell<Option<StorageFaultPoint>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn durability_checkpoint(checkpoint: DurabilityCheckpoint) {
    DURABILITY_CHECKPOINTS.with(|checkpoints| checkpoints.borrow_mut().push(checkpoint));
}

#[cfg(not(test))]
fn durability_checkpoint(_checkpoint: DurabilityCheckpoint) {}

#[cfg(test)]
pub(super) fn take_durability_checkpoints() -> Vec<DurabilityCheckpoint> {
    DURABILITY_CHECKPOINTS.with(|checkpoints| std::mem::take(&mut *checkpoints.borrow_mut()))
}

#[cfg(test)]
pub(super) fn arm_storage_fault(point: StorageFaultPoint) {
    STORAGE_FAULT.with(|fault| {
        assert!(fault.replace(Some(point)).is_none(), "a storage fault is already armed");
    });
}

#[cfg(test)]
pub(super) fn assert_storage_fault_consumed() {
    STORAGE_FAULT.with(|fault| assert!(fault.get().is_none(), "armed storage fault was not reached"));
}

fn storage_fault(point: StorageFaultPoint) -> io::Result<()> {
    #[cfg(test)]
    {
        let injected = STORAGE_FAULT.with(|fault| fault.get() == Some(point));
        if injected {
            STORAGE_FAULT.with(|fault| fault.set(None));
            return Err(io::Error::other(format!(
                "injected transition-journal fault at {point:?}"
            )));
        }
    }
    let _ = point;
    Ok(())
}

#[derive(Debug)]
pub(crate) struct TransitionJournalStore {
    pub(super) directory: std::fs::File,
    _lock: std::fs::File,
    pub(super) operation_lock: Mutex<()>,
    path: PathBuf,
}

#[derive(Debug)]
struct LoadedRecord {
    record: TransitionRecord,
    _file: std::fs::File,
    identity: InodeIdentity,
}

#[derive(Debug)]
pub(super) struct TemporaryRecord {
    pub(super) name: CString,
    pub(super) file: std::fs::File,
    identity: InodeIdentity,
}

impl TransitionJournalStore {
    /// Open the owner-controlled `.cast/journal` directory, creating only its
    /// final fixed component when absent.
    pub(crate) fn open(root: &Path) -> Result<Self, StorageError> {
        let root_directory = open_directory_path(root).map_err(|source| StorageError::OpenRoot {
            path: root.to_owned(),
            source,
        })?;
        Self::open_from_root(&root_directory, root)
    }

    /// Open below an installation-root capability retained before the caller
    /// acquired its installation lock. No absolute pathname is reopened as
    /// authority for journal creation or locking.
    pub(crate) fn open_retained(root_directory: &std::fs::File, root: &Path) -> Result<Self, StorageError> {
        let root_directory =
            open_existing_directory(root_directory, c".", root, DirectoryPolicy::Controlled).map_err(|source| {
                StorageError::OpenRoot {
                    path: root.to_owned(),
                    source,
                }
            })?;
        Self::open_from_root(&root_directory, root)
    }

    /// Open below the exact `.cast` descriptor retained by a writable
    /// Installation. This constructor never resolves the public `.cast` name;
    /// the surrounding startup stage separately proves that the descriptor is
    /// still named before and after journal work.
    pub(crate) fn open_in_retained_cast(cast_directory: &std::fs::File, root: &Path) -> Result<Self, StorageError> {
        let cast_path = root.join(".cast");
        let cast = open_existing_directory(cast_directory, c".", &cast_path, DirectoryPolicy::Controlled).map_err(
            |source| StorageError::OpenCastDirectory {
                path: cast_path.clone(),
                source,
            },
        )?;
        Self::open_from_cast(&cast, cast_path, true)
    }

    /// Nonblocking pre-journal inspection under the canonical writer-first
    /// lock order.  A held journal lock is returned as `AcquireLock` rather
    /// than waiting behind an owner which may itself need the writer lease.
    pub(crate) fn try_open_in_retained_cast(cast_directory: &std::fs::File, root: &Path) -> Result<Self, StorageError> {
        let cast_path = root.join(".cast");
        let cast = open_existing_directory(cast_directory, c".", &cast_path, DirectoryPolicy::Controlled).map_err(
            |source| StorageError::OpenCastDirectory {
                path: cast_path.clone(),
                source,
            },
        )?;
        Self::open_from_cast(&cast, cast_path, false)
    }

    fn open_from_root(root_directory: &std::fs::File, root: &Path) -> Result<Self, StorageError> {
        let cast_path = root.join(".cast");
        let cast = open_existing_directory(root_directory, c".cast", &cast_path, DirectoryPolicy::Controlled).map_err(
            |source| StorageError::OpenCastDirectory {
                path: cast_path.clone(),
                source,
            },
        )?;
        Self::open_from_cast(&cast, cast_path, true)
    }

    fn open_from_cast(cast: &std::fs::File, cast_path: PathBuf, wait: bool) -> Result<Self, StorageError> {
        let path = cast_path.join("journal");
        let directory = ensure_journal_directory(cast, &path).map_err(|source| StorageError::OpenJournalDirectory {
            path: path.clone(),
            source,
        })?;
        let lock = if wait {
            open_and_lock(&directory, &path)?
        } else {
            try_open_and_lock(&directory, &path)?
        };
        let store = Self {
            directory,
            _lock: lock,
            operation_lock: Mutex::new(()),
            path,
        };
        store.cleanup_stale_temporaries()?;
        Ok(store)
    }

    /// Read only the canonical record. Temporary files are never recovery
    /// candidates, including when the canonical file is corrupt or absent.
    pub(crate) fn load(&self) -> Result<Option<TransitionRecord>, StorageError> {
        let _operation = self.lock_operation()?;
        Ok(self.load_pinned()?.map(|loaded| loaded.record))
    }

    /// Durably create the first `preparing` record for one transaction.
    pub(crate) fn create(&self, record: &TransitionRecord) -> Result<(), StorageError> {
        let _operation = self.lock_operation()?;
        let framed = encode(record).map_err(StorageError::Encode)?;
        if record.generation != 1 || record.phase != Phase::Preparing || record.rollback.is_some() {
            return Err(StorageError::InvalidCreationRecord);
        }
        if self.load_pinned()?.is_some() {
            return Err(StorageError::CanonicalAlreadyExists);
        }
        self.publish_record(&framed, None)
    }

    /// Conditionally advance the exact current record by one legal phase and
    /// one generation. A stale caller can never overwrite newer evidence.
    pub(crate) fn advance(&self, expected: &TransitionRecord, next: &TransitionRecord) -> Result<(), StorageError> {
        let _operation = self.lock_operation()?;
        validate_advance(expected, next).map_err(StorageError::InvalidAdvance)?;
        let framed = encode(next).map_err(StorageError::Encode)?;
        let Some(existing) = self.load_pinned()? else {
            return Err(StorageError::CanonicalMissing);
        };
        if existing.record != *expected {
            return Err(StorageError::ExpectedRecordMismatch);
        }
        self.publish_record(&framed, Some(existing))
    }

    fn publish_record(&self, framed: &[u8], existing: Option<LoadedRecord>) -> Result<(), StorageError> {
        let mut temporary = self.create_temporary()?;
        if let Err(source) = temporary.file.write_all(&framed) {
            self.cleanup_temporary(&temporary)?;
            return Err(StorageError::WriteTemporary { source });
        }
        // Full fsync persists both record contents and the exact 0600 mode set
        // after exclusive creation.
        if let Err(source) = storage_fault(StorageFaultPoint::TemporarySync).and_then(|()| temporary.file.sync_all()) {
            self.cleanup_temporary(&temporary)?;
            return Err(StorageError::SyncTemporary { source });
        }
        durability_checkpoint(DurabilityCheckpoint::TemporaryFullySynced);
        if let Err(source) = require_safe_regular_file(
            &temporary.file,
            &self.path.join(temporary.name.to_string_lossy().as_ref()),
        ) {
            self.cleanup_temporary(&temporary)?;
            return Err(StorageError::ValidateTemporary { source });
        }
        temporary.identity = match inode_identity(&temporary.file) {
            Ok(identity) => identity,
            Err(source) => {
                self.cleanup_temporary(&temporary)?;
                return Err(StorageError::ValidateTemporary { source });
            }
        };

        match existing {
            None => self.publish_initial(&temporary),
            Some(existing) => self.publish_update(&temporary, &existing),
        }
    }

    /// Remove the canonical record after validating both its storage metadata
    /// and payload. Returns false only when the canonical name is absent.
    pub(crate) fn delete(&self, expected: &TransitionRecord) -> Result<bool, StorageError> {
        let _operation = self.lock_operation()?;
        expected.validate().map_err(StorageError::Decode)?;
        if !expected.phase.deletable() {
            return Err(StorageError::DeleteNonterminal);
        }
        let Some(existing) = self.load_pinned()? else {
            return Ok(false);
        };
        if existing.record != *expected {
            return Err(StorageError::ExpectedRecordMismatch);
        }
        let named = self.open_named(CANONICAL_NAME)?.ok_or(StorageError::CanonicalChanged)?;
        require_same_inode(
            existing.identity,
            inode_identity(&named).map_err(|source| StorageError::ValidateCanonical { source })?,
        )?;
        storage_fault(StorageFaultPoint::CanonicalUnlink)
            .and_then(|()| unlinkat(self.directory.as_raw_fd(), CANONICAL_NAME))
            .map_err(|source| StorageError::DeleteCanonical { source })?;
        durability_checkpoint(DurabilityCheckpoint::CanonicalUnlinked);
        storage_fault(StorageFaultPoint::DeleteDirectorySync)
            .and_then(|()| self.directory.sync_all())
            .map_err(|source| StorageError::SyncJournalDirectory { source })?;
        durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);
        Ok(true)
    }

    fn lock_operation(&self) -> Result<MutexGuard<'_, ()>, StorageError> {
        self.operation_lock
            .lock()
            .map_err(|_| StorageError::OperationLockPoisoned)
    }

    fn load_pinned(&self) -> Result<Option<LoadedRecord>, StorageError> {
        let Some(mut file) = self.open_named(CANONICAL_NAME)? else {
            return Ok(None);
        };
        let identity = inode_identity(&file).map_err(|source| StorageError::ValidateCanonical { source })?;
        let framed = read_bounded(&mut file).map_err(|source| StorageError::ReadCanonical { source })?;
        require_safe_regular_file(&file, &self.path.join("state-transition"))
            .map_err(|source| StorageError::ValidateCanonical { source })?;
        let record = decode(&framed).map_err(StorageError::Decode)?;
        Ok(Some(LoadedRecord {
            record,
            _file: file,
            identity,
        }))
    }

    fn open_named(&self, name: &CStr) -> Result<Option<std::fs::File>, StorageError> {
        match openat2_file(
            self.directory.as_raw_fd(),
            name,
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            0,
            controlled_resolution(),
        ) {
            Ok(file) => {
                require_safe_regular_file(&file, &self.path.join(name.to_string_lossy().as_ref()))
                    .map_err(|source| StorageError::ValidateCanonical { source })?;
                Ok(Some(file))
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StorageError::OpenCanonical { source }),
        }
    }

    pub(super) fn create_temporary(&self) -> Result<TemporaryRecord, StorageError> {
        const MAX_ATTEMPTS: usize = 128;
        for _ in 0..MAX_ATTEMPTS {
            let name = temporary_name();
            match openat2_file(
                self.directory.as_raw_fd(),
                &name,
                nix::libc::O_WRONLY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK
                    | nix::libc::O_CREAT
                    | nix::libc::O_EXCL,
                JOURNAL_FILE_MODE,
                controlled_resolution(),
            ) {
                Ok(file) => {
                    let identity = match inode_identity(&file) {
                        Ok(identity) => identity,
                        Err(source) => {
                            unlinkat(self.directory.as_raw_fd(), &name).map_err(|cleanup| {
                                StorageError::CleanupTemporary {
                                    name: name.to_string_lossy().into_owned(),
                                    source: cleanup,
                                }
                            })?;
                            self.directory
                                .sync_all()
                                .map_err(|source| StorageError::SyncJournalDirectory { source })?;
                            return Err(StorageError::ValidateTemporary { source });
                        }
                    };
                    if let Err(source) = file.set_permissions(std::fs::Permissions::from_mode(JOURNAL_FILE_MODE)) {
                        self.cleanup_temporary_identity(&name, identity)?;
                        return Err(StorageError::CreateTemporary { source });
                    }
                    if let Err(source) =
                        require_safe_regular_file(&file, &self.path.join(name.to_string_lossy().as_ref()))
                    {
                        self.cleanup_temporary_identity(&name, identity)?;
                        return Err(StorageError::ValidateTemporary { source });
                    }
                    return Ok(TemporaryRecord { name, file, identity });
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(StorageError::CreateTemporary { source }),
            }
        }
        Err(StorageError::TemporaryNamesExhausted)
    }

    fn publish_initial(&self, temporary: &TemporaryRecord) -> Result<(), StorageError> {
        if let Err(source) = storage_fault(StorageFaultPoint::InitialRename).and_then(|()| {
            renameat2(
                self.directory.as_raw_fd(),
                &temporary.name,
                self.directory.as_raw_fd(),
                CANONICAL_NAME,
                nix::libc::RENAME_NOREPLACE,
            )
        }) {
            self.cleanup_temporary(temporary)?;
            return Err(StorageError::PublishCanonical { source });
        }
        durability_checkpoint(DurabilityCheckpoint::CanonicalPublished);
        let canonical = self.open_named(CANONICAL_NAME)?.ok_or(StorageError::CanonicalChanged)?;
        require_same_inode(
            temporary.identity,
            inode_identity(&canonical).map_err(|source| StorageError::ValidateCanonical { source })?,
        )?;
        storage_fault(StorageFaultPoint::InitialDirectorySync)
            .and_then(|()| self.directory.sync_all())
            .map_err(|source| StorageError::SyncJournalDirectory { source })?;
        durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);
        Ok(())
    }

    fn publish_update(&self, temporary: &TemporaryRecord, existing: &LoadedRecord) -> Result<(), StorageError> {
        // Reauthenticate the fixed canonical name immediately before the
        // exchange. The retained descriptor keeps the decoded inode pinned.
        let named = self.open_named(CANONICAL_NAME)?.ok_or(StorageError::CanonicalChanged)?;
        require_same_inode(
            existing.identity,
            inode_identity(&named).map_err(|source| StorageError::ValidateCanonical { source })?,
        )?;

        if let Err(source) = storage_fault(StorageFaultPoint::UpdateExchange).and_then(|()| {
            renameat2(
                self.directory.as_raw_fd(),
                &temporary.name,
                self.directory.as_raw_fd(),
                CANONICAL_NAME,
                nix::libc::RENAME_EXCHANGE,
            )
        }) {
            self.cleanup_temporary(temporary)?;
            return Err(StorageError::PublishCanonical { source });
        }
        durability_checkpoint(DurabilityCheckpoint::CanonicalExchanged);

        // After exchange, the canonical name must identify the fdatasynced
        // inode and the temporary name must identify the old decoded inode.
        // If either proof fails, preserve both names for diagnosis.
        let canonical = self.open_named(CANONICAL_NAME)?.ok_or(StorageError::CanonicalChanged)?;
        require_same_inode(
            temporary.identity,
            inode_identity(&canonical).map_err(|source| StorageError::ValidateCanonical { source })?,
        )?;
        let displaced = self
            .open_named(&temporary.name)?
            .ok_or(StorageError::CanonicalChanged)?;
        require_same_inode(
            existing.identity,
            inode_identity(&displaced).map_err(|source| StorageError::ValidateCanonical { source })?,
        )?;

        storage_fault(StorageFaultPoint::UpdateFirstDirectorySync)
            .and_then(|()| self.directory.sync_all())
            .map_err(|source| StorageError::SyncJournalDirectory { source })?;
        durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);
        storage_fault(StorageFaultPoint::DisplacedUnlink)
            .and_then(|()| unlinkat(self.directory.as_raw_fd(), &temporary.name))
            .map_err(|source| StorageError::DeleteDisplaced { source })?;
        durability_checkpoint(DurabilityCheckpoint::DisplacedUnlinked);
        storage_fault(StorageFaultPoint::UpdateFinalDirectorySync)
            .and_then(|()| self.directory.sync_all())
            .map_err(|source| StorageError::SyncJournalDirectory { source })?;
        durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);
        Ok(())
    }

    pub(super) fn cleanup_temporary(&self, temporary: &TemporaryRecord) -> Result<(), StorageError> {
        self.cleanup_temporary_identity(&temporary.name, temporary.identity)
    }

    fn cleanup_temporary_identity(&self, name: &CStr, expected: InodeIdentity) -> Result<(), StorageError> {
        let display = name.to_string_lossy().into_owned();
        let named = openat2_file(
            self.directory.as_raw_fd(),
            name,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        )
        .map_err(|source| StorageError::CleanupTemporary {
            name: display.clone(),
            source,
        })?;
        let actual = inode_identity(&named).map_err(|source| StorageError::CleanupTemporary {
            name: display.clone(),
            source,
        })?;
        if actual != expected {
            return Err(StorageError::CanonicalChanged);
        }
        unlinkat(self.directory.as_raw_fd(), name)
            .map_err(|source| StorageError::CleanupTemporary { name: display, source })?;
        self.directory
            .sync_all()
            .map_err(|source| StorageError::SyncJournalDirectory { source })
    }

    fn cleanup_stale_temporaries(&self) -> Result<(), StorageError> {
        let entries = directory_entries(&self.directory).map_err(|source| StorageError::EnumerateJournal { source })?;
        let mut stale = Vec::new();
        for name in entries {
            match name.to_bytes() {
                b"state-transition" | b"state-transition.lock" => {}
                bytes if valid_temporary_name(bytes) => {
                    stale.push(name);
                    if stale.len() > MAX_STALE_TEMPORARIES {
                        return Err(StorageError::TooManyStaleTemporaries);
                    }
                }
                _ => {
                    return Err(StorageError::UnexpectedJournalEntry(
                        name.to_string_lossy().into_owned(),
                    ));
                }
            }
        }
        let mut authenticated = Vec::with_capacity(stale.len());
        for name in stale {
            let display = name.to_string_lossy().into_owned();
            let file = openat2_file(
                self.directory.as_raw_fd(),
                &name,
                nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                0,
                controlled_resolution(),
            )
            .map_err(|source| StorageError::ValidateStaleTemporary {
                name: display.clone(),
                source,
            })?;
            require_safe_stale_temporary(&file, &self.path.join(&display)).map_err(|source| {
                StorageError::ValidateStaleTemporary {
                    name: display.clone(),
                    source,
                }
            })?;
            let identity = inode_identity(&file)
                .map_err(|source| StorageError::ValidateStaleTemporary { name: display, source })?;
            authenticated.push((name, identity));
        }
        for (name, identity) in authenticated {
            self.cleanup_temporary_identity(&name, identity)?;
        }
        Ok(())
    }
}
