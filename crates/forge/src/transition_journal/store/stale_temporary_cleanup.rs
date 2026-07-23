use std::{ffi::CStr, os::fd::AsRawFd as _};

use super::{TemporaryRecord, TransitionJournalStore};
use super::super::{
    InodeIdentity, MAX_STALE_TEMPORARIES, StorageError, controlled_resolution, directory_entries,
    inode_identity, openat2_file, require_safe_stale_temporary, unlinkat, valid_temporary_name,
};

impl TransitionJournalStore {
    pub(in crate::transition_journal) fn cleanup_temporary(
        &self,
        temporary: &TemporaryRecord,
    ) -> Result<(), StorageError> {
        self.cleanup_temporary_identity(&temporary.name, temporary.identity)
    }

    pub(super) fn cleanup_temporary_identity(
        &self,
        name: &CStr,
        expected: InodeIdentity,
    ) -> Result<(), StorageError> {
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

    pub(super) fn cleanup_stale_temporaries(&self) -> Result<(), StorageError> {
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
