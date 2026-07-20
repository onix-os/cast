//! Persistence-facing proof and bound publication for DB invalidation.
//!
//! This child owns read-only revalidation of the retained joint absence and
//! the sole journal mutation that publishes the successor fixed by the effect
//! authority's private origin. It performs no database or namespace mutation.

use crate::{
    Installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    FreshDbInvalidationDatabaseKind, UsrRollbackFreshDbInvalidationAuthorityError,
    UsrRollbackFreshDbInvalidationAuthorityErrorKind, fresh_db_invalidation_plan_is_exact, inspect_current_database,
    require_exact_database,
};
use super::UsrRollbackFreshDbInvalidationEffectAuthority;

/// Exact authority-derived `FreshDbInvalidated` publication and its new inode
/// binding.
pub(in crate::client) struct UsrRollbackFreshDbInvalidationPublishedRecord {
    record: TransitionRecord,
    binding: TransitionJournalRecordBinding,
}

impl UsrRollbackFreshDbInvalidationPublishedRecord {
    pub(in crate::client) fn into_parts(self) -> (TransitionRecord, TransitionJournalRecordBinding) {
        (self.record, self.binding)
    }
}

impl UsrRollbackFreshDbInvalidationEffectAuthority<'_> {
    /// Revalidate the exact source record, bound joint absence, and preserved
    /// namespace without repeating the database effect.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackFreshDbInvalidationAuthorityError> {
        // The per-open binding is deliberately the first retained-evidence
        // observation on every persistence-side revalidation.
        super::super::require_journal_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        if self.database.kind() != FreshDbInvalidationDatabaseKind::JointlyAbsent {
            return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::EvidenceMismatch.into());
        }

        self.installation.revalidate_mutable_namespace()?;
        let database_before =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        self.namespace.revalidate(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        let database_after =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        if database_before != database_after
            || !fresh_db_invalidation_plan_is_exact(&self.record)
            || self.database.kind() != FreshDbInvalidationDatabaseKind::JointlyAbsent
        {
            return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::EvidenceMismatch.into());
        }
        super::super::require_journal_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    /// Borrow the retained installation which owns this authority.
    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.installation
    }

    /// Borrow the exact `FreshDbInvalidationIntent` source record.
    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.record
    }

    /// Revalidate, then consume this authority through the exact bound
    /// `FreshDbInvalidationIntent` to `FreshDbInvalidated` journal boundary.
    pub(in crate::client) fn advance_fresh_db_invalidated_record_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackFreshDbInvalidationPublishedRecord, UsrRollbackFreshDbInvalidationRecordAdvanceError> {
        self.revalidate(journal)?;
        let successor = self
            .record
            .rollback_successor(Some(self.origin))
            .map_err(UsrRollbackFreshDbInvalidationRecordAdvanceError::Successor)?;
        if successor.phase != Phase::FreshDbInvalidated {
            return Err(UsrRollbackFreshDbInvalidationRecordAdvanceError::UnexpectedSuccessor {
                phase: successor.phase,
            });
        }
        let cast = self.installation.retained_mutable_cast_directory()?;
        match journal.advance_record_binding(cast, self.journal_record_binding, &successor) {
            Ok(binding) => Ok(UsrRollbackFreshDbInvalidationPublishedRecord {
                record: successor,
                binding,
            }),
            Err(source) => Err(UsrRollbackFreshDbInvalidationRecordAdvanceError::Storage { source, successor }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackFreshDbInvalidationRecordAdvanceError {
    #[error("revalidate exact fresh-database invalidation authority before the bound journal advance")]
    Authority(#[from] UsrRollbackFreshDbInvalidationAuthorityError),
    #[error("revalidate retained installation before the bound FreshDbInvalidated journal advance")]
    Installation(#[from] crate::installation::Error),
    #[error("derive the authority-owned FreshDbInvalidated successor")]
    Successor(#[source] CodecError),
    #[error("authority-owned fresh-database invalidation successor has unexpected phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("advance the exact bound fresh-database invalidation journal record")]
    Storage {
        #[source]
        source: StorageError,
        successor: TransitionRecord,
    },
}
