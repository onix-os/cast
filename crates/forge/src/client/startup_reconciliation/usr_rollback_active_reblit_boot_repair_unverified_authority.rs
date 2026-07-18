//! Independent journal-only authority for conservatively retaining an
//! interrupted ActiveReblit boot repair as `BootRepairUnverified`.
//!
//! This authority is captured only from a `BootRepairStarted` record observed
//! at startup entry. It independently recaptures the complete target state,
//! live active selection, database/provenance context, and phase-specific
//! preserved-wrapper namespace. It owns no claimed mutable-system typestate and
//! cannot invoke boot.

use crate::{
    Installation, db,
    transition_journal::{
        CodecError, Operation, Phase, TransitionJournalBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::{ActiveStateReservation, ActiveStateSnapshot},
    startup_gate::UsrRollbackActiveReblitBootRepairUnverifiedSeal,
};
use super::{
    ActiveReblitBootRepairDatabaseEvidence, ActiveReblitBootRepairDatabaseInspection,
    ActiveReblitBootRepairEvidenceError, UsrRollbackActiveReblitBootRepairStartedNamespaceError,
    UsrRollbackActiveReblitBootRepairStartedNamespaceInspection,
    UsrRollbackActiveReblitBootRepairStartedNamespaceProof, active_reblit_pending_boot_repair_plan_is_exact,
    capture_active_reblit_boot_repair_active_state, inspect_active_reblit_boot_repair_database,
    require_exact_active_reblit_boot_repair_active_state, require_exact_active_reblit_boot_repair_database,
    started_namespace_error_is_structural,
};

pub(in crate::client) enum UsrRollbackActiveReblitBootRepairUnverifiedAdmission<'system, 'reservation> {
    NotApplicable,
    Deferred,
    Ready(UsrRollbackActiveReblitBootRepairUnverifiedAuthority<'system, 'reservation>),
}

/// Exact read-only authority for only the Started -> Unverified journal edge.
pub(in crate::client) struct UsrRollbackActiveReblitBootRepairUnverifiedAuthority<'system, 'reservation> {
    installation: &'system Installation,
    state_db: &'system db::state::Database,
    record: TransitionRecord,
    database: ActiveReblitBootRepairDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: UsrRollbackActiveReblitBootRepairStartedNamespaceProof,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'system, 'reservation> UsrRollbackActiveReblitBootRepairUnverifiedAuthority<'system, 'reservation> {
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackActiveReblitBootRepairUnverifiedSeal,
        installation: &'system Installation,
        state_db: &'system db::state::Database,
        journal: &TransitionJournalStore,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitBootRepairUnverifiedAdmission<'system, 'reservation>,
        UsrRollbackActiveReblitBootRepairUnverifiedAuthorityError,
    > {
        if record.operation != Operation::ActiveReblit || record.phase != Phase::BootRepairStarted {
            return Ok(UsrRollbackActiveReblitBootRepairUnverifiedAdmission::NotApplicable);
        }
        if !active_reblit_pending_boot_repair_plan_is_exact(record, Phase::BootRepairStarted) {
            return Ok(UsrRollbackActiveReblitBootRepairUnverifiedAdmission::Deferred);
        }

        let journal_binding = journal.binding();
        if !journal.has_binding(&journal_binding) {
            return Err(UsrRollbackActiveReblitBootRepairUnverifiedAuthorityErrorKind::JournalBindingMismatch.into());
        }
        installation.revalidate_mutable_namespace()?;
        let database_before = match inspect_active_reblit_boot_repair_database(record, state_db)? {
            ActiveReblitBootRepairDatabaseInspection::Exact(database) => database,
            ActiveReblitBootRepairDatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActiveReblitBootRepairUnverifiedAdmission::Deferred);
            }
        };
        let active_state =
            capture_active_reblit_boot_repair_active_state(record, installation, active_state_reservation)?;
        let namespace_inspection =
            match UsrRollbackActiveReblitBootRepairStartedNamespaceInspection::begin(installation, journal, record) {
                Ok(inspection) => inspection,
                Err(source) if started_namespace_error_is_structural(&source) => {
                    return Ok(UsrRollbackActiveReblitBootRepairUnverifiedAdmission::Deferred);
                }
                Err(source) => return Err(source.into()),
            };
        let namespace = match namespace_inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(source) if started_namespace_error_is_structural(&source) => {
                return Ok(UsrRollbackActiveReblitBootRepairUnverifiedAdmission::Deferred);
            }
            Err(source) => return Err(source.into()),
        };
        let database_after = match inspect_active_reblit_boot_repair_database(record, state_db)? {
            ActiveReblitBootRepairDatabaseInspection::Exact(database) => database,
            ActiveReblitBootRepairDatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActiveReblitBootRepairUnverifiedAdmission::Deferred);
            }
        };
        require_exact_active_reblit_boot_repair_database(
            &database_before,
            ActiveReblitBootRepairDatabaseInspection::Exact(database_after),
        )?;
        require_exact_active_reblit_boot_repair_active_state(record, installation, &active_state)?;
        installation.revalidate_mutable_namespace()?;

        Ok(UsrRollbackActiveReblitBootRepairUnverifiedAdmission::Ready(Self {
            installation,
            state_db,
            record: record.clone(),
            database: database_before,
            active_state,
            namespace,
            journal_binding,
            _active_state_reservation: active_state_reservation,
        }))
    }

    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackActiveReblitBootRepairUnverifiedAuthorityError> {
        if !journal.has_binding(&self.journal_binding) {
            return Err(UsrRollbackActiveReblitBootRepairUnverifiedAuthorityErrorKind::JournalBindingMismatch.into());
        }
        let installation = self.installation;
        let state_db = self.state_db;
        installation.revalidate_mutable_namespace()?;
        let database_before = require_exact_active_reblit_boot_repair_database(
            &self.database,
            inspect_active_reblit_boot_repair_database(&self.record, state_db)?,
        )?;
        require_exact_active_reblit_boot_repair_active_state(&self.record, installation, &self.active_state)?;
        self.namespace.revalidate(installation, journal, &self.record)?;
        let database_after = require_exact_active_reblit_boot_repair_database(
            &self.database,
            inspect_active_reblit_boot_repair_database(&self.record, state_db)?,
        )?;
        require_exact_active_reblit_boot_repair_active_state(&self.record, installation, &self.active_state)?;
        if database_before != database_after
            || !active_reblit_pending_boot_repair_plan_is_exact(&self.record, Phase::BootRepairStarted)
        {
            return Err(UsrRollbackActiveReblitBootRepairUnverifiedAuthorityErrorKind::RouteEvidenceMismatch.into());
        }
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        self.installation
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.record
    }

    /// Derive the sole Started -> Unverified successor. Transition-journal
    /// validation fixes both its phase and `BootRollback::Unverified` update.
    pub(in crate::client) fn boot_repair_unverified_successor(&self) -> Result<TransitionRecord, CodecError> {
        self.record.rollback_successor(None)
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackActiveReblitBootRepairUnverifiedAuthorityError(
    #[from] UsrRollbackActiveReblitBootRepairUnverifiedAuthorityErrorKind,
);

impl From<ActiveReblitBootRepairEvidenceError> for UsrRollbackActiveReblitBootRepairUnverifiedAuthorityError {
    fn from(source: ActiveReblitBootRepairEvidenceError) -> Self {
        UsrRollbackActiveReblitBootRepairUnverifiedAuthorityErrorKind::Evidence(source).into()
    }
}

impl From<UsrRollbackActiveReblitBootRepairStartedNamespaceError>
    for UsrRollbackActiveReblitBootRepairUnverifiedAuthorityError
{
    fn from(source: UsrRollbackActiveReblitBootRepairStartedNamespaceError) -> Self {
        UsrRollbackActiveReblitBootRepairUnverifiedAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackActiveReblitBootRepairUnverifiedAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackActiveReblitBootRepairUnverifiedAuthorityErrorKind::Installation(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackActiveReblitBootRepairUnverifiedAuthorityErrorKind {
    #[error("ActiveReblit BootRepairStarted authority was paired with a different open journal store")]
    JournalBindingMismatch,
    #[error("exact ActiveReblit BootRepairStarted evidence changed")]
    RouteEvidenceMismatch,
    #[error("inspect exact ActiveReblit BootRepairStarted database, target-state, and active-state evidence")]
    Evidence(#[source] ActiveReblitBootRepairEvidenceError),
    #[error("revalidate exact ActiveReblit BootRepairStarted namespace evidence")]
    Namespace(#[source] UsrRollbackActiveReblitBootRepairStartedNamespaceError),
    #[error("revalidate retained mutable installation namespace around the Unverified route")]
    Installation(#[source] crate::installation::Error),
}
