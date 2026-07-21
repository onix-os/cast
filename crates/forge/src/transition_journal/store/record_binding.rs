use std::{ffi::CStr, os::fd::AsRawFd as _, sync::Arc};

#[cfg(test)]
use std::ffi::CString;

use thiserror::Error;

use super::{
    DurabilityCheckpoint, PublicBindingRevalidationBoundary, StorageFaultPoint,
    TransitionJournalStore, durability_checkpoint, public_binding_revalidation_boundary, storage_fault,
};
use super::super::{
    CANONICAL_NAME, DirectoryPolicy, LOCK_NAME, StorageError, TransitionRecord, delete_name,
    directory_entries, encode, inode_identity, open_existing_directory, read_bounded, renameat2, unlinkat,
    validation::validate_advance,
};

/// Exact same-store public state authenticated after a failed bound deletion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransitionJournalRecordDeleteState {
    ExactSource,
    Absent,
}

/// Failure from a consuming exact-inode terminal journal deletion.
#[derive(Debug, Error)]
pub(crate) enum TransitionJournalRecordDeleteError {
    #[error("authenticate the exact record binding before journal deletion")]
    Admission(#[source] StorageError),
    #[error("terminal journal deletion failed after reconciliation proved {state:?}")]
    Storage {
        state: TransitionJournalRecordDeleteState,
        #[source]
        source: StorageError,
    },
    #[error("terminal journal deletion failed ({storage}) and exact same-store reconciliation failed")]
    StorageAndReconciliation {
        storage: StorageError,
        #[source]
        reconciliation: StorageError,
    },
    #[error("authenticate the exact privately detached journal before unlink")]
    Detached(#[source] StorageError),
    #[error("authenticate exact public absence after terminal journal deletion")]
    PostDelete(#[source] StorageError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundRecordDeleteNameState {
    Absent,
    Exact,
    Foreign,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundRecordDeleteLayout {
    ExactSource,
    ExactDetached,
    Absent,
}

/// Exact publicly named canonical-record inode retained by one open journal
/// store. Equal bytes at the same name are not interchangeable with the inode
/// and decoded record that startup authority originally admitted.
#[derive(Debug)]
pub(crate) struct TransitionJournalRecordBinding {
    store: Arc<()>,
    canonical: Arc<std::fs::File>,
    record: TransitionRecord,
}

impl TransitionJournalStore {
    /// Check only the in-process store identity carried by an exact record
    /// binding. Authorities use this before consulting any installation,
    /// database, or namespace evidence so a mixed store fails binding-first.
    pub(crate) fn has_record_store_binding(&self, expected: &TransitionJournalRecordBinding) -> bool {
        Arc::ptr_eq(&self.binding, &expected.store)
    }

    /// Bind an expected record to this publicly authenticated store and retain
    /// its canonical inode. This is stricter than semantic record equality.
    pub(crate) fn record_binding(
        &self,
        cast_directory: &std::fs::File,
        expected: &TransitionRecord,
    ) -> Result<TransitionJournalRecordBinding, StorageError> {
        let _operation = self.lock_operation()?;
        let loaded = self
            .load_pinned_revalidated_retained_cast_locked(cast_directory)?
            .ok_or(StorageError::CanonicalChanged)?;
        if loaded.record != *expected {
            return Err(StorageError::CanonicalChanged);
        }
        Ok(TransitionJournalRecordBinding {
            store: Arc::clone(&self.binding),
            canonical: Arc::new(loaded._file),
            record: loaded.record,
        })
    }

    /// Reauthenticate the public store, record semantics, and retained
    /// canonical inode captured by `record_binding`.
    pub(crate) fn has_record_binding(
        &self,
        cast_directory: &std::fs::File,
        expected: &TransitionJournalRecordBinding,
        record: &TransitionRecord,
    ) -> Result<bool, StorageError> {
        if !self.has_record_store_binding(expected) || expected.record != *record {
            return Ok(false);
        }
        self.has_public_record_inode_binding(cast_directory, expected, record)
    }

    /// Reauthenticate a freshly reopened store against the exact public
    /// record inode retained by a predecessor store's successful bound
    /// advance. This deliberately does not compare the per-open store marker:
    /// the old lock-bearing store must be gone before the new store can open.
    pub(crate) fn has_reopened_record_binding(
        &self,
        cast_directory: &std::fs::File,
        expected: &TransitionJournalRecordBinding,
        record: &TransitionRecord,
    ) -> Result<bool, StorageError> {
        if expected.record != *record {
            return Ok(false);
        }
        self.has_public_record_inode_binding(cast_directory, expected, record)
    }

    fn has_public_record_inode_binding(
        &self,
        cast_directory: &std::fs::File,
        expected: &TransitionJournalRecordBinding,
        record: &TransitionRecord,
    ) -> Result<bool, StorageError> {
        let _operation = self.lock_operation()?;
        let Some(loaded) = self.load_pinned_revalidated_retained_cast_locked(cast_directory)? else {
            return Ok(false);
        };
        let retained = inode_identity(&expected.canonical)
            .map_err(|source| StorageError::ValidateCanonical { source })?;
        Ok(loaded.record == expected.record && loaded.record == *record && loaded.identity == retained)
    }

    /// Consume one exact predecessor binding and durably publish one legal
    /// successor while the same operation lock protects the complete public
    /// predecessor-to-successor authentication boundary.
    pub(crate) fn advance_record_binding(
        &self,
        cast_directory: &std::fs::File,
        expected: TransitionJournalRecordBinding,
        next: &TransitionRecord,
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

        public_binding_revalidation_boundary(PublicBindingRevalidationBoundary::BeforeBoundAdvancePublish);
        let journal = self.revalidate_retained_cast_binding_locked(cast_directory)?;
        self.revalidate_exact_public_state(&journal, Some(&loaded))?;
        let published = self.publish_record_retained(&framed, next, Some(loaded))?;
        drop(expected);

        public_binding_revalidation_boundary(PublicBindingRevalidationBoundary::BeforeBoundAdvanceFinalBinding);
        let journal = self.revalidate_retained_cast_binding_locked(cast_directory)?;
        self.revalidate_exact_public_state(&journal, Some(&published))?;
        Ok(TransitionJournalRecordBinding {
            store: Arc::clone(&self.binding),
            canonical: Arc::new(published._file),
            record: published.record,
        })
    }

    /// Consume one exact canonical-record binding and durably remove that
    /// terminal record while continuously retaining the same store lock.
    ///
    /// Success means the retained journal directory is still the exact child
    /// of `cast_directory`, its retained lock is still publicly named, and
    /// the public inventory contains only that lock. A storage failure is
    /// classified as the exact bound source or authenticated absence without
    /// retrying the detach or unlink and with at most one reconciliation sync.
    /// Process loss after detach may leave a `.state-transition.delete-*`
    /// residue. A writer reopen restores only the exact recoverable terminal
    /// residue to the canonical name; read-only inspection still rejects it.
    pub(crate) fn delete_record_binding(
        &self,
        cast_directory: &std::fs::File,
        expected: TransitionJournalRecordBinding,
        record: &TransitionRecord,
    ) -> Result<(), TransitionJournalRecordDeleteError> {
        let _operation = self
            .lock_operation()
            .map_err(TransitionJournalRecordDeleteError::Admission)?;
        if !self.has_record_store_binding(&expected) {
            return Err(TransitionJournalRecordDeleteError::Admission(
                StorageError::CanonicalChanged,
            ));
        }
        record
            .validate()
            .map_err(StorageError::Decode)
            .map_err(TransitionJournalRecordDeleteError::Admission)?;
        if !record.phase.deletable() {
            return Err(TransitionJournalRecordDeleteError::Admission(
                StorageError::DeleteNonterminal,
            ));
        }
        if expected.record != *record {
            return Err(TransitionJournalRecordDeleteError::Admission(
                StorageError::ExpectedRecordMismatch,
            ));
        }

        public_binding_revalidation_boundary(PublicBindingRevalidationBoundary::BeforeBoundDeleteAdmission);
        let loaded = self
            .load_pinned_revalidated_retained_cast_locked(cast_directory)
            .map_err(TransitionJournalRecordDeleteError::Admission)?
            .ok_or(StorageError::CanonicalChanged)
            .map_err(TransitionJournalRecordDeleteError::Admission)?;
        require_loaded_record_binding(&expected, record, &loaded)
            .map_err(TransitionJournalRecordDeleteError::Admission)?;

        let journal = self
            .revalidate_retained_cast_binding_locked(cast_directory)
            .map_err(TransitionJournalRecordDeleteError::Admission)?;
        self.revalidate_exact_public_state(&journal, Some(&loaded))
            .map_err(TransitionJournalRecordDeleteError::Admission)?;
        require_loaded_record_binding(&expected, record, &loaded)
            .map_err(TransitionJournalRecordDeleteError::Admission)?;
        let private_name = delete_name();
        bound_delete_private_name_boundary(&private_name);

        // This hook deliberately occupies the final check/syscall window. A
        // replacement cannot be unlinked here: RENAME_NOREPLACE atomically
        // detaches whichever inode won the public name, then the retained
        // record binding decides whether that private inode may be removed.
        public_binding_revalidation_boundary(PublicBindingRevalidationBoundary::BeforeBoundDeleteDetach);
        let detach = storage_fault(StorageFaultPoint::BoundDeleteDetach)
            .and_then(|()| {
                renameat2(
                    self.directory.as_raw_fd(),
                    CANONICAL_NAME,
                    self.directory.as_raw_fd(),
                    &private_name,
                    nix::libc::RENAME_NOREPLACE,
                )
            })
            .and_then(|()| storage_fault(StorageFaultPoint::BoundDeleteDetachReport))
            .map_err(|source| StorageError::DetachCanonical { source });
        let pending_detach_error = match detach {
            Ok(()) => match self.classify_bound_record_delete_layout_locked(
                cast_directory,
                &expected,
                record,
                &loaded,
                &private_name,
            ) {
                Ok(BoundRecordDeleteLayout::ExactDetached) => None,
                Ok(_) => {
                    return Err(TransitionJournalRecordDeleteError::Detached(
                        StorageError::CanonicalChanged,
                    ));
                }
                Err(source) => return Err(TransitionJournalRecordDeleteError::Detached(source)),
            },
            Err(storage) => {
                public_binding_revalidation_boundary(
                    PublicBindingRevalidationBoundary::BeforeBoundDeleteFailureReconciliation,
                );
                match self.classify_bound_record_delete_layout_locked(
                    cast_directory,
                    &expected,
                    record,
                    &loaded,
                    &private_name,
                ) {
                    Ok(BoundRecordDeleteLayout::ExactSource) => {
                        return Err(TransitionJournalRecordDeleteError::Storage {
                            state: TransitionJournalRecordDeleteState::ExactSource,
                            source: storage,
                        });
                    }
                    Ok(BoundRecordDeleteLayout::ExactDetached) => Some(storage),
                    Ok(BoundRecordDeleteLayout::Absent) => {
                        return Err(TransitionJournalRecordDeleteError::Storage {
                            state: TransitionJournalRecordDeleteState::Absent,
                            source: storage,
                        });
                    }
                    Err(reconciliation) => {
                        return Err(TransitionJournalRecordDeleteError::StorageAndReconciliation {
                            storage,
                            reconciliation,
                        });
                    }
                }
            }
        };
        durability_checkpoint(DurabilityCheckpoint::CanonicalDetached);

        if let Err(source) = storage_fault(StorageFaultPoint::CanonicalUnlink) {
            return Err(self.reconcile_private_unlink_failure(
                cast_directory,
                &expected,
                record,
                &loaded,
                &private_name,
                StorageError::DeleteDetachedCanonical { source },
                false,
            ));
        }
        public_binding_revalidation_boundary(PublicBindingRevalidationBoundary::BeforeBoundDeletePrivateUnlink);
        match self.classify_bound_record_delete_layout_locked(
            cast_directory,
            &expected,
            record,
            &loaded,
            &private_name,
        ) {
            Ok(BoundRecordDeleteLayout::ExactDetached) => {}
            Ok(_) => {
                return Err(TransitionJournalRecordDeleteError::Detached(
                    StorageError::CanonicalChanged,
                ));
            }
            Err(source) => return Err(TransitionJournalRecordDeleteError::Detached(source)),
        }

        // The generated name is fresh and collision-detecting, not secret.
        // The 0700 journal, retained lock, and operation lock define a
        // cooperative same-process boundary. An uncooperative same-credential
        // writer replacing this private name inside the final compare/unlink
        // syscall window is outside that boundary. Intentionally perform no
        // optional callback, fault hook, or other work after the exact-private
        // classification and before this sole unlink.
        let unlink = unlinkat(self.directory.as_raw_fd(), &private_name);
        let unlink_applied = unlink.is_ok();
        if unlink_applied {
            #[cfg(test)]
            super::journal_delete_durability_boundary(super::JournalDeleteDurabilityBoundary::CanonicalUnlinked);
            durability_checkpoint(DurabilityCheckpoint::CanonicalUnlinked);
        }
        // This post-syscall fault represents an ambiguous report after the
        // sole unlink actually completed. Reconciliation must prove absence,
        // sync once, and must never retry that unlink.
        let unlink = unlink
            .and_then(|()| storage_fault(StorageFaultPoint::BoundDeleteUnlinkReport))
            .map_err(|source| StorageError::DeleteDetachedCanonical { source });
        if let Err(source) = unlink {
            return Err(self.reconcile_private_unlink_failure(
                cast_directory,
                &expected,
                record,
                &loaded,
                &private_name,
                source,
                unlink_applied,
            ));
        }

        public_binding_revalidation_boundary(PublicBindingRevalidationBoundary::AfterBoundDeleteUnlink);
        self.require_bound_record_absence_locked(cast_directory, &expected, record, &loaded, &private_name, None)
            .map_err(TransitionJournalRecordDeleteError::PostDelete)?;

        let directory_sync = self.sync_bound_delete_directory();
        if let Err(source) = directory_sync {
            return Err(self.reconcile_clean_delete_failure(
                cast_directory,
                &expected,
                record,
                &loaded,
                &private_name,
                source,
            ));
        }
        #[cfg(test)]
        super::journal_delete_durability_boundary(super::JournalDeleteDurabilityBoundary::DeleteDirectorySynced);
        durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);

        public_binding_revalidation_boundary(PublicBindingRevalidationBoundary::BeforeBoundDeletePublication);
        self.require_bound_record_absence_locked(
            cast_directory,
            &expected,
            record,
            &loaded,
            &private_name,
            Some(PublicBindingRevalidationBoundary::BeforeBoundDeletePublicationFinalBinding),
        )
            .map_err(TransitionJournalRecordDeleteError::PostDelete)?;
        drop(expected);
        match pending_detach_error {
            Some(source) => Err(TransitionJournalRecordDeleteError::Storage {
                state: TransitionJournalRecordDeleteState::Absent,
                source,
            }),
            None => Ok(()),
        }
    }

    fn reconcile_private_unlink_failure(
        &self,
        cast_directory: &std::fs::File,
        expected: &TransitionJournalRecordBinding,
        record: &TransitionRecord,
        loaded: &super::LoadedRecord,
        private_name: &CStr,
        storage: StorageError,
        unlink_applied: bool,
    ) -> TransitionJournalRecordDeleteError {
        public_binding_revalidation_boundary(
            PublicBindingRevalidationBoundary::BeforeBoundDeleteFailureReconciliation,
        );
        match self.classify_bound_record_delete_layout_locked(
            cast_directory,
            expected,
            record,
            loaded,
            private_name,
        ) {
            Ok(BoundRecordDeleteLayout::ExactSource) => self.finish_reconciled_delete_state(
                cast_directory,
                expected,
                record,
                loaded,
                private_name,
                storage,
                TransitionJournalRecordDeleteState::ExactSource,
                unlink_applied,
            ),
            Ok(BoundRecordDeleteLayout::Absent) => {
                self.finish_reconciled_delete_state(
                    cast_directory,
                    expected,
                    record,
                    loaded,
                    private_name,
                    storage,
                    TransitionJournalRecordDeleteState::Absent,
                    unlink_applied,
                )
            }
            Ok(BoundRecordDeleteLayout::ExactDetached) => {
                let restore = renameat2(
                    self.directory.as_raw_fd(),
                    private_name,
                    self.directory.as_raw_fd(),
                    CANONICAL_NAME,
                    nix::libc::RENAME_NOREPLACE,
                )
                .map_err(|source| StorageError::RestoreCanonical { source });
                match self.classify_bound_record_delete_layout_locked(
                    cast_directory,
                    expected,
                    record,
                    loaded,
                    private_name,
                ) {
                    Ok(BoundRecordDeleteLayout::ExactSource) => self.finish_reconciled_delete_state(
                        cast_directory,
                        expected,
                        record,
                        loaded,
                        private_name,
                        storage,
                        TransitionJournalRecordDeleteState::ExactSource,
                        unlink_applied,
                    ),
                    Ok(BoundRecordDeleteLayout::Absent) => self.finish_reconciled_delete_state(
                        cast_directory,
                        expected,
                        record,
                        loaded,
                        private_name,
                        storage,
                        TransitionJournalRecordDeleteState::Absent,
                        unlink_applied,
                    ),
                    Ok(BoundRecordDeleteLayout::ExactDetached) => {
                        TransitionJournalRecordDeleteError::StorageAndReconciliation {
                            storage,
                            reconciliation: restore.err().unwrap_or(StorageError::CanonicalChanged),
                        }
                    }
                    Err(reconciliation) => TransitionJournalRecordDeleteError::StorageAndReconciliation {
                        storage,
                        reconciliation,
                    },
                }
            }
            Err(reconciliation) => TransitionJournalRecordDeleteError::StorageAndReconciliation {
                storage,
                reconciliation,
            },
        }
    }

    fn finish_reconciled_delete_state(
        &self,
        cast_directory: &std::fs::File,
        expected: &TransitionJournalRecordBinding,
        record: &TransitionRecord,
        loaded: &super::LoadedRecord,
        private_name: &CStr,
        storage: StorageError,
        state: TransitionJournalRecordDeleteState,
        unlink_applied: bool,
    ) -> TransitionJournalRecordDeleteError {
        match self.sync_bound_delete_directory() {
            Ok(()) => {
                if unlink_applied {
                    #[cfg(test)]
                    super::journal_delete_durability_boundary(
                        super::JournalDeleteDurabilityBoundary::DeleteDirectorySynced,
                    );
                }
                durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);
                match self.classify_bound_record_delete_layout_locked(
                    cast_directory,
                    expected,
                    record,
                    loaded,
                    private_name,
                ) {
                    Ok(BoundRecordDeleteLayout::ExactSource)
                        if state == TransitionJournalRecordDeleteState::ExactSource =>
                    {
                        TransitionJournalRecordDeleteError::Storage { state, source: storage }
                    }
                    Ok(BoundRecordDeleteLayout::Absent) if state == TransitionJournalRecordDeleteState::Absent => {
                        TransitionJournalRecordDeleteError::Storage { state, source: storage }
                    }
                    Ok(_) => TransitionJournalRecordDeleteError::StorageAndReconciliation {
                        storage,
                        reconciliation: StorageError::CanonicalChanged,
                    },
                    Err(reconciliation) => TransitionJournalRecordDeleteError::StorageAndReconciliation {
                        storage,
                        reconciliation,
                    },
                }
            }
            Err(reconciliation) => TransitionJournalRecordDeleteError::StorageAndReconciliation {
                storage,
                reconciliation,
            },
        }
    }

    fn reconcile_clean_delete_failure(
        &self,
        cast_directory: &std::fs::File,
        expected: &TransitionJournalRecordBinding,
        record: &TransitionRecord,
        loaded: &super::LoadedRecord,
        private_name: &CStr,
        storage: StorageError,
    ) -> TransitionJournalRecordDeleteError {
        public_binding_revalidation_boundary(
            PublicBindingRevalidationBoundary::BeforeBoundDeleteFailureReconciliation,
        );
        match self.classify_bound_record_delete_layout_locked(
            cast_directory,
            expected,
            record,
            loaded,
            private_name,
        ) {
            Ok(BoundRecordDeleteLayout::ExactSource) => TransitionJournalRecordDeleteError::Storage {
                state: TransitionJournalRecordDeleteState::ExactSource,
                source: storage,
            },
            Ok(BoundRecordDeleteLayout::Absent) => TransitionJournalRecordDeleteError::Storage {
                state: TransitionJournalRecordDeleteState::Absent,
                source: storage,
            },
            Ok(BoundRecordDeleteLayout::ExactDetached) => {
                TransitionJournalRecordDeleteError::StorageAndReconciliation {
                    storage,
                    reconciliation: StorageError::CanonicalChanged,
                }
            }
            Err(reconciliation) => TransitionJournalRecordDeleteError::StorageAndReconciliation {
                storage,
                reconciliation,
            },
        }
    }

    fn require_bound_record_absence_locked(
        &self,
        cast_directory: &std::fs::File,
        expected: &TransitionJournalRecordBinding,
        record: &TransitionRecord,
        loaded: &super::LoadedRecord,
        private_name: &CStr,
        final_binding_boundary: Option<PublicBindingRevalidationBoundary>,
    ) -> Result<(), StorageError> {
        match self.classify_bound_record_delete_layout_at_boundary_locked(
            cast_directory,
            expected,
            record,
            loaded,
            private_name,
            final_binding_boundary,
        )? {
            BoundRecordDeleteLayout::Absent => Ok(()),
            BoundRecordDeleteLayout::ExactSource | BoundRecordDeleteLayout::ExactDetached => {
                Err(StorageError::CanonicalChanged)
            }
        }
    }

    fn classify_bound_record_delete_layout_locked(
        &self,
        cast_directory: &std::fs::File,
        expected: &TransitionJournalRecordBinding,
        record: &TransitionRecord,
        loaded: &super::LoadedRecord,
        private_name: &CStr,
    ) -> Result<BoundRecordDeleteLayout, StorageError> {
        self.classify_bound_record_delete_layout_at_boundary_locked(
            cast_directory,
            expected,
            record,
            loaded,
            private_name,
            None,
        )
    }

    fn classify_bound_record_delete_layout_at_boundary_locked(
        &self,
        cast_directory: &std::fs::File,
        expected: &TransitionJournalRecordBinding,
        record: &TransitionRecord,
        loaded: &super::LoadedRecord,
        private_name: &CStr,
        final_binding_boundary: Option<PublicBindingRevalidationBoundary>,
    ) -> Result<BoundRecordDeleteLayout, StorageError> {
        require_loaded_record_binding(expected, record, loaded)?;
        let journal = self.revalidate_retained_cast_binding_locked(cast_directory)?;
        let first = self.observe_bound_record_delete_layout(&journal, loaded, private_name)?;
        if let Some(boundary) = final_binding_boundary {
            public_binding_revalidation_boundary(boundary);
        }
        let journal = self.revalidate_retained_cast_binding_locked(cast_directory)?;
        let second = self.observe_bound_record_delete_layout(&journal, loaded, private_name)?;
        if first != second {
            return Err(StorageError::CanonicalChanged);
        }
        Ok(second)
    }

    fn observe_bound_record_delete_layout(
        &self,
        journal: &std::fs::File,
        loaded: &super::LoadedRecord,
        private_name: &CStr,
    ) -> Result<BoundRecordDeleteLayout, StorageError> {
        let scan = open_existing_directory(&journal, c".", &self.path, DirectoryPolicy::ExactPrivate)
            .map_err(|source| StorageError::RevalidateJournalEntrySet { source })?;
        let names = directory_entries(&scan)
            .map_err(|source| StorageError::RevalidateJournalEntrySet { source })?;
        let lock_count = names
            .iter()
            .filter(|name| name.as_bytes() == LOCK_NAME.to_bytes())
            .count();
        let canonical_count = names
            .iter()
            .filter(|name| name.as_bytes() == CANONICAL_NAME.to_bytes())
            .count();
        let private_count = names
            .iter()
            .filter(|name| name.as_bytes() == private_name.to_bytes())
            .count();
        if lock_count != 1
            || canonical_count > 1
            || private_count > 1
            || names.len() != lock_count + canonical_count + private_count
        {
            let mut entries = names
                .into_iter()
                .map(|name| name.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            entries.sort();
            return Err(StorageError::BoundDeleteEntrySetMismatch {
                private_name: private_name.to_string_lossy().into_owned(),
                entries,
            });
        }
        let canonical = self.observe_bound_record_delete_name(CANONICAL_NAME, loaded)?;
        let private = self.observe_bound_record_delete_name(private_name, loaded)?;
        match (canonical, private) {
            (BoundRecordDeleteNameState::Exact, BoundRecordDeleteNameState::Absent) => {
                Ok(BoundRecordDeleteLayout::ExactSource)
            }
            (BoundRecordDeleteNameState::Absent, BoundRecordDeleteNameState::Exact) => {
                Ok(BoundRecordDeleteLayout::ExactDetached)
            }
            (BoundRecordDeleteNameState::Absent, BoundRecordDeleteNameState::Absent) => {
                Ok(BoundRecordDeleteLayout::Absent)
            }
            _ => Err(StorageError::CanonicalChanged),
        }
    }

    fn observe_bound_record_delete_name(
        &self,
        name: &CStr,
        loaded: &super::LoadedRecord,
    ) -> Result<BoundRecordDeleteNameState, StorageError> {
        let Some(mut file) = self.open_named(name)? else {
            return Ok(BoundRecordDeleteNameState::Absent);
        };
        let identity = inode_identity(&file)
            .map_err(|source| StorageError::ValidateCanonical { source })?;
        let framed = read_bounded(&mut file)
            .map_err(|source| StorageError::ReadCanonical { source })?;
        if identity == loaded.identity && framed == loaded.framed {
            Ok(BoundRecordDeleteNameState::Exact)
        } else {
            Ok(BoundRecordDeleteNameState::Foreign)
        }
    }

    fn sync_bound_delete_directory(&self) -> Result<(), StorageError> {
        storage_fault(StorageFaultPoint::DeleteDirectorySync)
            .and_then(|()| self.directory.sync_all())
            .map_err(|source| StorageError::SyncJournalDirectory { source })?;
        Ok(())
    }
}

fn require_loaded_record_binding(
    expected: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
    loaded: &super::LoadedRecord,
) -> Result<(), StorageError> {
    let retained = inode_identity(&expected.canonical)
        .map_err(|source| StorageError::ValidateCanonical { source })?;
    if expected.record != *record || loaded.record != *record || loaded.identity != retained {
        return Err(StorageError::CanonicalChanged);
    }
    Ok(())
}

#[cfg(test)]
std::thread_local! {
    static BOUND_DELETE_PRIVATE_NAME_CALLBACK: std::cell::RefCell<Option<Box<dyn FnOnce(CString)>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_bound_delete_private_name_callback(callback: impl FnOnce(CString) + 'static) {
    BOUND_DELETE_PRIVATE_NAME_CALLBACK.with(|armed| {
        assert!(
            armed.borrow_mut().replace(Box::new(callback)).is_none(),
            "a bound-delete private-name callback is already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn assert_bound_delete_private_name_callback_consumed() {
    BOUND_DELETE_PRIVATE_NAME_CALLBACK.with(|armed| {
        assert!(armed.borrow().is_none(), "bound-delete private-name callback was not reached");
    });
}

#[cfg(test)]
fn bound_delete_private_name_boundary(name: &CStr) {
    let callback = BOUND_DELETE_PRIVATE_NAME_CALLBACK.with(|armed| armed.borrow_mut().take());
    if let Some(callback) = callback {
        callback(name.to_owned());
    }
}

#[cfg(not(test))]
fn bound_delete_private_name_boundary(_name: &CStr) {}
