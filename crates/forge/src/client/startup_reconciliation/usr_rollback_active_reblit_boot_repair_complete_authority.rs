//! Phase-specific authority for routing a verified ActiveReblit boot repair to
//! rollback completion.
//!
//! This authority is captured only from `BootRepairComplete`. It independently
//! retains the complete target state, live active selection, database and
//! metadata provenance, preserved-wrapper namespace, and exact journal
//! binding. It exposes only the journal successor and owns no boot, database,
//! namespace, cleanup, retry, or finalization effect.

use crate::{
    Installation, db,
    transition_journal::{
        CodecError, Operation, Phase, TransitionJournalBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::{ActiveStateReservation, ActiveStateSnapshot},
    startup_gate::UsrRollbackActiveReblitBootRepairCompleteSeal,
};
use super::{
    ActiveReblitBootRepairDatabaseEvidence, ActiveReblitBootRepairDatabaseInspection,
    ActiveReblitBootRepairEvidenceError, UsrRollbackActiveReblitBootRepairCompleteNamespaceError,
    UsrRollbackActiveReblitBootRepairCompleteNamespaceInspection,
    UsrRollbackActiveReblitBootRepairCompleteNamespaceProof, active_reblit_completed_boot_repair_plan_is_exact,
    capture_active_reblit_boot_repair_active_state, complete_namespace_error_is_structural,
    inspect_active_reblit_boot_repair_database,
    require_exact_active_reblit_boot_repair_active_state, require_exact_active_reblit_boot_repair_database,
};

pub(in crate::client) enum UsrRollbackActiveReblitBootRepairCompleteAdmission<'system, 'reservation> {
    NotApplicable,
    Deferred,
    Ready(UsrRollbackActiveReblitBootRepairCompleteAuthority<'system, 'reservation>),
}

/// Exact read-only authority for only the Complete-to-RollbackComplete edge.
pub(in crate::client) struct UsrRollbackActiveReblitBootRepairCompleteAuthority<'system, 'reservation> {
    installation: &'system Installation,
    state_db: &'system db::state::Database,
    record: TransitionRecord,
    database: ActiveReblitBootRepairDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: UsrRollbackActiveReblitBootRepairCompleteNamespaceProof,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'system, 'reservation> UsrRollbackActiveReblitBootRepairCompleteAuthority<'system, 'reservation> {
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackActiveReblitBootRepairCompleteSeal,
        installation: &'system Installation,
        state_db: &'system db::state::Database,
        journal: &TransitionJournalStore,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitBootRepairCompleteAdmission<'system, 'reservation>,
        UsrRollbackActiveReblitBootRepairCompleteAuthorityError,
    > {
        if record.operation != Operation::ActiveReblit || record.phase != Phase::BootRepairComplete {
            return Ok(UsrRollbackActiveReblitBootRepairCompleteAdmission::NotApplicable);
        }
        if !active_reblit_completed_boot_repair_plan_is_exact(record) {
            return Ok(UsrRollbackActiveReblitBootRepairCompleteAdmission::Deferred);
        }

        let journal_binding = journal.binding();
        if !journal.has_binding(&journal_binding) {
            return Err(UsrRollbackActiveReblitBootRepairCompleteAuthorityErrorKind::JournalBindingMismatch.into());
        }
        installation.revalidate_mutable_namespace()?;
        let database_before = match inspect_active_reblit_boot_repair_database(record, state_db)? {
            ActiveReblitBootRepairDatabaseInspection::Exact(database) => database,
            ActiveReblitBootRepairDatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActiveReblitBootRepairCompleteAdmission::Deferred);
            }
        };
        let active_state =
            capture_active_reblit_boot_repair_active_state(record, installation, active_state_reservation)?;
        let namespace_inspection =
            match UsrRollbackActiveReblitBootRepairCompleteNamespaceInspection::begin(installation, journal, record) {
                Ok(inspection) => inspection,
                Err(source) if complete_namespace_error_is_structural(&source) => {
                    return Ok(UsrRollbackActiveReblitBootRepairCompleteAdmission::Deferred);
                }
                Err(source) => return Err(source.into()),
            };
        run_between_database_captures();
        let namespace = match namespace_inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(source) if complete_namespace_error_is_structural(&source) => {
                return Ok(UsrRollbackActiveReblitBootRepairCompleteAdmission::Deferred);
            }
            Err(source) => return Err(source.into()),
        };
        let database_after = match inspect_active_reblit_boot_repair_database(record, state_db)? {
            ActiveReblitBootRepairDatabaseInspection::Exact(database) => database,
            ActiveReblitBootRepairDatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActiveReblitBootRepairCompleteAdmission::Deferred);
            }
        };
        require_exact_active_reblit_boot_repair_database(
            &database_before,
            ActiveReblitBootRepairDatabaseInspection::Exact(database_after),
        )?;
        require_exact_active_reblit_boot_repair_active_state(record, installation, &active_state)?;
        installation.revalidate_mutable_namespace()?;

        Ok(UsrRollbackActiveReblitBootRepairCompleteAdmission::Ready(Self {
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
    ) -> Result<(), UsrRollbackActiveReblitBootRepairCompleteAuthorityError> {
        if !journal.has_binding(&self.journal_binding) {
            return Err(UsrRollbackActiveReblitBootRepairCompleteAuthorityErrorKind::JournalBindingMismatch.into());
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
        if database_before != database_after || !active_reblit_completed_boot_repair_plan_is_exact(&self.record) {
            return Err(UsrRollbackActiveReblitBootRepairCompleteAuthorityErrorKind::RouteEvidenceMismatch.into());
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

    pub(in crate::client) fn rollback_complete_successor(&self) -> Result<TransitionRecord, CodecError> {
        self.record.boot_repair_rollback_complete_successor()
    }

    #[cfg(test)]
    pub(in crate::client) fn wrapper_index(&self) -> usize {
        self.namespace.wrapper_index()
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackActiveReblitBootRepairCompleteAuthorityError(
    #[from] UsrRollbackActiveReblitBootRepairCompleteAuthorityErrorKind,
);

impl From<ActiveReblitBootRepairEvidenceError> for UsrRollbackActiveReblitBootRepairCompleteAuthorityError {
    fn from(source: ActiveReblitBootRepairEvidenceError) -> Self {
        UsrRollbackActiveReblitBootRepairCompleteAuthorityErrorKind::Evidence(source).into()
    }
}

impl From<UsrRollbackActiveReblitBootRepairCompleteNamespaceError>
    for UsrRollbackActiveReblitBootRepairCompleteAuthorityError
{
    fn from(source: UsrRollbackActiveReblitBootRepairCompleteNamespaceError) -> Self {
        UsrRollbackActiveReblitBootRepairCompleteAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackActiveReblitBootRepairCompleteAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackActiveReblitBootRepairCompleteAuthorityErrorKind::Installation(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackActiveReblitBootRepairCompleteAuthorityErrorKind {
    #[error("ActiveReblit BootRepairComplete authority was paired with a different open journal store")]
    JournalBindingMismatch,
    #[error("exact ActiveReblit BootRepairComplete evidence changed")]
    RouteEvidenceMismatch,
    #[error("inspect exact ActiveReblit BootRepairComplete database, target-state, and active-state evidence")]
    Evidence(#[source] ActiveReblitBootRepairEvidenceError),
    #[error("revalidate exact ActiveReblit BootRepairComplete namespace evidence")]
    Namespace(#[source] UsrRollbackActiveReblitBootRepairCompleteNamespaceError),
    #[error("revalidate retained mutable installation namespace around successful boot-repair routing")]
    Installation(#[source] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_usr_rollback_active_reblit_boot_repair_complete_database_captures(
    hook: impl FnOnce() + 'static,
) {
    BETWEEN_DATABASE_CAPTURES.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_between_database_captures() {
    BETWEEN_DATABASE_CAPTURES.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_between_database_captures() {}
