//! Sealed read-only authority for routing an ActiveReblit
//! `CandidatePreserved` record to rollback completion.
//!
//! This authority is deliberately disjoint from the NewState completion
//! route: there is no fresh-database or joint-absence evidence. Admission
//! instead retains the exact cleared existing-state row, its metadata
//! provenance, the preserved whole-wrapper namespace, exact journal-record
//! binding,
//! installation capability, and active-state reservation. It exposes no
//! database, namespace, journal, trigger, cleanup, retry, or finalization
//! effect.

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, StorageError,
        TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackActiveReblitCompleteRouteSeal,
};
use super::{
    DatabaseEvidence, InspectionError, UsrRollbackActiveReblitCompleteRouteNamespaceError,
    UsrRollbackActiveReblitCompleteRouteNamespaceInspection, UsrRollbackActiveReblitCompleteRouteNamespaceProof,
    database_ownership_evidence_compatible, inspect_database, metadata_provenance_evidence_compatible,
};

/// Exact result of read-only ActiveReblit rollback-completion admission.
pub(in crate::client) enum UsrRollbackActiveReblitCompleteRouteAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Ready(UsrRollbackActiveReblitCompleteRouteAuthority<'reservation>),
}

/// Retained evidence authorizing only the next ActiveReblit journal route.
pub(in crate::client) struct UsrRollbackActiveReblitCompleteRouteAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: UsrRollbackActiveReblitCompleteRouteDatabaseEvidence,
    namespace: UsrRollbackActiveReblitCompleteRouteNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Exact existing-state observation bound to this route. This type is
/// intentionally not `Clone`.
#[derive(Debug, Eq, PartialEq)]
struct UsrRollbackActiveReblitCompleteRouteDatabaseEvidence {
    context: DatabaseEvidence,
}

enum DatabaseInspection {
    Exact(UsrRollbackActiveReblitCompleteRouteDatabaseEvidence),
    Incompatible(DatabaseEvidence),
}

impl<'reservation> UsrRollbackActiveReblitCompleteRouteAuthority<'reservation> {
    /// Capture the exact durable ActiveReblit `CandidatePreserved` prefix
    /// without effects. Only the phase-specific startup child can construct
    /// the production seal.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackActiveReblitCompleteRouteSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitCompleteRouteAdmission<'reservation>,
        UsrRollbackActiveReblitCompleteRouteAuthorityError,
    > {
        if record.operation != Operation::ActiveReblit || record.phase != Phase::CandidatePreserved {
            return Ok(UsrRollbackActiveReblitCompleteRouteAdmission::NotApplicable);
        }
        if record.rollback.is_none() || !active_reblit_complete_route_plan_is_exact(record) {
            return Ok(UsrRollbackActiveReblitCompleteRouteAdmission::Deferred);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding =
            journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        installation.revalidate_mutable_namespace()?;

        let database_before = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActiveReblitCompleteRouteAdmission::Deferred);
            }
        };
        let namespace_inspection = match UsrRollbackActiveReblitCompleteRouteNamespaceInspection::begin(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(inspection) => inspection,
            Err(_) => return Ok(UsrRollbackActiveReblitCompleteRouteAdmission::Deferred),
        };
        run_between_database_captures();
        let namespace = match namespace_inspection.finish(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackActiveReblitCompleteRouteAdmission::Deferred),
        };
        let database_after = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackActiveReblitCompleteRouteAdmission::Deferred);
            }
        };
        if database_before != database_after || !active_reblit_complete_route_plan_is_exact(record) {
            return Ok(UsrRollbackActiveReblitCompleteRouteAdmission::Deferred);
        }

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        require_journal_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackActiveReblitCompleteRouteAdmission::Ready(Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database: database_after,
            namespace,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        }))
    }

    /// Revalidate the exact binding-first DB -> namespace -> DB sandwich.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackActiveReblitCompleteRouteAuthorityError> {
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
        if database_before != database_after || !active_reblit_complete_route_plan_is_exact(&self.record) {
            return Err(UsrRollbackActiveReblitCompleteRouteAuthorityErrorKind::RouteEvidenceMismatch.into());
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
    ) -> Result<TransitionJournalRecordBinding, UsrRollbackActiveReblitCompleteRouteRecordAdvanceError> {
        self.revalidate(journal)?;
        let cast = self.installation.retained_mutable_cast_directory()?;
        journal
            .advance_record_binding(cast, self.journal_record_binding, next)
            .map_err(UsrRollbackActiveReblitCompleteRouteRecordAdvanceError::Storage)
    }

    #[cfg(test)]
    pub(in crate::client) fn wrapper_index(&self) -> usize {
        self.namespace.wrapper_index()
    }
}

fn require_journal_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), UsrRollbackActiveReblitCompleteRouteAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(
            UsrRollbackActiveReblitCompleteRouteAuthorityErrorKind::JournalRecordBindingMismatch.into(),
        );
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, binding, record)? {
        Ok(())
    } else {
        Err(UsrRollbackActiveReblitCompleteRouteAuthorityErrorKind::JournalRecordBindingMismatch.into())
    }
}

/// Exact narrow plan accepted by the ActiveReblit completion checkpoint.
fn active_reblit_complete_route_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    record.operation == Operation::ActiveReblit
        && record.phase == Phase::CandidatePreserved
        && record.candidate.id.is_some()
        && record.candidate.id == record.previous.id
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
        && rollback.fresh_db == RollbackAction::NotRequired
        && rollback.boot == BootRollback::NotRequired
        && rollback.external_effects_may_remain
}

/// Inspect exact existing-state evidence around the general startup context
/// so no candidate row, ownership, or provenance observation can be paired
/// with a different database moment.
fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseInspection, UsrRollbackActiveReblitCompleteRouteAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    if active_reblit_database_pair_is_exact(record, &context) {
        Ok(DatabaseInspection::Exact(
            UsrRollbackActiveReblitCompleteRouteDatabaseEvidence { context },
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
    expected: &UsrRollbackActiveReblitCompleteRouteDatabaseEvidence,
    actual: DatabaseInspection,
) -> Result<UsrRollbackActiveReblitCompleteRouteDatabaseEvidence, UsrRollbackActiveReblitCompleteRouteAuthorityError> {
    match actual {
        DatabaseInspection::Exact(actual) if actual == *expected => Ok(actual),
        DatabaseInspection::Exact(_) => {
            Err(UsrRollbackActiveReblitCompleteRouteAuthorityErrorKind::DatabaseChanged.into())
        }
        DatabaseInspection::Incompatible(evidence) => Err(
            UsrRollbackActiveReblitCompleteRouteAuthorityErrorKind::DatabaseIncompatible {
                evidence: Box::new(evidence),
            }
            .into(),
        ),
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackActiveReblitCompleteRouteAuthorityError(
    #[from] UsrRollbackActiveReblitCompleteRouteAuthorityErrorKind,
);

impl From<InspectionError> for UsrRollbackActiveReblitCompleteRouteAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackActiveReblitCompleteRouteAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<UsrRollbackActiveReblitCompleteRouteNamespaceError> for UsrRollbackActiveReblitCompleteRouteAuthorityError {
    fn from(source: UsrRollbackActiveReblitCompleteRouteNamespaceError) -> Self {
        UsrRollbackActiveReblitCompleteRouteAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackActiveReblitCompleteRouteAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackActiveReblitCompleteRouteAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for UsrRollbackActiveReblitCompleteRouteAuthorityError {
    fn from(source: StorageError) -> Self {
        UsrRollbackActiveReblitCompleteRouteAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackActiveReblitCompleteRouteRecordAdvanceError {
    #[error("revalidate exact ActiveReblit rollback-completion route authority before the bound journal advance")]
    Authority(#[from] UsrRollbackActiveReblitCompleteRouteAuthorityError),
    #[error("revalidate retained installation before the bound ActiveReblit rollback-completion route advance")]
    Installation(#[from] crate::installation::Error),
    #[error("advance the exact bound ActiveReblit rollback-completion route record")]
    Storage(#[source] StorageError),
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackActiveReblitCompleteRouteAuthorityErrorKind {
    #[error("ActiveReblit rollback-completion authority lost its exact journal record binding")]
    JournalRecordBindingMismatch,
    #[error("capture or revalidate the exact ActiveReblit rollback-completion journal record")]
    Journal(#[source] StorageError),
    #[error("exact ActiveReblit CandidatePreserved evidence no longer selects rollback completion")]
    RouteEvidenceMismatch,
    #[error("inspect ActiveReblit rollback-completion startup database context")]
    Inspection(#[source] InspectionError),
    #[error("revalidate the independent ActiveReblit rollback-completion namespace proof")]
    Namespace(#[source] UsrRollbackActiveReblitCompleteRouteNamespaceError),
    #[error("revalidate retained mutable installation namespace around ActiveReblit rollback-completion routing")]
    Installation(#[source] crate::installation::Error),
    #[error("ActiveReblit rollback-completion database context is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("ActiveReblit rollback-completion database evidence changed across its DB -> namespace -> DB sandwich")]
    DatabaseChanged,
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_usr_rollback_active_reblit_complete_route_database_captures(
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
