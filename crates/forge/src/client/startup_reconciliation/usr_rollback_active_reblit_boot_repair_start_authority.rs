//! Read-only authority for the conservative ActiveReblit
//! `BootRepairRequired -> BootRepairStarted` journal edge.
//!
//! This authority recaptures exact database, active-state, preserved-wrapper,
//! installation, and journal-record-binding evidence. It exposes no boot,
//! filesystem, database, cleanup, or general journal effect.

use crate::{
    Installation, db,
    transition_journal::{
        CodecError, Operation, Phase, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::{ActiveStateReservation, ActiveStateSnapshot},
    startup_gate::UsrRollbackActiveReblitBootRepairStartSeal,
};
use super::{
    ActiveReblitBootRepairDatabaseEvidence, ActiveReblitBootRepairDatabaseInspection,
    ActiveReblitBootRepairEvidenceError, UsrRollbackActiveReblitBootRepairStartNamespaceError,
    UsrRollbackActiveReblitBootRepairStartNamespaceInspection,
    UsrRollbackActiveReblitBootRepairStartNamespaceProof,
    active_reblit_pending_boot_repair_plan_is_exact,
    capture_active_reblit_boot_repair_active_state,
    inspect_active_reblit_boot_repair_database,
    require_exact_active_reblit_boot_repair_active_state,
    require_exact_active_reblit_boot_repair_database,
    start_namespace_error_is_structural,
};

pub(in crate::client) enum UsrRollbackActiveReblitBootRepairStartAdmission<'system, 'reservation> {
    NotApplicable,
    Deferred,
    Ready(UsrRollbackActiveReblitBootRepairStartAuthority<'system, 'reservation>),
}

/// Exact non-effect authority for only the Required -> Started journal edge.
pub(in crate::client) struct UsrRollbackActiveReblitBootRepairStartAuthority<'system, 'reservation> {
    installation: &'system Installation,
    state_db: &'system db::state::Database,
    record: TransitionRecord,
    database: ActiveReblitBootRepairDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: UsrRollbackActiveReblitBootRepairStartNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'system, 'reservation> UsrRollbackActiveReblitBootRepairStartAuthority<'system, 'reservation> {
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackActiveReblitBootRepairStartSeal,
        installation: &'system Installation,
        state_db: &'system db::state::Database,
        journal: &TransitionJournalStore,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitBootRepairStartAdmission<'system, 'reservation>,
        UsrRollbackActiveReblitBootRepairStartAuthorityError,
    > {
        if record.operation != Operation::ActiveReblit || record.phase != Phase::BootRepairRequired {
            return Ok(UsrRollbackActiveReblitBootRepairStartAdmission::NotApplicable);
        }
        if !active_reblit_pending_boot_repair_plan_is_exact(record, Phase::BootRepairRequired) {
            return Ok(UsrRollbackActiveReblitBootRepairStartAdmission::Deferred);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding =
            journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        installation.revalidate_mutable_namespace()?;
        let database_before = match inspect_active_reblit_boot_repair_database(record, state_db)? {
            ActiveReblitBootRepairDatabaseInspection::Exact(database) => database,
            ActiveReblitBootRepairDatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActiveReblitBootRepairStartAdmission::Deferred);
            }
        };
        let active_state =
            capture_active_reblit_boot_repair_active_state(record, installation, active_state_reservation)?;
        let namespace_inspection = match UsrRollbackActiveReblitBootRepairStartNamespaceInspection::begin(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(inspection) => inspection,
            Err(source) if start_namespace_error_is_structural(&source) => {
                return Ok(UsrRollbackActiveReblitBootRepairStartAdmission::Deferred);
            }
            Err(source) => return Err(source.into()),
        };
        let namespace = match namespace_inspection.finish(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(namespace) => namespace,
            Err(source) if start_namespace_error_is_structural(&source) => {
                return Ok(UsrRollbackActiveReblitBootRepairStartAdmission::Deferred);
            }
            Err(source) => return Err(source.into()),
        };
        let database_after = match inspect_active_reblit_boot_repair_database(record, state_db)? {
            ActiveReblitBootRepairDatabaseInspection::Exact(database) => database,
            ActiveReblitBootRepairDatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActiveReblitBootRepairStartAdmission::Deferred);
            }
        };
        require_exact_active_reblit_boot_repair_database(
            &database_before,
            ActiveReblitBootRepairDatabaseInspection::Exact(database_after),
        )?;
        require_exact_active_reblit_boot_repair_active_state(record, installation, &active_state)?;
        installation.revalidate_mutable_namespace()?;
        require_journal_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;

        Ok(UsrRollbackActiveReblitBootRepairStartAdmission::Ready(Self {
            installation,
            state_db,
            record: record.clone(),
            database: database_before,
            active_state,
            namespace,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        }))
    }

    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackActiveReblitBootRepairStartAuthorityError> {
        require_journal_record_binding(
            self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        let installation = self.installation;
        installation.revalidate_mutable_namespace()?;
        let database_before = require_exact_active_reblit_boot_repair_database(
            &self.database,
            inspect_active_reblit_boot_repair_database(&self.record, self.state_db)?,
        )?;
        require_exact_active_reblit_boot_repair_active_state(&self.record, installation, &self.active_state)?;
        self.namespace.revalidate(
            installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        let database_after = require_exact_active_reblit_boot_repair_database(
            &self.database,
            inspect_active_reblit_boot_repair_database(&self.record, self.state_db)?,
        )?;
        require_exact_active_reblit_boot_repair_active_state(&self.record, installation, &self.active_state)?;
        if database_before != database_after
            || !active_reblit_pending_boot_repair_plan_is_exact(&self.record, Phase::BootRepairRequired)
        {
            return Err(UsrRollbackActiveReblitBootRepairStartAuthorityErrorKind::RouteEvidenceMismatch.into());
        }
        require_journal_record_binding(
            installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        self.installation
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.record
    }

    pub(in crate::client) fn boot_repair_started_successor(&self) -> Result<TransitionRecord, CodecError> {
        self.record.boot_repair_started_successor()
    }

    /// Consume this complete authority through the exact bound
    /// `BootRepairRequired -> BootRepairStarted` journal boundary.
    pub(in crate::client) fn advance_record_binding(
        self,
        journal: &TransitionJournalStore,
        next: &TransitionRecord,
    ) -> Result<TransitionJournalRecordBinding, UsrRollbackActiveReblitBootRepairStartRecordAdvanceError> {
        self.revalidate(journal)?;
        let cast = self.installation.retained_mutable_cast_directory()?;
        journal
            .advance_record_binding(cast, self.journal_record_binding, next)
            .map_err(UsrRollbackActiveReblitBootRepairStartRecordAdvanceError::Storage)
    }
}

fn require_journal_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), UsrRollbackActiveReblitBootRepairStartAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(
            UsrRollbackActiveReblitBootRepairStartAuthorityErrorKind::JournalRecordBindingMismatch.into(),
        );
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, binding, record)? {
        Ok(())
    } else {
        Err(UsrRollbackActiveReblitBootRepairStartAuthorityErrorKind::JournalRecordBindingMismatch.into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackActiveReblitBootRepairStartAuthorityError(
    #[from] UsrRollbackActiveReblitBootRepairStartAuthorityErrorKind,
);

impl From<ActiveReblitBootRepairEvidenceError> for UsrRollbackActiveReblitBootRepairStartAuthorityError {
    fn from(source: ActiveReblitBootRepairEvidenceError) -> Self {
        UsrRollbackActiveReblitBootRepairStartAuthorityErrorKind::Evidence(source).into()
    }
}

impl From<UsrRollbackActiveReblitBootRepairStartNamespaceError>
    for UsrRollbackActiveReblitBootRepairStartAuthorityError
{
    fn from(source: UsrRollbackActiveReblitBootRepairStartNamespaceError) -> Self {
        UsrRollbackActiveReblitBootRepairStartAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackActiveReblitBootRepairStartAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackActiveReblitBootRepairStartAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for UsrRollbackActiveReblitBootRepairStartAuthorityError {
    fn from(source: StorageError) -> Self {
        UsrRollbackActiveReblitBootRepairStartAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackActiveReblitBootRepairStartRecordAdvanceError {
    #[error("revalidate exact ActiveReblit BootRepairRequired authority before the bound journal advance")]
    Authority(#[from] UsrRollbackActiveReblitBootRepairStartAuthorityError),
    #[error("revalidate retained installation before the bound ActiveReblit BootRepairStarted advance")]
    Installation(#[from] crate::installation::Error),
    #[error("advance the exact bound ActiveReblit BootRepairRequired record")]
    Storage(#[source] StorageError),
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackActiveReblitBootRepairStartAuthorityErrorKind {
    #[error("ActiveReblit BootRepairRequired authority lost its exact journal record binding")]
    JournalRecordBindingMismatch,
    #[error("capture or revalidate the exact ActiveReblit BootRepairRequired journal record")]
    Journal(#[source] StorageError),
    #[error("exact ActiveReblit BootRepairRequired evidence changed")]
    RouteEvidenceMismatch,
    #[error("inspect exact ActiveReblit BootRepairRequired database, target-state, and active-state evidence")]
    Evidence(#[source] ActiveReblitBootRepairEvidenceError),
    #[error("revalidate exact ActiveReblit BootRepairRequired namespace evidence")]
    Namespace(#[source] UsrRollbackActiveReblitBootRepairStartNamespaceError),
    #[error("revalidate retained mutable installation namespace around the Started route")]
    Installation(#[source] crate::installation::Error),
}
