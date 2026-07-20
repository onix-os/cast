use std::sync::Arc;

use super::{
    PublicBindingRevalidationBoundary, TransitionJournalStore, public_binding_revalidation_boundary,
};
use super::super::{StorageError, TransitionRecord, encode, inode_identity, validation::validate_advance};

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
        if !Arc::ptr_eq(&self.binding, &expected.store) || expected.record != *record {
            return Ok(false);
        }
        let _operation = self.lock_operation()?;
        let Some(loaded) = self.load_pinned_revalidated_retained_cast_locked(cast_directory)? else {
            return Ok(false);
        };
        let retained = inode_identity(&expected.canonical)
            .map_err(|source| StorageError::ValidateCanonical { source })?;
        Ok(loaded.record == expected.record && loaded.identity == retained)
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
        if !Arc::ptr_eq(&self.binding, &expected.store) {
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
}
