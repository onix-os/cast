//! Test-sealed read-only authority for routing `FreshDbInvalidated` to
//! rollback completion.
//!
//! Admission pairs the broader startup database context with a non-cloneable,
//! source-database-bound proof that the exact fresh transition is jointly
//! absent. The authority is issued only after a binding-first
//! DB -> namespace -> DB proof. It exposes no database, namespace, journal,
//! trigger, cleanup, retry, or finalization effect.

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, TransitionJournalBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackCompleteRouteSeal};
use super::{
    DatabaseEvidence, InspectionError, UsrRollbackCompleteRouteNamespaceError,
    UsrRollbackCompleteRouteNamespaceInspection, UsrRollbackCompleteRouteNamespaceProof,
    database_ownership_evidence_compatible, inspect_database, metadata_provenance_evidence_compatible,
};

/// Exact result of read-only rollback-completion route admission.
pub(in crate::client) enum UsrRollbackCompleteRouteAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Ready(UsrRollbackCompleteRouteAuthority<'reservation>),
}

/// Retained evidence authorizing only the next journal route decision.
pub(in crate::client) struct UsrRollbackCompleteRouteAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: UsrRollbackCompleteRouteDatabaseEvidence,
    namespace: UsrRollbackCompleteRouteNamespaceProof,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// General startup context paired with exact, source-database-bound joint
/// absence. This type is intentionally not `Clone`.
#[derive(Debug, Eq, PartialEq)]
struct UsrRollbackCompleteRouteDatabaseEvidence {
    context: DatabaseEvidence,
    absence: db::state::ExactFreshTransitionAbsence,
}

enum DatabaseInspection {
    Exact(UsrRollbackCompleteRouteDatabaseEvidence),
    Incompatible(DatabaseEvidence),
}

impl<'reservation> UsrRollbackCompleteRouteAuthority<'reservation> {
    /// Capture the exact durable `FreshDbInvalidated` prefix without effects.
    /// The route-specific seal has no production constructor, so this
    /// checkpoint remains unreachable from production startup dispatch.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackCompleteRouteSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackCompleteRouteAdmission<'reservation>, UsrRollbackCompleteRouteAuthorityError> {
        if record.phase != Phase::FreshDbInvalidated || record.operation != Operation::NewState {
            return Ok(UsrRollbackCompleteRouteAdmission::NotApplicable);
        }
        if record.rollback.is_none() || !rollback_complete_route_plan_is_exact(record) {
            return Ok(UsrRollbackCompleteRouteAdmission::Deferred);
        }

        let journal_binding = journal.binding();
        if !journal.has_binding(&journal_binding) {
            return Err(UsrRollbackCompleteRouteAuthorityErrorKind::JournalBindingMismatch.into());
        }
        installation.revalidate_mutable_namespace()?;

        let database_before = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => return Ok(UsrRollbackCompleteRouteAdmission::Deferred),
        };
        let namespace_inspection =
            match UsrRollbackCompleteRouteNamespaceInspection::begin(installation, journal, record) {
                Ok(inspection) => inspection,
                Err(_) => return Ok(UsrRollbackCompleteRouteAdmission::Deferred),
            };
        run_between_database_captures();
        let namespace = match namespace_inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackCompleteRouteAdmission::Deferred),
        };
        let database_after = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => return Ok(UsrRollbackCompleteRouteAdmission::Deferred),
        };
        if database_before != database_after || !rollback_complete_route_plan_is_exact(record) {
            return Ok(UsrRollbackCompleteRouteAdmission::Deferred);
        }

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackCompleteRouteAdmission::Ready(Self {
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
    ) -> Result<(), UsrRollbackCompleteRouteAuthorityError> {
        if !journal.has_binding(&self.journal_binding) {
            return Err(UsrRollbackCompleteRouteAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        let database_before =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        self.namespace.revalidate(&self.installation, journal, &self.record)?;
        let database_after =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        if database_before != database_after || !rollback_complete_route_plan_is_exact(&self.record) {
            return Err(UsrRollbackCompleteRouteAuthorityErrorKind::RouteEvidenceMismatch.into());
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

/// Exact narrow plan accepted by the completion-route checkpoint.
fn rollback_complete_route_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    record.operation == Operation::NewState
        && record.phase == Phase::FreshDbInvalidated
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
) -> Result<DatabaseInspection, UsrRollbackCompleteRouteAuthorityError> {
    let candidate = record
        .candidate
        .id
        .map(crate::state::Id::from)
        .ok_or(UsrRollbackCompleteRouteAuthorityErrorKind::RouteEvidenceMismatch)?;
    let exact_before = state_db.inspect_exact_fresh_transition(candidate, &record.transition_id)?;
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    let exact_after = state_db.inspect_exact_fresh_transition(candidate, &record.transition_id)?;
    if exact_before != exact_after {
        return Err(UsrRollbackCompleteRouteAuthorityErrorKind::DatabaseChanged.into());
    }
    let db::state::ExactFreshTransitionObservation::JointlyAbsent(absence) = exact_after else {
        return Ok(DatabaseInspection::Incompatible(context));
    };
    let evidence = UsrRollbackCompleteRouteDatabaseEvidence { context, absence };
    if database_pair_is_exact(record, &evidence) {
        Ok(DatabaseInspection::Exact(evidence))
    } else {
        Ok(DatabaseInspection::Incompatible(evidence.context))
    }
}

fn database_pair_is_exact(record: &TransitionRecord, evidence: &UsrRollbackCompleteRouteDatabaseEvidence) -> bool {
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
    expected: &UsrRollbackCompleteRouteDatabaseEvidence,
    actual: DatabaseInspection,
) -> Result<UsrRollbackCompleteRouteDatabaseEvidence, UsrRollbackCompleteRouteAuthorityError> {
    match actual {
        DatabaseInspection::Exact(actual) if actual == *expected => Ok(actual),
        DatabaseInspection::Exact(_) => Err(UsrRollbackCompleteRouteAuthorityErrorKind::DatabaseChanged.into()),
        DatabaseInspection::Incompatible(evidence) => {
            Err(UsrRollbackCompleteRouteAuthorityErrorKind::DatabaseIncompatible {
                evidence: Box::new(evidence),
            }
            .into())
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackCompleteRouteAuthorityError(#[from] UsrRollbackCompleteRouteAuthorityErrorKind);

impl From<InspectionError> for UsrRollbackCompleteRouteAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackCompleteRouteAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<db::state::ExactFreshTransitionInspectionError> for UsrRollbackCompleteRouteAuthorityError {
    fn from(source: db::state::ExactFreshTransitionInspectionError) -> Self {
        UsrRollbackCompleteRouteAuthorityErrorKind::ExactInspection(source).into()
    }
}

impl From<UsrRollbackCompleteRouteNamespaceError> for UsrRollbackCompleteRouteAuthorityError {
    fn from(source: UsrRollbackCompleteRouteNamespaceError) -> Self {
        UsrRollbackCompleteRouteAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackCompleteRouteAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackCompleteRouteAuthorityErrorKind::Installation(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackCompleteRouteAuthorityErrorKind {
    #[error("rollback-completion route authority was paired with a different open journal store")]
    JournalBindingMismatch,
    #[error("exact FreshDbInvalidated evidence no longer selects rollback completion")]
    RouteEvidenceMismatch,
    #[error("inspect rollback-completion route startup context")]
    Inspection(#[source] InspectionError),
    #[error("inspect exact jointly-absent fresh transition for rollback completion")]
    ExactInspection(#[source] db::state::ExactFreshTransitionInspectionError),
    #[error("revalidate the independent rollback-completion route namespace proof")]
    Namespace(#[source] UsrRollbackCompleteRouteNamespaceError),
    #[error("revalidate retained mutable installation namespace around rollback-completion routing")]
    Installation(#[source] crate::installation::Error),
    #[error("rollback-completion route database context is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("rollback-completion route database evidence changed across its DB -> namespace -> DB sandwich")]
    DatabaseChanged,
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_usr_rollback_complete_route_database_captures(hook: impl FnOnce() + 'static) {
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
