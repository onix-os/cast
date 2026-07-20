//! Sealed read-only authority for routing a preserved NewState candidate.

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, StorageError,
        TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackFreshDbInvalidationRouteSeal,
};
use super::{
    DatabaseEvidence, InspectionError, UsrRollbackFreshDbInvalidationRouteNamespaceError,
    UsrRollbackFreshDbInvalidationRouteNamespaceInspection, UsrRollbackFreshDbInvalidationRouteNamespaceProof,
    database_ownership_evidence_compatible, inspect_database, metadata_provenance_evidence_compatible,
};

/// Exact result of read-only fresh-database invalidation route admission.
pub(in crate::client) enum UsrRollbackFreshDbInvalidationRouteAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Ready(UsrRollbackFreshDbInvalidationRouteAuthority<'reservation>),
}

/// Retained evidence authorizing only the next journal route decision.
///
/// No database, namespace, journal, trigger, cleanup, or retry effect is
/// exposed by this authority.
pub(in crate::client) struct UsrRollbackFreshDbInvalidationRouteAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackFreshDbInvalidationRouteNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> UsrRollbackFreshDbInvalidationRouteAuthority<'reservation> {
    /// Capture the exact durable `CandidatePreserved` prefix without effects.
    /// Only the phase-specific writer-first startup child can construct the
    /// production route seal.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackFreshDbInvalidationRouteSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<
        UsrRollbackFreshDbInvalidationRouteAdmission<'reservation>,
        UsrRollbackFreshDbInvalidationRouteAuthorityError,
    > {
        if record.phase != Phase::CandidatePreserved || record.operation != Operation::NewState {
            return Ok(UsrRollbackFreshDbInvalidationRouteAdmission::NotApplicable);
        }
        let Some(rollback) = record.rollback.as_ref() else {
            return Ok(UsrRollbackFreshDbInvalidationRouteAdmission::Deferred);
        };
        if !matches!(
            rollback.source,
            ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged | ForwardPhase::RootLinksComplete
        ) {
            return Ok(UsrRollbackFreshDbInvalidationRouteAdmission::NotApplicable);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding =
            journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        installation.revalidate_mutable_namespace()?;
        let namespace_inspection = match UsrRollbackFreshDbInvalidationRouteNamespaceInspection::begin(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(inspection) => inspection,
            Err(_) => return Ok(UsrRollbackFreshDbInvalidationRouteAdmission::Deferred),
        };
        let database = inspect_database(record, state_db, initial_in_flight)?;
        if !database_is_exact(record, &database) || !route_plan_is_exact(record) {
            return Ok(UsrRollbackFreshDbInvalidationRouteAdmission::Deferred);
        }

        run_between_initial_database_captures();
        let in_flight_after = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
        let database_after = inspect_database(record, state_db, in_flight_after)?;
        if !database_is_exact(record, &database_after) || database != database_after {
            return Ok(UsrRollbackFreshDbInvalidationRouteAdmission::Deferred);
        }
        let namespace = match namespace_inspection.finish(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackFreshDbInvalidationRouteAdmission::Deferred),
        };

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        require_journal_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackFreshDbInvalidationRouteAdmission::Ready(Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        }))
    }

    /// Revalidate the exact DB -> namespace -> DB sandwich. The exact journal
    /// record binding is deliberately the first retained-evidence observation.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackFreshDbInvalidationRouteAuthorityError> {
        require_journal_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        self.installation.revalidate_mutable_namespace()?;
        let database_before = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_before)?;
        self.namespace.revalidate(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        let database_after = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_after)?;
        if !route_plan_is_exact(&self.record) {
            return Err(UsrRollbackFreshDbInvalidationRouteAuthorityErrorKind::RouteEvidenceMismatch.into());
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

    /// Consume this complete authority through the exact bound journal
    /// predecessor-to-successor boundary.
    pub(in crate::client) fn advance_record_binding(
        self,
        journal: &TransitionJournalStore,
        next: &TransitionRecord,
    ) -> Result<TransitionJournalRecordBinding, UsrRollbackFreshDbInvalidationRouteRecordAdvanceError> {
        self.revalidate(journal)?;
        let cast = self.installation.retained_mutable_cast_directory()?;
        journal
            .advance_record_binding(cast, self.journal_record_binding, next)
            .map_err(UsrRollbackFreshDbInvalidationRouteRecordAdvanceError::Storage)
    }
}

fn require_journal_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), UsrRollbackFreshDbInvalidationRouteAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(UsrRollbackFreshDbInvalidationRouteAuthorityErrorKind::JournalRecordBindingMismatch.into());
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, binding, record)? {
        Ok(())
    } else {
        Err(UsrRollbackFreshDbInvalidationRouteAuthorityErrorKind::JournalRecordBindingMismatch.into())
    }
}

fn route_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    record.operation == Operation::NewState
        && record.phase == Phase::CandidatePreserved
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
        && rollback.candidate.disposition == AbortDisposition::Quarantine
        && rollback.fresh_db == RollbackAction::Pending
        && rollback.boot == BootRollback::NotRequired
        && rollback.external_effects_may_remain
}

fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseEvidence, UsrRollbackFreshDbInvalidationRouteAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let evidence = inspect_database(record, state_db, in_flight)?;
    if database_is_exact(record, &evidence) {
        Ok(evidence)
    } else {
        Err(
            UsrRollbackFreshDbInvalidationRouteAuthorityErrorKind::DatabaseIncompatible {
                evidence: Box::new(evidence),
            }
            .into(),
        )
    }
}

fn database_is_exact(record: &TransitionRecord, evidence: &DatabaseEvidence) -> bool {
    matches!(
        evidence,
        DatabaseEvidence::CandidateOwnership {
            ownership: db::state::TransitionOwnership::Matching,
            provenance: Some(_),
            ..
        }
    ) && database_ownership_evidence_compatible(record, evidence)
        && metadata_provenance_evidence_compatible(record, evidence)
}

fn require_exact_database(
    expected: &DatabaseEvidence,
    actual: DatabaseEvidence,
) -> Result<(), UsrRollbackFreshDbInvalidationRouteAuthorityError> {
    if *expected == actual {
        Ok(())
    } else {
        Err(UsrRollbackFreshDbInvalidationRouteAuthorityErrorKind::DatabaseChanged {
            expected: Box::new(expected.clone()),
            actual: Box::new(actual),
        }
        .into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackFreshDbInvalidationRouteAuthorityError(
    #[from] UsrRollbackFreshDbInvalidationRouteAuthorityErrorKind,
);

impl From<InspectionError> for UsrRollbackFreshDbInvalidationRouteAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackFreshDbInvalidationRouteAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<UsrRollbackFreshDbInvalidationRouteNamespaceError> for UsrRollbackFreshDbInvalidationRouteAuthorityError {
    fn from(source: UsrRollbackFreshDbInvalidationRouteNamespaceError) -> Self {
        UsrRollbackFreshDbInvalidationRouteAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackFreshDbInvalidationRouteAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackFreshDbInvalidationRouteAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for UsrRollbackFreshDbInvalidationRouteAuthorityError {
    fn from(source: StorageError) -> Self {
        UsrRollbackFreshDbInvalidationRouteAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackFreshDbInvalidationRouteRecordAdvanceError {
    #[error("revalidate exact fresh-database invalidation route authority before the bound journal advance")]
    Authority(#[from] UsrRollbackFreshDbInvalidationRouteAuthorityError),
    #[error("revalidate retained installation before the bound fresh-database invalidation route advance")]
    Installation(#[from] crate::installation::Error),
    #[error("advance the exact bound fresh-database invalidation route record")]
    Storage(#[source] StorageError),
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackFreshDbInvalidationRouteAuthorityErrorKind {
    #[error("fresh-database invalidation route authority lost its exact journal record binding")]
    JournalRecordBindingMismatch,
    #[error("capture or revalidate the exact fresh-database invalidation route journal record")]
    Journal(#[source] StorageError),
    #[error("exact fresh-database invalidation route evidence no longer selects the persisted first intent")]
    RouteEvidenceMismatch,
    #[error("inspect exact fresh-database invalidation route database evidence")]
    Inspection(#[source] InspectionError),
    #[error("revalidate the independent fresh-database invalidation route namespace proof")]
    Namespace(#[source] UsrRollbackFreshDbInvalidationRouteNamespaceError),
    #[error("revalidate retained mutable installation namespace around fresh-database invalidation routing")]
    Installation(#[source] crate::installation::Error),
    #[error("fresh-database invalidation route database evidence is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("fresh-database invalidation route database evidence changed from {expected:?} to {actual:?}")]
    DatabaseChanged {
        expected: Box<DatabaseEvidence>,
        actual: Box<DatabaseEvidence>,
    },
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_INITIAL_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_usr_rollback_fresh_db_invalidation_route_database_captures(
    hook: impl FnOnce() + 'static,
) {
    BETWEEN_INITIAL_DATABASE_CAPTURES.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_between_initial_database_captures() {
    BETWEEN_INITIAL_DATABASE_CAPTURES.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_between_initial_database_captures() {}
