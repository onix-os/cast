//! Clean-journal permission for legacy in-process boot repair.
//!
//! The durable transition coordinator owns a non-empty journal. Requiring
//! this authority at the legacy compensating call therefore makes the two
//! repair routes disjoint without removing the still-live legacy recovery
//! behavior before forward coordinator integration is complete.

use thiserror::Error;

use crate::{Installation, db, installation, transition_journal};

use super::StatefulTreeIdentity;

/// Non-cloneable permission for one legacy compensating synchronization while
/// the exact clean journal and state-database capabilities remain retained by
/// the borrowed tree identity.
pub(crate) struct LegacyBootRepairAuthority<'identity> {
    identity: &'identity StatefulTreeIdentity,
}

impl StatefulTreeIdentity {
    /// Bind legacy repair to the exact client installation and state database,
    /// then establish public journal absence and orphan-row absence.
    pub(crate) fn authorize_legacy_boot_repair(
        &self,
        installation: &Installation,
        state_database: &db::state::Database,
    ) -> Result<LegacyBootRepairAuthority<'_>, LegacyBootRepairAuthorityError> {
        let authority = LegacyBootRepairAuthority { identity: self };
        authority.revalidate(installation, state_database)?;
        Ok(authority)
    }
}

impl LegacyBootRepairAuthority<'_> {
    /// Revalidate the client-to-identity binding and exact clean transition
    /// evidence without releasing the identity's retained journal lock.
    pub(crate) fn revalidate(
        &self,
        installation: &Installation,
        state_database: &db::state::Database,
    ) -> Result<(), LegacyBootRepairAuthorityError> {
        if !self.identity.state_database.same_instance(state_database) {
            return Err(LegacyBootRepairAuthorityError::StateDatabaseMismatch);
        }

        installation.revalidate_mutable_namespace()?;
        let cast = installation.retained_mutable_cast_directory()?;
        let record = self.identity.journal.load_revalidated_retained_cast(cast);
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let record = record?;
        if let Some(record) = record {
            return Err(LegacyBootRepairAuthorityError::UnresolvedJournal {
                transition: record.transition_id.as_str().to_owned(),
            });
        }

        let in_flight = self.identity.state_database.audit_in_flight_transition();
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;

        // Repeat public binding and absence last: the installation namespace
        // does not itself retain the public journal child or its lock inode.
        let cast = installation.retained_mutable_cast_directory()?;
        let trailing_record = self.identity.journal.load_revalidated_retained_cast(cast);
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let trailing_record = trailing_record?;
        if let Some(record) = trailing_record {
            return Err(LegacyBootRepairAuthorityError::UnresolvedJournal {
                transition: record.transition_id.as_str().to_owned(),
            });
        }

        if let Some(orphan) = in_flight? {
            return Err(LegacyBootRepairAuthorityError::OrphanTransitionRow {
                state: i32::from(orphan.state_id),
                transition: orphan.transition_id.as_str().to_owned(),
            });
        }
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum LegacyBootRepairAuthorityError {
    #[error("legacy boot repair client does not share the retained state-database capability")]
    StateDatabaseMismatch,
    #[error("revalidate the retained installation during legacy boot repair authorization")]
    Installation(#[from] installation::Error),
    #[error("bind or inspect the public transition journal during legacy boot repair authorization")]
    Journal(#[from] transition_journal::StorageError),
    #[error("audit transition ownership during legacy boot repair authorization")]
    Database(#[from] db::state::TransitionEvidenceError),
    #[error("legacy boot repair is blocked by unresolved transition {transition}")]
    UnresolvedJournal { transition: String },
    #[error("legacy boot repair is blocked by orphan transition {transition} on state {state}")]
    OrphanTransitionRow { state: i32, transition: String },
}
