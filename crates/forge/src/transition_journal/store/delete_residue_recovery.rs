use std::{ffi::{CStr, CString}, os::fd::AsRawFd as _};

use super::{StorageFaultPoint, TransitionJournalStore, storage_fault};
use super::super::{
    CANONICAL_NAME, DELETE_PREFIX, DirectoryPolicy, InodeIdentity, LOCK_NAME,
    StorageError, TransitionRecord, controlled_resolution, decode, directory_entries, inode_identity,
    openat2_file, read_bounded, renameat2, require_directory, require_safe_regular_file,
    require_same_directory, valid_delete_name,
};

#[derive(Debug)]
struct RetainedDeleteResidue {
    name: CString,
    display: String,
    identity: InodeIdentity,
    framed: Vec<u8>,
    _record: TransitionRecord,
    _file: std::fs::File,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeleteResidueLayout {
    ExactResidue,
    ExactCanonical,
}

/// Test-only namespace boundaries around exact interrupted-delete recovery.
/// Production has no callback storage or dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeleteResidueRecoveryRevalidationBoundary {
    BetweenLayoutObservations,
    BeforeRestoreFinalBinding,
    BeforeRestoredCanonicalSync,
    BeforeFinalCanonicalBinding,
}

/// Completed filesystem operations in interrupted-delete recovery. These
/// seams let crash tests stop after the rename or its durability sync without
/// adding production control flow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeleteResidueRecoveryDurabilityBoundary {
    CanonicalRestored,
    JournalDirectorySynced,
}

#[cfg(test)]
std::thread_local! {
    static REVALIDATION_CALLBACK: std::cell::RefCell<Option<(
        DeleteResidueRecoveryRevalidationBoundary,
        Box<dyn FnOnce()>,
    )>> = const { std::cell::RefCell::new(None) };
    static DURABILITY_CALLBACK: std::cell::RefCell<Option<(
        DeleteResidueRecoveryDurabilityBoundary,
        Box<dyn FnOnce()>,
    )>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_delete_residue_recovery_revalidation_callback(
    boundary: DeleteResidueRecoveryRevalidationBoundary,
    callback: impl FnOnce() + 'static,
) {
    REVALIDATION_CALLBACK.with(|armed| {
        assert!(
            armed.borrow_mut().replace((boundary, Box::new(callback))).is_none(),
            "a delete-residue recovery revalidation callback is already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn assert_delete_residue_recovery_revalidation_callback_consumed() {
    REVALIDATION_CALLBACK.with(|armed| {
        assert!(
            armed.borrow().is_none(),
            "armed delete-residue recovery revalidation callback was not reached"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_delete_residue_recovery_durability_callback(
    boundary: DeleteResidueRecoveryDurabilityBoundary,
    callback: impl FnOnce() + 'static,
) {
    DURABILITY_CALLBACK.with(|armed| {
        assert!(
            armed.borrow_mut().replace((boundary, Box::new(callback))).is_none(),
            "a delete-residue recovery durability callback is already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn assert_delete_residue_recovery_durability_callback_consumed() {
    DURABILITY_CALLBACK.with(|armed| {
        assert!(
            armed.borrow().is_none(),
            "armed delete-residue recovery durability callback was not reached"
        );
    });
}

fn revalidation_boundary(boundary: DeleteResidueRecoveryRevalidationBoundary) {
    #[cfg(test)]
    {
        let callback = REVALIDATION_CALLBACK.with(|armed| {
            let mut armed = armed.borrow_mut();
            if armed.as_ref().is_some_and(|(target, _)| *target == boundary) {
                armed.take().map(|(_, callback)| callback)
            } else {
                None
            }
        });
        if let Some(callback) = callback {
            callback();
        }
    }
    let _ = boundary;
}

fn durability_boundary(boundary: DeleteResidueRecoveryDurabilityBoundary) {
    #[cfg(test)]
    {
        let callback = DURABILITY_CALLBACK.with(|armed| {
            let mut armed = armed.borrow_mut();
            if armed.as_ref().is_some_and(|(target, _)| *target == boundary) {
                armed.take().map(|(_, callback)| callback)
            } else {
                None
            }
        });
        if let Some(callback) = callback {
            callback();
        }
    }
    let _ = boundary;
}

impl TransitionJournalStore {
    /// Restore one exact terminal record left privately named by a process
    /// loss after bound deletion detached the canonical name.
    ///
    /// The generated residue name is collision detecting, not secret. The
    /// owner-private journal and retained exclusive lock establish a
    /// cooperative same-credential boundary. An uncooperative same-credential
    /// writer changing a name inside the final compare/rename syscall window
    /// is outside that boundary. No optional work occurs in that window.
    pub(super) fn recover_interrupted_bound_delete(
        &self,
        cast_directory: &std::fs::File,
    ) -> Result<(), StorageError> {
        let entries = self.delete_recovery_entries(cast_directory)?;
        if !entries
            .iter()
            .any(|name| name.as_bytes().starts_with(DELETE_PREFIX))
        {
            return Ok(());
        }
        let residue_name = require_unique_delete_residue_inventory(entries)?;
        let retained = self.retain_delete_residue(residue_name)?;
        self.require_delete_residue_layout(cast_directory, &retained, DeleteResidueLayout::ExactResidue)?;

        // The injected pre-rename failure precedes the final binding proof so
        // the production compare/rename window below remains work-free.
        storage_fault(StorageFaultPoint::DeleteResidueRestore).map_err(|source| {
            StorageError::RestoreDeleteResidue {
                name: retained.display.clone(),
                restored: false,
                source,
            }
        })?;
        revalidation_boundary(DeleteResidueRecoveryRevalidationBoundary::BeforeRestoreFinalBinding);
        self.require_delete_residue_layout(cast_directory, &retained, DeleteResidueLayout::ExactResidue)?;
        let restore = renameat2(
            self.directory.as_raw_fd(),
            &retained.name,
            self.directory.as_raw_fd(),
            CANONICAL_NAME,
            nix::libc::RENAME_NOREPLACE,
        );
        let restore_applied = restore.is_ok();
        if restore_applied {
            durability_boundary(DeleteResidueRecoveryDurabilityBoundary::CanonicalRestored);
        }
        let reported = restore.and_then(|()| storage_fault(StorageFaultPoint::DeleteResidueRestoreReport));
        let pending_restore_error = match reported {
            Ok(()) => None,
            Err(source) => match self.classify_delete_residue_layout(cast_directory, &retained) {
                Ok(DeleteResidueLayout::ExactResidue) => {
                    return Err(StorageError::RestoreDeleteResidue {
                        name: retained.display,
                        restored: false,
                        source,
                    });
                }
                Ok(DeleteResidueLayout::ExactCanonical) => Some(source),
                Err(reconciliation) => {
                    return Err(StorageError::RestoreDeleteResidueAndReconciliation {
                        name: retained.display,
                        restore: source,
                        reconciliation: Box::new(reconciliation),
                    });
                }
            },
        };

        if let Err(source) = storage_fault(StorageFaultPoint::DeleteResidueDirectorySync) {
            return Err(self.reconcile_delete_residue_sync_error(cast_directory, &retained, source));
        }
        revalidation_boundary(DeleteResidueRecoveryRevalidationBoundary::BeforeRestoredCanonicalSync);
        self.require_delete_residue_layout(cast_directory, &retained, DeleteResidueLayout::ExactCanonical)?;
        let directory_sync = self.directory.sync_all();
        if directory_sync.is_ok() {
            durability_boundary(DeleteResidueRecoveryDurabilityBoundary::JournalDirectorySynced);
        }
        let directory_sync = directory_sync
            .and_then(|()| storage_fault(StorageFaultPoint::DeleteResidueDirectorySyncReport));
        if let Err(source) = directory_sync {
            return Err(self.reconcile_delete_residue_sync_error(cast_directory, &retained, source));
        }
        revalidation_boundary(DeleteResidueRecoveryRevalidationBoundary::BeforeFinalCanonicalBinding);
        self.require_delete_residue_layout(cast_directory, &retained, DeleteResidueLayout::ExactCanonical)?;

        match pending_restore_error {
            Some(source) => Err(StorageError::RestoreDeleteResidue {
                name: retained.display,
                restored: true,
                source,
            }),
            None => Ok(()),
        }
    }

    fn retain_delete_residue(&self, name: CString) -> Result<RetainedDeleteResidue, StorageError> {
        let display = name.to_string_lossy().into_owned();
        let mut file = self.open_delete_recovery_record(&name, &display)?;
        let identity = inode_identity(&file).map_err(|source| StorageError::ValidateDeleteResidue {
            name: display.clone(),
            source,
        })?;
        let framed = read_bounded(&mut file).map_err(|source| StorageError::ReadDeleteResidue {
            name: display.clone(),
            source,
        })?;
        require_safe_regular_file(&file, &self.path.join(&display)).map_err(|source| {
            StorageError::ValidateDeleteResidue {
                name: display.clone(),
                source,
            }
        })?;
        if inode_identity(&file).map_err(|source| StorageError::ValidateDeleteResidue {
            name: display.clone(),
            source,
        })? != identity
        {
            return Err(StorageError::DeleteResidueChanged { name: display });
        }
        let record = decode(&framed).map_err(|source| StorageError::DecodeDeleteResidue {
            name: display.clone(),
            source,
        })?;
        if !record.phase.deletable() {
            return Err(StorageError::NonterminalDeleteResidue { name: display });
        }
        Ok(RetainedDeleteResidue {
            name,
            display,
            identity,
            framed,
            _record: record,
            _file: file,
        })
    }

    fn require_delete_residue_layout(
        &self,
        cast_directory: &std::fs::File,
        retained: &RetainedDeleteResidue,
        expected: DeleteResidueLayout,
    ) -> Result<(), StorageError> {
        let actual = self.classify_delete_residue_layout(cast_directory, retained)?;
        if actual != expected {
            return Err(StorageError::DeleteResidueChanged {
                name: retained.display.clone(),
            });
        }
        Ok(())
    }

    fn reconcile_delete_residue_sync_error(
        &self,
        cast_directory: &std::fs::File,
        retained: &RetainedDeleteResidue,
        source: std::io::Error,
    ) -> StorageError {
        match self.classify_delete_residue_layout(cast_directory, retained) {
            Ok(DeleteResidueLayout::ExactCanonical) => StorageError::SyncJournalDirectory { source },
            Ok(DeleteResidueLayout::ExactResidue) => StorageError::SyncDeleteResidueAndReconciliation {
                name: retained.display.clone(),
                sync: source,
                reconciliation: Box::new(StorageError::DeleteResidueChanged {
                    name: retained.display.clone(),
                }),
            },
            Err(reconciliation) => StorageError::SyncDeleteResidueAndReconciliation {
                name: retained.display.clone(),
                sync: source,
                reconciliation: Box::new(reconciliation),
            },
        }
    }

    fn classify_delete_residue_layout(
        &self,
        cast_directory: &std::fs::File,
        retained: &RetainedDeleteResidue,
    ) -> Result<DeleteResidueLayout, StorageError> {
        let first = self.observe_delete_residue_layout(cast_directory, retained)?;
        revalidation_boundary(DeleteResidueRecoveryRevalidationBoundary::BetweenLayoutObservations);
        let second = self.observe_delete_residue_layout(cast_directory, retained)?;
        if first != second {
            return Err(StorageError::DeleteResidueChanged {
                name: retained.display.clone(),
            });
        }
        Ok(second)
    }

    fn observe_delete_residue_layout(
        &self,
        cast_directory: &std::fs::File,
        retained: &RetainedDeleteResidue,
    ) -> Result<DeleteResidueLayout, StorageError> {
        let entries = self.delete_recovery_entries(cast_directory)?;
        let lock_count = entries
            .iter()
            .filter(|name| name.as_bytes() == LOCK_NAME.to_bytes())
            .count();
        let residue_count = entries
            .iter()
            .filter(|name| name.as_bytes() == retained.name.as_bytes())
            .count();
        let canonical_count = entries
            .iter()
            .filter(|name| name.as_bytes() == CANONICAL_NAME.to_bytes())
            .count();
        if lock_count != 1
            || residue_count + canonical_count != 1
            || entries.len() != 2
            || residue_count > 1
            || canonical_count > 1
        {
            return Err(delete_residue_entry_set_error(entries));
        }
        let (name, display, layout) = if residue_count == 1 {
            (retained.name.as_c_str(), retained.display.as_str(), DeleteResidueLayout::ExactResidue)
        } else {
            (CANONICAL_NAME, "state-transition", DeleteResidueLayout::ExactCanonical)
        };
        let mut named = self.open_delete_recovery_record(name, display)?;
        let identity = inode_identity(&named).map_err(|source| StorageError::ValidateDeleteResidue {
            name: display.to_owned(),
            source,
        })?;
        let framed = read_bounded(&mut named).map_err(|source| StorageError::ReadDeleteResidue {
            name: display.to_owned(),
            source,
        })?;
        require_safe_regular_file(&named, &self.path.join(display)).map_err(|source| {
            StorageError::ValidateDeleteResidue {
                name: display.to_owned(),
                source,
            }
        })?;
        if identity != retained.identity || framed != retained.framed {
            return Err(StorageError::DeleteResidueChanged {
                name: retained.display.clone(),
            });
        }
        Ok(layout)
    }

    fn open_delete_recovery_record(
        &self,
        name: &CStr,
        display: &str,
    ) -> Result<std::fs::File, StorageError> {
        let file = openat2_file(
            self.directory.as_raw_fd(),
            name,
            nix::libc::O_RDONLY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK
                | nix::libc::O_NOATIME,
            0,
            controlled_resolution(),
        )
        .map_err(|source| StorageError::ValidateDeleteResidue {
            name: display.to_owned(),
            source,
        })?;
        require_safe_regular_file(&file, &self.path.join(display)).map_err(|source| {
            StorageError::ValidateDeleteResidue {
                name: display.to_owned(),
                source,
            }
        })?;
        Ok(file)
    }

    fn delete_recovery_entries(
        &self,
        cast_directory: &std::fs::File,
    ) -> Result<Vec<CString>, StorageError> {
        let journal = self.revalidate_retained_cast_binding_locked(cast_directory)?;
        let scan = openat2_file(
            journal.as_raw_fd(),
            c".",
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK
                | nix::libc::O_NOATIME,
            0,
            controlled_resolution(),
        )
        .map_err(|source| StorageError::EnumerateJournal { source })?;
        require_directory(&scan, &self.path, DirectoryPolicy::ExactPrivate)
            .map_err(|source| StorageError::EnumerateJournal { source })?;
        require_same_directory(&journal, &scan, &self.path)
            .map_err(|source| StorageError::EnumerateJournal { source })?;
        directory_entries(&scan).map_err(|source| StorageError::EnumerateJournal { source })
    }
}

fn require_unique_delete_residue_inventory(entries: Vec<CString>) -> Result<CString, StorageError> {
    let lock_count = entries
        .iter()
        .filter(|name| name.as_bytes() == LOCK_NAME.to_bytes())
        .count();
    let mut residues = entries
        .iter()
        .filter(|name| valid_delete_name(name.as_bytes()));
    let residue = residues.next().cloned();
    let multiple_residues = residues.next().is_some();
    if lock_count != 1
        || entries.len() != 2
        || multiple_residues
        || residue.is_none()
        || entries.iter().any(|name| {
            name.as_bytes().starts_with(DELETE_PREFIX) && !valid_delete_name(name.as_bytes())
        })
    {
        return Err(delete_residue_entry_set_error(entries));
    }
    Ok(residue.expect("one valid residue was checked above"))
}

fn delete_residue_entry_set_error(entries: Vec<CString>) -> StorageError {
    let mut entries = entries
        .into_iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    entries.sort();
    StorageError::DeleteResidueEntrySetMismatch { entries }
}
