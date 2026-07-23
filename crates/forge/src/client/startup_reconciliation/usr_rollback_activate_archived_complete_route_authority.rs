//! Sealed read-only authority for routing an ActivateArchived
//! `CandidatePreserved` record to rollback completion.
//!
//! Admission retains the exact two cleared existing-state rows, immutable
//! candidate provenance, preserved canonical-slot namespace, exact journal-
//! record binding,
//! installation capability, and active-state reservation. It exposes no
//! database, namespace, journal, trigger, cleanup, retry, or finalization
//! effect.

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, CandidateOrigin, ForwardPhase, Operation, Phase, PreviousOrigin,
        RollbackAction, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackActivateArchivedCompleteRouteSeal,
};
use super::{
    DatabaseEvidence, InspectionError, UsrRollbackActivateArchivedCompleteRouteNamespaceError,
    UsrRollbackActivateArchivedCompleteRouteNamespaceInspection,
    UsrRollbackActivateArchivedCompleteRouteNamespaceProof, database_ownership_evidence_compatible, inspect_database,
    metadata_provenance_evidence_compatible,
};

pub(in crate::client) enum UsrRollbackActivateArchivedCompleteRouteAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Ready(UsrRollbackActivateArchivedCompleteRouteAuthority<'reservation>),
}

pub(in crate::client) struct UsrRollbackActivateArchivedCompleteRouteAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: UsrRollbackActivateArchivedCompleteRouteDatabaseEvidence,
    namespace: UsrRollbackActivateArchivedCompleteRouteNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[derive(Debug, Eq, PartialEq)]
struct UsrRollbackActivateArchivedCompleteRouteDatabaseEvidence {
    context: DatabaseEvidence,
}

enum DatabaseInspection {
    Exact(UsrRollbackActivateArchivedCompleteRouteDatabaseEvidence),
    Incompatible(DatabaseEvidence),
}

impl<'reservation> UsrRollbackActivateArchivedCompleteRouteAuthority<'reservation> {
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackActivateArchivedCompleteRouteSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackActivateArchivedCompleteRouteAdmission<'reservation>,
        UsrRollbackActivateArchivedCompleteRouteAuthorityError,
    > {
        if record.operation != Operation::ActivateArchived || record.phase != Phase::CandidatePreserved {
            return Ok(UsrRollbackActivateArchivedCompleteRouteAdmission::NotApplicable);
        }
        if record.rollback.is_none() || !activate_archived_complete_route_plan_is_exact(record) {
            return Ok(UsrRollbackActivateArchivedCompleteRouteAdmission::Deferred);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding =
            journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        installation.revalidate_mutable_namespace()?;

        let database_before = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActivateArchivedCompleteRouteAdmission::Deferred);
            }
        };
        let namespace_inspection =
            match UsrRollbackActivateArchivedCompleteRouteNamespaceInspection::begin(
                installation,
                journal,
                &journal_record_binding,
                record,
            ) {
                Ok(inspection) => inspection,
                Err(_) => return Ok(UsrRollbackActivateArchivedCompleteRouteAdmission::Deferred),
            };
        run_between_database_captures();
        let namespace = match namespace_inspection.finish(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackActivateArchivedCompleteRouteAdmission::Deferred),
        };
        let database_after = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActivateArchivedCompleteRouteAdmission::Deferred);
            }
        };
        if database_before != database_after || !activate_archived_complete_route_plan_is_exact(record) {
            return Ok(UsrRollbackActivateArchivedCompleteRouteAdmission::Deferred);
        }

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        require_journal_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackActivateArchivedCompleteRouteAdmission::Ready(Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database: database_after,
            namespace,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        }))
    }

    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackActivateArchivedCompleteRouteAuthorityError> {
        require_journal_record_binding(
            &self.installation,
            journal,
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
        if database_before != database_after || !activate_archived_complete_route_plan_is_exact(&self.record) {
            return Err(UsrRollbackActivateArchivedCompleteRouteAuthorityErrorKind::RouteEvidenceMismatch.into());
        }
        require_journal_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.installation
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.record
    }

    /// Consume the complete authority through the exact bound
    /// predecessor-to-successor journal boundary.
    pub(in crate::client) fn advance_record_binding(
        self,
        journal: &TransitionJournalStore,
        next: &TransitionRecord,
    ) -> Result<TransitionJournalRecordBinding, UsrRollbackActivateArchivedCompleteRouteRecordAdvanceError> {
        self.revalidate(journal)?;
        let cast = self.installation.retained_mutable_cast_directory()?;
        journal
            .advance_record_binding(cast, self.journal_record_binding, next)
            .map_err(UsrRollbackActivateArchivedCompleteRouteRecordAdvanceError::Storage)
    }
}

fn require_journal_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), UsrRollbackActivateArchivedCompleteRouteAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(
            UsrRollbackActivateArchivedCompleteRouteAuthorityErrorKind::JournalRecordBindingMismatch.into(),
        );
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, binding, record)? {
        Ok(())
    } else {
        Err(UsrRollbackActivateArchivedCompleteRouteAuthorityErrorKind::JournalRecordBindingMismatch.into())
    }
}

fn activate_archived_complete_route_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    record.operation == Operation::ActivateArchived
        && record.phase == Phase::CandidatePreserved
        && record.candidate.origin == CandidateOrigin::Archived
        && record.previous.origin == PreviousOrigin::ActiveState
        && record.candidate.id.is_some()
        && record.previous.id.is_some()
        && record.candidate.id != record.previous.id
        && matches!(
            rollback.source,
            ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged | ForwardPhase::RootLinksComplete
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

fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseInspection, UsrRollbackActivateArchivedCompleteRouteAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    if activate_archived_database_pair_is_exact(record, &context) {
        Ok(DatabaseInspection::Exact(
            UsrRollbackActivateArchivedCompleteRouteDatabaseEvidence { context },
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
    expected: &UsrRollbackActivateArchivedCompleteRouteDatabaseEvidence,
    actual: DatabaseInspection,
) -> Result<
    UsrRollbackActivateArchivedCompleteRouteDatabaseEvidence,
    UsrRollbackActivateArchivedCompleteRouteAuthorityError,
> {
    match actual {
        DatabaseInspection::Exact(actual) if actual == *expected => Ok(actual),
        DatabaseInspection::Exact(_) => {
            Err(UsrRollbackActivateArchivedCompleteRouteAuthorityErrorKind::DatabaseChanged.into())
        }
        DatabaseInspection::Incompatible(evidence) => Err(
            UsrRollbackActivateArchivedCompleteRouteAuthorityErrorKind::DatabaseIncompatible {
                evidence: Box::new(evidence),
            }
            .into(),
        ),
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackActivateArchivedCompleteRouteAuthorityError(
    #[from] UsrRollbackActivateArchivedCompleteRouteAuthorityErrorKind,
);

impl From<InspectionError> for UsrRollbackActivateArchivedCompleteRouteAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackActivateArchivedCompleteRouteAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<UsrRollbackActivateArchivedCompleteRouteNamespaceError>
    for UsrRollbackActivateArchivedCompleteRouteAuthorityError
{
    fn from(source: UsrRollbackActivateArchivedCompleteRouteNamespaceError) -> Self {
        UsrRollbackActivateArchivedCompleteRouteAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackActivateArchivedCompleteRouteAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackActivateArchivedCompleteRouteAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for UsrRollbackActivateArchivedCompleteRouteAuthorityError {
    fn from(source: StorageError) -> Self {
        UsrRollbackActivateArchivedCompleteRouteAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackActivateArchivedCompleteRouteRecordAdvanceError {
    #[error("revalidate exact ActivateArchived rollback-completion route authority before the bound journal advance")]
    Authority(#[from] UsrRollbackActivateArchivedCompleteRouteAuthorityError),
    #[error("revalidate retained installation before the bound ActivateArchived rollback-completion route advance")]
    Installation(#[from] crate::installation::Error),
    #[error("advance the exact bound ActivateArchived rollback-completion route record")]
    Storage(#[source] StorageError),
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackActivateArchivedCompleteRouteAuthorityErrorKind {
    #[error("ActivateArchived rollback-completion authority lost its exact journal record binding")]
    JournalRecordBindingMismatch,
    #[error("capture or revalidate the exact ActivateArchived rollback-completion journal record")]
    Journal(#[source] StorageError),
    #[error("exact ActivateArchived CandidatePreserved evidence no longer selects rollback completion")]
    RouteEvidenceMismatch,
    #[error("inspect ActivateArchived rollback-completion startup database context")]
    Inspection(#[source] InspectionError),
    #[error("revalidate the independent ActivateArchived rollback-completion namespace proof")]
    Namespace(#[source] UsrRollbackActivateArchivedCompleteRouteNamespaceError),
    #[error("revalidate retained mutable installation namespace around ActivateArchived rollback-completion routing")]
    Installation(#[source] crate::installation::Error),
    #[error("ActivateArchived rollback-completion database context is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("ActivateArchived rollback-completion database evidence changed across its DB -> namespace -> DB sandwich")]
    DatabaseChanged,
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_usr_rollback_activate_archived_complete_route_database_captures(
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
