//! Test-sealed authority for exact NewState rollback finalization.
//!
//! Admission pairs general startup database context with a non-cloneable,
//! source-database-bound proof that the exact fresh transition is jointly
//! absent. The authority is issued only after a binding-first
//! DB -> terminal namespace -> DB sandwich. It exposes no journal deletion,
//! database, namespace, trigger, cleanup, retry, or startup-dispatch effect.

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, TransitionJournalBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackFinalizationSeal};
use super::{
    DatabaseEvidence, InspectionError, UsrRollbackFinalizationNamespaceError,
    UsrRollbackFinalizationNamespaceInspection, UsrRollbackFinalizationNamespaceProof,
    database_ownership_evidence_compatible, inspect_database, metadata_provenance_evidence_compatible,
};

#[cfg(test)]
#[allow(dead_code)] // shared candidate fixture contains wider preservation helpers
#[path = "usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod candidate_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared invalidation fixture contains wider effect helpers
#[path = "usr_rollback_fresh_db_invalidation_authority/tests/support.rs"]
mod invalidation_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider startup-recovery helpers
#[path = "../startup_recovery/test_support.rs"]
mod test_fixture;
#[cfg(test)]
mod tests;

/// Exact result of read-only rollback-finalization admission.
pub(in crate::client) enum UsrRollbackFinalizationAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Ready(UsrRollbackFinalizationAuthority<'reservation>),
}

/// Retained evidence authorizing only a future terminal journal finalizer.
///
/// This type is intentionally not `Clone`. Checkpoint A provides no method
/// which exposes or deletes the retained journal record.
pub(in crate::client) struct UsrRollbackFinalizationAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: UsrRollbackFinalizationDatabaseEvidence,
    namespace: UsrRollbackFinalizationNamespaceProof,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// General startup context paired with exact, source-database-bound joint
/// absence. This type is intentionally not `Clone`.
#[derive(Debug, Eq, PartialEq)]
struct UsrRollbackFinalizationDatabaseEvidence {
    context: DatabaseEvidence,
    absence: db::state::ExactFreshTransitionAbsence,
}

enum DatabaseInspection {
    Exact(UsrRollbackFinalizationDatabaseEvidence),
    Incompatible(DatabaseEvidence),
}

impl<'reservation> UsrRollbackFinalizationAuthority<'reservation> {
    /// Capture exact durable `RollbackComplete` evidence without effects.
    ///
    /// The supplied seal has no production constructor in checkpoint A.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackFinalizationSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackFinalizationAdmission<'reservation>, UsrRollbackFinalizationAuthorityError> {
        if record.phase != Phase::RollbackComplete || record.operation != Operation::NewState {
            return Ok(UsrRollbackFinalizationAdmission::NotApplicable);
        }
        if record.rollback.is_none() || !rollback_finalization_plan_is_exact(record) {
            return Ok(UsrRollbackFinalizationAdmission::Deferred);
        }

        let journal_binding = journal.binding();
        if !journal.has_binding(&journal_binding) {
            return Err(UsrRollbackFinalizationAuthorityErrorKind::JournalBindingMismatch.into());
        }
        installation.revalidate_mutable_namespace()?;

        let database_before = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => return Ok(UsrRollbackFinalizationAdmission::Deferred),
        };
        let namespace_inspection =
            match UsrRollbackFinalizationNamespaceInspection::begin(installation, journal, record) {
                Ok(inspection) => inspection,
                Err(_) => return Ok(UsrRollbackFinalizationAdmission::Deferred),
            };
        run_between_database_captures();
        let namespace = match namespace_inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackFinalizationAdmission::Deferred),
        };
        let database_after = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => return Ok(UsrRollbackFinalizationAdmission::Deferred),
        };
        if database_before != database_after || !rollback_finalization_plan_is_exact(record) {
            return Ok(UsrRollbackFinalizationAdmission::Deferred);
        }

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackFinalizationAdmission::Ready(Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database: database_after,
            namespace,
            journal_binding,
            _active_state_reservation: active_state_reservation,
        }))
    }

    /// Revalidate the exact binding-first DB -> namespace -> DB sandwich.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackFinalizationAuthorityError> {
        if !journal.has_binding(&self.journal_binding) {
            return Err(UsrRollbackFinalizationAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        let database_before =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        self.namespace.revalidate(&self.installation, journal, &self.record)?;
        let database_after =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        if database_before != database_after || !rollback_finalization_plan_is_exact(&self.record) {
            return Err(UsrRollbackFinalizationAuthorityErrorKind::FinalizationEvidenceMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.installation
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.record
    }
}

/// Exact narrow plan accepted by NewState rollback finalization.
fn rollback_finalization_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    record.operation == Operation::NewState
        && record.phase == Phase::RollbackComplete
        && record.candidate.id.is_some()
        && matches!(
            rollback.source,
            ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged
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
        && matches!(
            rollback.fresh_db,
            RollbackAction::Applied | RollbackAction::AlreadySatisfied
        )
        && rollback.boot == BootRollback::NotRequired
        && rollback.external_effects_may_remain
}

/// Inspect exact-before -> generic context -> exact-after so neither evidence
/// source can be paired with a different database moment.
fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseInspection, UsrRollbackFinalizationAuthorityError> {
    let candidate = record
        .candidate
        .id
        .map(crate::state::Id::from)
        .ok_or(UsrRollbackFinalizationAuthorityErrorKind::FinalizationEvidenceMismatch)?;
    let exact_before = state_db.inspect_exact_fresh_transition(candidate, &record.transition_id)?;
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    let exact_after = state_db.inspect_exact_fresh_transition(candidate, &record.transition_id)?;
    if exact_before != exact_after {
        return Err(UsrRollbackFinalizationAuthorityErrorKind::DatabaseChanged.into());
    }
    let db::state::ExactFreshTransitionObservation::JointlyAbsent(absence) = exact_after else {
        return Ok(DatabaseInspection::Incompatible(context));
    };
    let evidence = UsrRollbackFinalizationDatabaseEvidence { context, absence };
    if database_pair_is_exact(record, &evidence) {
        Ok(DatabaseInspection::Exact(evidence))
    } else {
        Ok(DatabaseInspection::Incompatible(evidence.context))
    }
}

fn database_pair_is_exact(record: &TransitionRecord, evidence: &UsrRollbackFinalizationDatabaseEvidence) -> bool {
    if !database_ownership_evidence_compatible(record, &evidence.context)
        || !metadata_provenance_evidence_compatible(record, &evidence.context)
    {
        return false;
    }
    let Some(candidate) = record.candidate.id.map(crate::state::Id::from) else {
        return false;
    };
    matches!(
        &evidence.context,
        DatabaseEvidence::CandidateOwnership {
            state,
            ownership: db::state::TransitionOwnership::Missing,
            provenance: None,
            ..
        } if *state == candidate
    ) && evidence.absence.state_id() == candidate
        && evidence.absence.transition_id() == &record.transition_id
}

fn require_exact_database(
    expected: &UsrRollbackFinalizationDatabaseEvidence,
    actual: DatabaseInspection,
) -> Result<UsrRollbackFinalizationDatabaseEvidence, UsrRollbackFinalizationAuthorityError> {
    match actual {
        DatabaseInspection::Exact(actual) if actual == *expected => Ok(actual),
        DatabaseInspection::Exact(_) => Err(UsrRollbackFinalizationAuthorityErrorKind::DatabaseChanged.into()),
        DatabaseInspection::Incompatible(evidence) => {
            Err(UsrRollbackFinalizationAuthorityErrorKind::DatabaseIncompatible {
                evidence: Box::new(evidence),
            }
            .into())
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackFinalizationAuthorityError(#[from] UsrRollbackFinalizationAuthorityErrorKind);

impl From<InspectionError> for UsrRollbackFinalizationAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackFinalizationAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<db::state::ExactFreshTransitionInspectionError> for UsrRollbackFinalizationAuthorityError {
    fn from(source: db::state::ExactFreshTransitionInspectionError) -> Self {
        UsrRollbackFinalizationAuthorityErrorKind::ExactInspection(source).into()
    }
}

impl From<UsrRollbackFinalizationNamespaceError> for UsrRollbackFinalizationAuthorityError {
    fn from(source: UsrRollbackFinalizationNamespaceError) -> Self {
        UsrRollbackFinalizationAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackFinalizationAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackFinalizationAuthorityErrorKind::Installation(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackFinalizationAuthorityErrorKind {
    #[error("rollback-finalization authority was paired with a different open journal store")]
    JournalBindingMismatch,
    #[error("exact RollbackComplete evidence no longer authorizes finalization")]
    FinalizationEvidenceMismatch,
    #[error("inspect rollback-finalization startup context")]
    Inspection(#[source] InspectionError),
    #[error("inspect exact jointly-absent fresh transition for rollback finalization")]
    ExactInspection(#[source] db::state::ExactFreshTransitionInspectionError),
    #[error("revalidate the independent rollback-finalization namespace proof")]
    Namespace(#[source] UsrRollbackFinalizationNamespaceError),
    #[error("revalidate retained mutable installation namespace around rollback finalization")]
    Installation(#[source] crate::installation::Error),
    #[error("rollback-finalization database context is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("rollback-finalization database evidence changed across its DB -> namespace -> DB sandwich")]
    DatabaseChanged,
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_usr_rollback_finalization_database_captures(hook: impl FnOnce() + 'static) {
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
