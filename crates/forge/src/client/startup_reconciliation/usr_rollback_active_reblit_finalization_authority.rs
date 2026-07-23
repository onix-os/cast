//! Sealed authority for exact ActiveReblit rollback finalization.
//!
//! This authority is deliberately disjoint from both the NewState terminal
//! authority and the earlier ActiveReblit completion route. Admission retains
//! the exact cleared existing-state row, immutable provenance, preserved
//! whole-wrapper namespace and wrapper index, exact journal inode,
//! installation, and active-state reservation through a binding-first
//! DB -> namespace -> DB sandwich. Its only effect surface consumes that exact
//! binding into one operation-specific terminal deletion attempt.

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, StorageError,
        TransitionJournalBinding, TransitionJournalRecordBinding, TransitionJournalRecordDeleteError,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackActiveReblitFinalizationSeal,
};
use super::{
    DatabaseEvidence, InspectionError, UsrRollbackActiveReblitFinalizationNamespaceError,
    UsrRollbackActiveReblitFinalizationNamespaceInspection, UsrRollbackActiveReblitFinalizationNamespaceProof,
    database_ownership_evidence_compatible, inspect_database, metadata_provenance_evidence_compatible,
};

/// Exact result of read-only ActiveReblit rollback-finalization admission.
pub(in crate::client) enum UsrRollbackActiveReblitFinalizationAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Ready(UsrRollbackActiveReblitFinalizationAuthority<'reservation>),
}

/// Retained evidence authorizing only one future terminal journal deletion.
///
/// This type is intentionally not `Clone`. Only the operation-specific
/// finalizer may consume it, and successful absence consumes its terminal
/// namespace proof.
pub(in crate::client) struct UsrRollbackActiveReblitFinalizationAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: UsrRollbackActiveReblitFinalizationDatabaseEvidence,
    namespace: UsrRollbackActiveReblitFinalizationNamespaceProof,
    journal_binding: TransitionJournalBinding,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// One-shot evidence retained after the exact record binding has been
/// consumed by a deletion attempt. This type is intentionally not `Clone`.
pub(in crate::client) struct UsrRollbackActiveReblitFinalizationAfterDeleteAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: UsrRollbackActiveReblitFinalizationDatabaseEvidence,
    namespace: UsrRollbackActiveReblitFinalizationNamespaceProof,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Exact existing-state observation bound to this finalizer. This type is
/// intentionally not `Clone`.
#[derive(Debug, Eq, PartialEq)]
struct UsrRollbackActiveReblitFinalizationDatabaseEvidence {
    context: DatabaseEvidence,
}

enum DatabaseInspection {
    Exact(UsrRollbackActiveReblitFinalizationDatabaseEvidence),
    Incompatible(DatabaseEvidence),
}

impl<'reservation> UsrRollbackActiveReblitFinalizationAuthority<'reservation> {
    /// Capture exact durable ActiveReblit `RollbackComplete` evidence without
    /// effects. Only the phase-specific startup child can construct the
    /// production seal.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackActiveReblitFinalizationSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitFinalizationAdmission<'reservation>,
        UsrRollbackActiveReblitFinalizationAuthorityError,
    > {
        if record.operation != Operation::ActiveReblit || record.phase != Phase::RollbackComplete {
            return Ok(UsrRollbackActiveReblitFinalizationAdmission::NotApplicable);
        }
        if record.rollback.is_none() || !active_reblit_finalization_plan_is_exact(record) {
            return Ok(UsrRollbackActiveReblitFinalizationAdmission::Deferred);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_binding = journal.binding();
        let journal_record_binding =
            journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        installation.revalidate_mutable_namespace()?;

        let database_before = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActiveReblitFinalizationAdmission::Deferred);
            }
        };
        let namespace_inspection =
            match UsrRollbackActiveReblitFinalizationNamespaceInspection::begin(
                installation,
                journal,
                &journal_record_binding,
                record,
            ) {
                Ok(inspection) => inspection,
                Err(_) => return Ok(UsrRollbackActiveReblitFinalizationAdmission::Deferred),
            };
        run_between_database_captures();
        let namespace = match namespace_inspection.finish(installation, journal, &journal_record_binding, record) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackActiveReblitFinalizationAdmission::Deferred),
        };
        let database_after = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActiveReblitFinalizationAdmission::Deferred);
            }
        };
        if database_before != database_after || !active_reblit_finalization_plan_is_exact(record) {
            return Ok(UsrRollbackActiveReblitFinalizationAdmission::Deferred);
        }
        require_journal_record_binding(journal, installation, &journal_record_binding, record)?;

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackActiveReblitFinalizationAdmission::Ready(Self {
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

    /// Revalidate the exact binding-first DB -> namespace -> DB sandwich.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackActiveReblitFinalizationAuthorityError> {
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
        if database_before != database_after || !active_reblit_finalization_plan_is_exact(&self.record) {
            return Err(UsrRollbackActiveReblitFinalizationAuthorityErrorKind::FinalizationEvidenceMismatch.into());
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
            UsrRollbackActiveReblitFinalizationAfterDeleteAuthority<'reservation>,
        ),
        UsrRollbackActiveReblitFinalizationAuthorityError,
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
            UsrRollbackActiveReblitFinalizationAfterDeleteAuthority {
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

    #[cfg(test)]
    pub(in crate::client) fn wrapper_index(&self) -> usize {
        self.namespace.wrapper_index()
    }
}

impl UsrRollbackActiveReblitFinalizationAfterDeleteAuthority<'_> {
    /// Consume this one-shot authority after terminal deletion and prove that
    /// its independent database and namespace evidence still describes the
    /// exact same ActiveReblit rollback result.
    pub(in crate::client) fn revalidate_after_journal_delete(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackActiveReblitFinalizationAuthorityError> {
        if !journal.has_binding(&self.journal_binding) {
            return Err(UsrRollbackActiveReblitFinalizationAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        let database_before =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        run_between_database_captures();
        self.namespace
            .revalidate_after_journal_delete(&self.installation, journal, &self.record)?;
        let database_after =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        if database_before != database_after || !active_reblit_finalization_plan_is_exact(&self.record) {
            return Err(UsrRollbackActiveReblitFinalizationAuthorityErrorKind::FinalizationEvidenceMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

/// Exact narrow plan accepted by ActiveReblit terminal finalization.
fn active_reblit_finalization_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    record.operation == Operation::ActiveReblit
        && record.phase == Phase::RollbackComplete
        && record.candidate.id.is_some()
        && record.candidate.id == record.previous.id
        && matches!(
            (rollback.source, record.generation),
            (ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged, _)
                | (ForwardPhase::RootLinksComplete, 14)
                | (ForwardPhase::SystemTriggersStarted, 15)
                | (ForwardPhase::SystemTriggersComplete, 16)
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
        && rollback.candidate.disposition == AbortDisposition::Quarantine
        && rollback.fresh_db == RollbackAction::NotRequired
        && rollback.boot == BootRollback::NotRequired
        && rollback.external_effects_may_remain
}

fn require_journal_record_binding(
    journal: &TransitionJournalStore,
    installation: &Installation,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), UsrRollbackActiveReblitFinalizationAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(UsrRollbackActiveReblitFinalizationAuthorityErrorKind::JournalBindingMismatch.into());
    }
    installation.revalidate_mutable_namespace()?;
    let cast = installation.retained_mutable_cast_directory()?;
    if !journal.has_record_binding(cast, binding, record)? {
        return Err(UsrRollbackActiveReblitFinalizationAuthorityErrorKind::JournalRecordBindingMismatch.into());
    }
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

/// Inspect the exact existing-state evidence around the general startup
/// context so no candidate row, ownership, or provenance observation can be
/// paired with a different database moment.
fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseInspection, UsrRollbackActiveReblitFinalizationAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    if active_reblit_database_pair_is_exact(record, &context) {
        Ok(DatabaseInspection::Exact(
            UsrRollbackActiveReblitFinalizationDatabaseEvidence { context },
        ))
    } else {
        Ok(DatabaseInspection::Incompatible(context))
    }
}

fn active_reblit_database_pair_is_exact(record: &TransitionRecord, evidence: &DatabaseEvidence) -> bool {
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
    if candidate != previous {
        return false;
    }
    matches!(
        evidence,
        DatabaseEvidence::ExistingCandidate {
            candidate: existing,
            provenance: Some(_),
            previous: None,
        } if existing.state == candidate
            && existing.ownership == db::state::TransitionOwnership::Cleared
    )
}

fn require_exact_database(
    expected: &UsrRollbackActiveReblitFinalizationDatabaseEvidence,
    actual: DatabaseInspection,
) -> Result<UsrRollbackActiveReblitFinalizationDatabaseEvidence, UsrRollbackActiveReblitFinalizationAuthorityError> {
    match actual {
        DatabaseInspection::Exact(actual) if actual == *expected => Ok(actual),
        DatabaseInspection::Exact(_) => {
            Err(UsrRollbackActiveReblitFinalizationAuthorityErrorKind::DatabaseChanged.into())
        }
        DatabaseInspection::Incompatible(evidence) => Err(
            UsrRollbackActiveReblitFinalizationAuthorityErrorKind::DatabaseIncompatible {
                evidence: Box::new(evidence),
            }
            .into(),
        ),
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackActiveReblitFinalizationAuthorityError(
    #[from] UsrRollbackActiveReblitFinalizationAuthorityErrorKind,
);

impl From<InspectionError> for UsrRollbackActiveReblitFinalizationAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackActiveReblitFinalizationAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<UsrRollbackActiveReblitFinalizationNamespaceError> for UsrRollbackActiveReblitFinalizationAuthorityError {
    fn from(source: UsrRollbackActiveReblitFinalizationNamespaceError) -> Self {
        UsrRollbackActiveReblitFinalizationAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackActiveReblitFinalizationAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackActiveReblitFinalizationAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for UsrRollbackActiveReblitFinalizationAuthorityError {
    fn from(source: StorageError) -> Self {
        UsrRollbackActiveReblitFinalizationAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackActiveReblitFinalizationAuthorityErrorKind {
    #[error("ActiveReblit rollback-finalization authority was paired with a different open journal store")]
    JournalBindingMismatch,
    #[error("the exact retained ActiveReblit terminal journal inode no longer matches its captured binding")]
    JournalRecordBindingMismatch,
    #[error("authenticate the exact retained ActiveReblit terminal journal inode")]
    Journal(#[source] StorageError),
    #[error("exact ActiveReblit RollbackComplete evidence no longer authorizes finalization")]
    FinalizationEvidenceMismatch,
    #[error("inspect ActiveReblit rollback-finalization startup database context")]
    Inspection(#[source] InspectionError),
    #[error("revalidate the independent ActiveReblit rollback-finalization namespace proof")]
    Namespace(#[source] UsrRollbackActiveReblitFinalizationNamespaceError),
    #[error("revalidate retained mutable installation namespace around ActiveReblit rollback finalization")]
    Installation(#[source] crate::installation::Error),
    #[error("ActiveReblit rollback-finalization database context is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("ActiveReblit rollback-finalization database evidence changed across its DB -> namespace -> DB sandwich")]
    DatabaseChanged,
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_usr_rollback_active_reblit_finalization_database_captures(
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
