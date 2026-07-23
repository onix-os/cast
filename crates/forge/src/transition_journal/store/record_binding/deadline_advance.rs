use std::{io::Write as _, os::fd::AsRawFd as _, time::Instant};

#[cfg(test)]
use std::ffi::CString;

use super::TransitionJournalRecordBinding;
use super::super::{
    DurabilityCheckpoint, StorageFaultPoint, TemporaryRecord, TransitionJournalStore, durability_checkpoint,
    storage_fault,
};
use super::super::super::{
    CANONICAL_NAME, StorageError, TransitionRecord, controlled_resolution, encode, inode_identity, openat2_file,
    renameat2, require_safe_regular_file, require_same_inode, unlinkat, validation::validate_advance,
};

/// Two immutable monotonic observations used to test the admission and final
/// publication boundaries without letting a clock callback mutate namespace
/// state. Namespace races use a separate, explicitly named one-shot hook.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ScriptedBoundAdvanceDeadlineClock {
    readings: [Instant; 2],
    samples: usize,
}

#[cfg(test)]
impl ScriptedBoundAdvanceDeadlineClock {
    pub(crate) const fn new(readings: [Instant; 2]) -> Self {
        Self { readings, samples: 0 }
    }

    pub(crate) const fn samples(&self) -> usize {
        self.samples
    }

    fn read(&mut self) -> Instant {
        let reading = *self
            .readings
            .get(self.samples)
            .expect("deadline-bound advance sampled its clock more than twice");
        self.samples += 1;
        reading
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_DEADLINE_CALLBACK: std::cell::RefCell<Option<Box<dyn FnOnce(CString)>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_EXPIRED_CLEANUP_CALLBACK: std::cell::RefCell<Option<Box<dyn FnOnce(CString)>>> =
        const { std::cell::RefCell::new(None) };
}

/// Arm one explicit namespace-race callback before the final temporary-name
/// reauthentication and deadline sample. Clock readings remain immutable and
/// side-effect free.
#[cfg(test)]
pub(crate) fn arm_bound_advance_before_final_deadline_callback(
    callback: impl FnOnce(CString) + 'static,
) {
    BEFORE_FINAL_DEADLINE_CALLBACK.with(|armed| {
        assert!(
            armed.borrow_mut().replace(Box::new(callback)).is_none(),
            "a bound-advance final-deadline callback is already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn assert_bound_advance_before_final_deadline_callback_consumed() {
    BEFORE_FINAL_DEADLINE_CALLBACK.with(|armed| {
        assert!(
            armed.borrow().is_none(),
            "the bound-advance final-deadline callback was not reached"
        );
    });
}

/// Arm one namespace-race callback after expiry is known but before exact
/// temporary cleanup authentication. This hook cannot reach the publication
/// path.
#[cfg(test)]
pub(crate) fn arm_bound_advance_before_expired_cleanup_callback(
    callback: impl FnOnce(CString) + 'static,
) {
    BEFORE_EXPIRED_CLEANUP_CALLBACK.with(|armed| {
        assert!(
            armed.borrow_mut().replace(Box::new(callback)).is_none(),
            "a bound-advance expired-cleanup callback is already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn assert_bound_advance_before_expired_cleanup_callback_consumed() {
    BEFORE_EXPIRED_CLEANUP_CALLBACK.with(|armed| {
        assert!(
            armed.borrow().is_none(),
            "the bound-advance expired-cleanup callback was not reached"
        );
    });
}

#[cfg(test)]
fn bound_advance_before_final_deadline_boundary(temporary_name: &std::ffi::CStr) {
    let callback = BEFORE_FINAL_DEADLINE_CALLBACK.with(|armed| armed.borrow_mut().take());
    if let Some(callback) = callback {
        callback(temporary_name.to_owned());
    }
}

#[cfg(test)]
fn bound_advance_before_expired_cleanup_boundary(temporary_name: &std::ffi::CStr) {
    let callback = BEFORE_EXPIRED_CLEANUP_CALLBACK.with(|armed| armed.borrow_mut().take());
    if let Some(callback) = callback {
        callback(temporary_name.to_owned());
    }
}

impl TransitionJournalStore {
    /// Consume an exact predecessor binding and publish its legal successor
    /// only while both admission and the final exchange boundary remain
    /// inside one inherited monotonic deadline.
    #[allow(dead_code)] // consumed by the exact boot-sync completion slice
    pub(crate) fn advance_record_binding_until(
        &self,
        cast_directory: &std::fs::File,
        expected: TransitionJournalRecordBinding,
        next: &TransitionRecord,
        deadline: Instant,
    ) -> Result<TransitionJournalRecordBinding, StorageError> {
        let mut clock = Instant::now;
        self.advance_record_binding_until_with_clock_inner(
            cast_directory,
            expected,
            next,
            deadline,
            &mut clock,
        )
    }

    #[cfg(test)]
    pub(crate) fn advance_record_binding_until_with_test_clock(
        &self,
        cast_directory: &std::fs::File,
        expected: TransitionJournalRecordBinding,
        next: &TransitionRecord,
        deadline: Instant,
        clock: &mut ScriptedBoundAdvanceDeadlineClock,
    ) -> Result<TransitionJournalRecordBinding, StorageError> {
        self.advance_record_binding_until_with_clock_inner(
            cast_directory,
            expected,
            next,
            deadline,
            &mut || clock.read(),
        )
    }

    fn advance_record_binding_until_with_clock_inner(
        &self,
        cast_directory: &std::fs::File,
        expected: TransitionJournalRecordBinding,
        next: &TransitionRecord,
        deadline: Instant,
        clock: &mut impl FnMut() -> Instant,
    ) -> Result<TransitionJournalRecordBinding, StorageError> {
        if !self.has_record_store_binding(&expected) {
            return Err(StorageError::CanonicalChanged);
        }
        let _operation = self.lock_operation()?;

        validate_advance(&expected.record, next).map_err(StorageError::InvalidAdvance)?;
        let framed = encode(next).map_err(StorageError::Encode)?;
        let loaded = self
            .load_pinned_revalidated_retained_cast_locked(cast_directory)?
            .ok_or(StorageError::CanonicalChanged)?;
        let retained = inode_identity(&expected.canonical)
            .map_err(|source| StorageError::ValidateCanonical { source })?;
        if loaded.record != expected.record || loaded.identity != retained {
            return Err(StorageError::CanonicalChanged);
        }

        super::super::public_binding_revalidation_boundary(
            super::super::PublicBindingRevalidationBoundary::BeforeBoundAdvancePublish,
        );
        let journal = self.revalidate_retained_cast_binding_locked(cast_directory)?;
        self.revalidate_exact_public_state(&journal, Some(&loaded))?;

        require_deadline(deadline, clock())?;
        let temporary = self.prepare_bound_update_temporary(&framed)?;
        let named = self
            .open_named(CANONICAL_NAME)?
            .ok_or(StorageError::CanonicalChanged)?;
        require_same_inode(
            loaded.identity,
            inode_identity(&named).map_err(|source| StorageError::ValidateCanonical { source })?,
        )?;
        self.require_prepared_bound_update_temporary(&temporary)?;

        self.publish_prepared_bound_update(&temporary, &loaded, deadline, clock)?;
        let published = super::super::LoadedRecord {
            record: next.clone(),
            framed,
            _file: temporary.file,
            identity: temporary.identity,
        };
        drop(expected);

        super::super::public_binding_revalidation_boundary(
            super::super::PublicBindingRevalidationBoundary::BeforeBoundAdvanceFinalBinding,
        );
        let journal = self.revalidate_retained_cast_binding_locked(cast_directory)?;
        self.revalidate_exact_public_state(&journal, Some(&published))?;
        Ok(TransitionJournalRecordBinding {
            store: std::sync::Arc::clone(&self.binding),
            canonical: std::sync::Arc::new(published._file),
            record: published.record,
        })
    }

    fn prepare_bound_update_temporary(&self, framed: &[u8]) -> Result<TemporaryRecord, StorageError> {
        let mut temporary = self.create_temporary()?;
        if let Err(source) = temporary.file.write_all(framed) {
            self.cleanup_temporary(&temporary)?;
            return Err(StorageError::WriteTemporary { source });
        }
        if let Err(source) = storage_fault(StorageFaultPoint::TemporarySync)
            .and_then(|()| temporary.file.sync_all())
        {
            self.cleanup_temporary(&temporary)?;
            return Err(StorageError::SyncTemporary { source });
        }
        #[cfg(test)]
        super::super::journal_update_durability_boundary(
            super::super::JournalUpdateDurabilityBoundary::TemporaryFullySynced,
        );
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
        Ok(temporary)
    }

    fn require_prepared_bound_update_temporary(
        &self,
        temporary: &TemporaryRecord,
    ) -> Result<(), StorageError> {
        let display = temporary.name.to_string_lossy().into_owned();
        let named = openat2_file(
            self.directory.as_raw_fd(),
            &temporary.name,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        )
        .map_err(|source| StorageError::ValidateTemporary { source })?;
        require_safe_regular_file(&named, &self.path.join(display))
            .map_err(|source| StorageError::ValidateTemporary { source })?;
        require_same_inode(
            temporary.identity,
            inode_identity(&named).map_err(|source| StorageError::ValidateTemporary { source })?,
        )
    }

    fn cleanup_expired_bound_update_temporary(
        &self,
        temporary: &TemporaryRecord,
    ) -> Result<(), StorageError> {
        let display = temporary.name.to_string_lossy().into_owned();
        let named = openat2_file(
            self.directory.as_raw_fd(),
            &temporary.name,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        )
        .map_err(|source| StorageError::CleanupTemporary {
            name: display.clone(),
            source,
        })?;
        require_safe_regular_file(&named, &self.path.join(&display))
            .map_err(|source| StorageError::ValidateTemporary { source })?;
        let actual = inode_identity(&named).map_err(|source| StorageError::CleanupTemporary {
            name: display.clone(),
            source,
        })?;
        if actual != temporary.identity {
            return Err(StorageError::CanonicalChanged);
        }

        storage_fault(StorageFaultPoint::BoundAdvanceDeadlineCleanupUnlink)
            .and_then(|()| unlinkat(self.directory.as_raw_fd(), &temporary.name))
            .map_err(|source| StorageError::CleanupTemporary {
                name: display,
                source,
            })?;
        storage_fault(StorageFaultPoint::BoundAdvanceDeadlineCleanupDirectorySync)
            .and_then(|()| self.directory.sync_all())
            .map_err(|source| StorageError::SyncJournalDirectory { source })?;
        durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);
        Ok(())
    }

    fn publish_prepared_bound_update(
        &self,
        temporary: &TemporaryRecord,
        existing: &super::super::LoadedRecord,
        deadline: Instant,
        clock: &mut impl FnMut() -> Instant,
    ) -> Result<(), StorageError> {
        #[cfg(test)]
        bound_advance_before_final_deadline_boundary(&temporary.name);
        self.require_prepared_bound_update_temporary(temporary)?;
        if clock() > deadline {
            #[cfg(test)]
            bound_advance_before_expired_cleanup_boundary(&temporary.name);
            self.cleanup_expired_bound_update_temporary(temporary)?;
            return Err(StorageError::BoundAdvanceDeadlineExceeded { deadline });
        }
        // The pure final clock sample follows all potentially blocking
        // validation. From here to RENAME_EXCHANGE there is no optional
        // production work. As in the ordinary journal update, an uncooperative
        // same-credential writer racing this final compare/exchange window is
        // outside the store's cooperative-writer contract.
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
        #[cfg(test)]
        super::super::journal_update_durability_boundary(
            super::super::JournalUpdateDurabilityBoundary::CanonicalExchanged,
        );
        durability_checkpoint(DurabilityCheckpoint::CanonicalExchanged);

        let canonical = self
            .open_named(CANONICAL_NAME)?
            .ok_or(StorageError::CanonicalChanged)?;
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
        #[cfg(test)]
        super::super::journal_update_durability_boundary(
            super::super::JournalUpdateDurabilityBoundary::UpdateFirstDirectorySynced,
        );
        durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);
        storage_fault(StorageFaultPoint::DisplacedUnlink)
            .and_then(|()| unlinkat(self.directory.as_raw_fd(), &temporary.name))
            .map_err(|source| StorageError::DeleteDisplaced { source })?;
        #[cfg(test)]
        super::super::journal_update_durability_boundary(
            super::super::JournalUpdateDurabilityBoundary::DisplacedUnlinked,
        );
        durability_checkpoint(DurabilityCheckpoint::DisplacedUnlinked);
        storage_fault(StorageFaultPoint::UpdateFinalDirectorySync)
            .and_then(|()| self.directory.sync_all())
            .map_err(|source| StorageError::SyncJournalDirectory { source })?;
        #[cfg(test)]
        super::super::journal_update_durability_boundary(
            super::super::JournalUpdateDurabilityBoundary::UpdateFinalDirectorySynced,
        );
        durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);
        Ok(())
    }
}

fn require_deadline(deadline: Instant, now: Instant) -> Result<(), StorageError> {
    if now > deadline {
        Err(StorageError::BoundAdvanceDeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}
