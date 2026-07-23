//! Sealed authority for exact ActivateArchived rollback finalization.
//!
//! Admission retains the exact two distinct cleared rows, immutable candidate
//! provenance, terminal archived canonical-slot topology, exact journal inode,
//! installation, and active-state reservation through a binding-first
//! DB -> namespace -> DB sandwich. Its only effect surface consumes that exact
//! binding into one operation-specific terminal deletion attempt.

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, CandidateOrigin, ForwardPhase, Operation, Phase, PreviousOrigin,
        RollbackAction, StorageError, TransitionJournalBinding, TransitionJournalRecordBinding,
        TransitionJournalRecordDeleteError, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackActivateArchivedFinalizationSeal,
};
use super::{
    DatabaseEvidence, InspectionError, UsrRollbackActivateArchivedFinalizationNamespaceError,
    UsrRollbackActivateArchivedFinalizationNamespaceInspection, UsrRollbackActivateArchivedFinalizationNamespaceProof,
    database_ownership_evidence_compatible, inspect_database, metadata_provenance_evidence_compatible,
};

pub(in crate::client) enum UsrRollbackActivateArchivedFinalizationAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Ready(UsrRollbackActivateArchivedFinalizationAuthority<'reservation>),
}

/// One-shot terminal authority. This type is intentionally not `Clone`.
pub(in crate::client) struct UsrRollbackActivateArchivedFinalizationAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: UsrRollbackActivateArchivedFinalizationDatabaseEvidence,
    namespace: UsrRollbackActivateArchivedFinalizationNamespaceProof,
    journal_binding: TransitionJournalBinding,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// One-shot evidence retained after the exact record binding has been
/// consumed by a deletion attempt. This type is intentionally not `Clone`.
pub(in crate::client) struct UsrRollbackActivateArchivedFinalizationAfterDeleteAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: UsrRollbackActivateArchivedFinalizationDatabaseEvidence,
    namespace: UsrRollbackActivateArchivedFinalizationNamespaceProof,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[derive(Debug, Eq, PartialEq)]
struct UsrRollbackActivateArchivedFinalizationDatabaseEvidence {
    context: DatabaseEvidence,
}

enum DatabaseInspection {
    Exact(UsrRollbackActivateArchivedFinalizationDatabaseEvidence),
    Incompatible(DatabaseEvidence),
}

impl<'reservation> UsrRollbackActivateArchivedFinalizationAuthority<'reservation> {
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackActivateArchivedFinalizationSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackActivateArchivedFinalizationAdmission<'reservation>,
        UsrRollbackActivateArchivedFinalizationAuthorityError,
    > {
        if record.operation != Operation::ActivateArchived || record.phase != Phase::RollbackComplete {
            return Ok(UsrRollbackActivateArchivedFinalizationAdmission::NotApplicable);
        }
        if record.rollback.is_none() || !activate_archived_finalization_plan_is_exact(record) {
            return Ok(UsrRollbackActivateArchivedFinalizationAdmission::Deferred);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_binding = journal.binding();
        let journal_record_binding =
            journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        installation.revalidate_mutable_namespace()?;

        let database_before = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActivateArchivedFinalizationAdmission::Deferred);
            }
        };
        let namespace_inspection =
            match UsrRollbackActivateArchivedFinalizationNamespaceInspection::begin(
                installation,
                journal,
                &journal_record_binding,
                record,
            ) {
                Ok(inspection) => inspection,
                Err(_) => return Ok(UsrRollbackActivateArchivedFinalizationAdmission::Deferred),
            };
        run_between_database_captures();
        let namespace = match namespace_inspection.finish(installation, journal, &journal_record_binding, record) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackActivateArchivedFinalizationAdmission::Deferred),
        };
        let database_after = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActivateArchivedFinalizationAdmission::Deferred);
            }
        };
        if database_before != database_after || !activate_archived_finalization_plan_is_exact(record) {
            return Ok(UsrRollbackActivateArchivedFinalizationAdmission::Deferred);
        }
        require_journal_record_binding(journal, installation, &journal_record_binding, record)?;

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackActivateArchivedFinalizationAdmission::Ready(Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database: database_after,
            namespace,
            journal_binding,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        }))
    }

    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackActivateArchivedFinalizationAuthorityError> {
        require_journal_record_binding(
            journal,
            &self.installation,
            &self.journal_record_binding,
            &self.record,
        )?;
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
        if database_before != database_after || !activate_archived_finalization_plan_is_exact(&self.record) {
            return Err(UsrRollbackActivateArchivedFinalizationAuthorityErrorKind::FinalizationEvidenceMismatch.into());
        }
        require_journal_record_binding(
            journal,
            &self.installation,
            &self.journal_record_binding,
            &self.record,
        )?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    pub(in crate::client) fn attempt_record_bound_delete(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        (
            Result<(), TransitionJournalRecordDeleteError>,
            UsrRollbackActivateArchivedFinalizationAfterDeleteAuthority<'reservation>,
        ),
        UsrRollbackActivateArchivedFinalizationAuthorityError,
    > {
        self.revalidate(journal)?;
        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_binding,
            journal_record_binding,
            _active_state_reservation,
        } = self;
        let delete = journal.delete_record_binding(
            installation.retained_mutable_cast_directory()?,
            journal_record_binding,
            &record,
        );
        Ok((
            delete,
            UsrRollbackActivateArchivedFinalizationAfterDeleteAuthority {
                installation,
                state_db,
                record,
                database,
                namespace,
                journal_binding,
                _active_state_reservation,
            },
        ))
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.record
    }
}

impl UsrRollbackActivateArchivedFinalizationAfterDeleteAuthority<'_> {
    pub(in crate::client) fn revalidate_after_journal_delete(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackActivateArchivedFinalizationAuthorityError> {
        if !journal.has_binding(&self.journal_binding) {
            return Err(UsrRollbackActivateArchivedFinalizationAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        let database_before =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        run_between_database_captures();
        self.namespace
            .revalidate_after_journal_delete(&self.installation, journal, &self.record)?;
        let database_after =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        if database_before != database_after || !activate_archived_finalization_plan_is_exact(&self.record) {
            return Err(UsrRollbackActivateArchivedFinalizationAuthorityErrorKind::FinalizationEvidenceMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn activate_archived_finalization_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    record.operation == Operation::ActivateArchived
        && record.phase == Phase::RollbackComplete
        && record.candidate.origin == CandidateOrigin::Archived
        && record.previous.origin == PreviousOrigin::ActiveState
        && record.candidate.id.is_some()
        && record.previous.id.is_some()
        && record.candidate.id != record.previous.id
        && matches!(
            (rollback.source, record.generation),
            (ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged, _)
                | (ForwardPhase::RootLinksComplete, 12)
        )
        && rollback.previous_archive == RollbackAction::NotRequired
        && matches!(
            rollback.usr_exchange,
            RollbackAction::Applied | RollbackAction::AlreadySatisfied
        )
        && matches!(
            rollback.candidate.action,
            RollbackAction::Applied | RollbackAction::AlreadySatisfied
        )
        && rollback.candidate.disposition == AbortDisposition::Rearchive
        && rollback.fresh_db == RollbackAction::NotRequired
        && rollback.boot == BootRollback::NotRequired
        && !rollback.external_effects_may_remain
}

fn require_journal_record_binding(
    journal: &TransitionJournalStore,
    installation: &Installation,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), UsrRollbackActivateArchivedFinalizationAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(UsrRollbackActivateArchivedFinalizationAuthorityErrorKind::JournalBindingMismatch.into());
    }
    installation.revalidate_mutable_namespace()?;
    let cast = installation.retained_mutable_cast_directory()?;
    if !journal.has_record_binding(cast, binding, record)? {
        return Err(UsrRollbackActivateArchivedFinalizationAuthorityErrorKind::JournalRecordBindingMismatch.into());
    }
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseInspection, UsrRollbackActivateArchivedFinalizationAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    if activate_archived_database_pair_is_exact(record, &context) {
        Ok(DatabaseInspection::Exact(
            UsrRollbackActivateArchivedFinalizationDatabaseEvidence { context },
        ))
    } else {
        Ok(DatabaseInspection::Incompatible(context))
    }
}

fn activate_archived_database_pair_is_exact(record: &TransitionRecord, evidence: &DatabaseEvidence) -> bool {
    if !database_ownership_evidence_compatible(record, evidence)
        || !metadata_provenance_evidence_compatible(record, evidence)
    {
        return false;
    }
    let (Some(candidate), Some(previous)) = (
        record.candidate.id.map(crate::state::Id::from),
        record.previous.id.map(crate::state::Id::from),
    ) else {
        return false;
    };
    if candidate == previous {
        return false;
    }
    matches!(
        evidence,
        DatabaseEvidence::ExistingCandidate {
            candidate: existing,
            provenance: Some(_),
            previous: Some(previous_existing),
        } if existing.state == candidate
            && existing.ownership == db::state::TransitionOwnership::Cleared
            && previous_existing.state == previous
            && previous_existing.ownership == db::state::TransitionOwnership::Cleared
    )
}

fn require_exact_database(
    expected: &UsrRollbackActivateArchivedFinalizationDatabaseEvidence,
    actual: DatabaseInspection,
) -> Result<
    UsrRollbackActivateArchivedFinalizationDatabaseEvidence,
    UsrRollbackActivateArchivedFinalizationAuthorityError,
> {
    match actual {
        DatabaseInspection::Exact(actual) if actual == *expected => Ok(actual),
        DatabaseInspection::Exact(_) => {
            Err(UsrRollbackActivateArchivedFinalizationAuthorityErrorKind::DatabaseChanged.into())
        }
        DatabaseInspection::Incompatible(evidence) => Err(
            UsrRollbackActivateArchivedFinalizationAuthorityErrorKind::DatabaseIncompatible {
                evidence: Box::new(evidence),
            }
            .into(),
        ),
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackActivateArchivedFinalizationAuthorityError(
    #[from] UsrRollbackActivateArchivedFinalizationAuthorityErrorKind,
);

impl From<InspectionError> for UsrRollbackActivateArchivedFinalizationAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackActivateArchivedFinalizationAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<UsrRollbackActivateArchivedFinalizationNamespaceError>
    for UsrRollbackActivateArchivedFinalizationAuthorityError
{
    fn from(source: UsrRollbackActivateArchivedFinalizationNamespaceError) -> Self {
        UsrRollbackActivateArchivedFinalizationAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackActivateArchivedFinalizationAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackActivateArchivedFinalizationAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for UsrRollbackActivateArchivedFinalizationAuthorityError {
    fn from(source: StorageError) -> Self {
        UsrRollbackActivateArchivedFinalizationAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackActivateArchivedFinalizationAuthorityErrorKind {
    #[error("ActivateArchived rollback-finalization authority was paired with a different open journal store")]
    JournalBindingMismatch,
    #[error("the exact retained ActivateArchived terminal journal inode no longer matches its captured binding")]
    JournalRecordBindingMismatch,
    #[error("authenticate the exact retained ActivateArchived terminal journal inode")]
    Journal(#[source] StorageError),
    #[error("exact ActivateArchived RollbackComplete evidence no longer authorizes finalization")]
    FinalizationEvidenceMismatch,
    #[error("inspect ActivateArchived rollback-finalization startup database context")]
    Inspection(#[source] InspectionError),
    #[error("revalidate the independent ActivateArchived rollback-finalization namespace proof")]
    Namespace(#[source] UsrRollbackActivateArchivedFinalizationNamespaceError),
    #[error("revalidate retained mutable installation namespace around ActivateArchived rollback finalization")]
    Installation(#[source] crate::installation::Error),
    #[error("ActivateArchived rollback-finalization database context is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error(
        "ActivateArchived rollback-finalization database evidence changed across its DB -> namespace -> DB sandwich"
    )]
    DatabaseChanged,
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_usr_rollback_activate_archived_finalization_database_captures(
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
