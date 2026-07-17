//! Persistence-facing proof and successor projection for DB invalidation.
//!
//! This child exposes only read-only revalidation of the retained joint
//! absence and the successor fixed by the effect authority's private origin.
//! It performs no database, namespace, or journal mutation.

use crate::{
    Installation,
    transition_journal::{CodecError, TransitionJournalStore, TransitionRecord},
};

use super::super::{
    FreshDbInvalidationDatabaseKind, UsrRollbackFreshDbInvalidationAuthorityError,
    UsrRollbackFreshDbInvalidationAuthorityErrorKind, fresh_db_invalidation_plan_is_exact, inspect_current_database,
    require_exact_database,
};
use super::UsrRollbackFreshDbInvalidationEffectAuthority;

impl UsrRollbackFreshDbInvalidationEffectAuthority<'_> {
    /// Revalidate the exact source record, bound joint absence, and preserved
    /// namespace without repeating the database effect.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackFreshDbInvalidationAuthorityError> {
        // The per-open binding is deliberately the first retained-evidence
        // observation on every persistence-side revalidation.
        if !journal.has_binding(&self.journal_binding) {
            return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::JournalBindingMismatch.into());
        }
        if self.database.kind() != FreshDbInvalidationDatabaseKind::JointlyAbsent {
            return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::EvidenceMismatch.into());
        }

        self.installation.revalidate_mutable_namespace()?;
        let database_before =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        self.namespace.revalidate(&self.installation, journal, &self.record)?;
        let database_after =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        if database_before != database_after
            || !fresh_db_invalidation_plan_is_exact(&self.record)
            || self.database.kind() != FreshDbInvalidationDatabaseKind::JointlyAbsent
        {
            return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::EvidenceMismatch.into());
        }
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

    /// Derive the sole legal `FreshDbInvalidated` successor from the origin
    /// fixed by this authority's construction path.
    pub(in crate::client) fn fresh_db_invalidated_successor(&self) -> Result<TransitionRecord, CodecError> {
        self.record.rollback_successor(Some(self.origin))
    }
}
