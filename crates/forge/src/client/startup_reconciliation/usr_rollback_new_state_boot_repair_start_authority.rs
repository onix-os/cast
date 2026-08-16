//! Sealed read-only authority for routing a NewState `FreshDbInvalidated`
//! record to `BootRepairRequired`.
//!
//! Admission is deliberately disjoint from ordinary rollback completion: it
//! accepts only a plan whose boot repair is still `PendingUnverifiable`, where
//! the completion route accepts only `NotRequired`. The two are mutually
//! exclusive at the same phase, so a rollback that published boot state cannot
//! be completed without repairing it first.
//!
//! The evidence is identical to the completion route — same phase, same
//! preserved-candidate topology, same jointly-absent fresh transition — so the
//! namespace proof is shared rather than duplicated. This authority exposes no
//! boot, database, namespace, journal, cleanup, retry, or finalization effect.

use crate::{
    Installation, db,
    transition_journal::{
        BootRollback, Operation, Phase, RollbackAction, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackNewStateBootRepairStartSeal,
};
use super::{
    DatabaseEvidence, InspectionError, UsrRollbackNewStateBootRepairNamespaceError,
    UsrRollbackNewStateBootRepairNamespaceInspection, UsrRollbackNewStateBootRepairNamespaceProof,
    database_ownership_evidence_compatible, inspect_database, metadata_provenance_evidence_compatible,
};

/// Exact result of read-only NewState boot-repair-required admission.
pub(in crate::client) enum UsrRollbackNewStateBootRepairStartAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Ready(UsrRollbackNewStateBootRepairStartAuthority<'reservation>),
}

/// Retained evidence authorizing only the next journal route decision.
pub(in crate::client) struct UsrRollbackNewStateBootRepairStartAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: UsrRollbackNewStateBootRepairStartDatabaseEvidence,
    namespace: UsrRollbackNewStateBootRepairNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// General startup context paired with exact, source-database-bound joint
/// absence. This type is intentionally not `Clone`.
#[derive(Debug, Eq, PartialEq)]
struct UsrRollbackNewStateBootRepairStartDatabaseEvidence {
    context: DatabaseEvidence,
    absence: db::state::ExactFreshTransitionAbsence,
}

enum DatabaseInspection {
    Exact(UsrRollbackNewStateBootRepairStartDatabaseEvidence),
    Incompatible(DatabaseEvidence),
}

impl<'reservation> UsrRollbackNewStateBootRepairStartAuthority<'reservation> {
    /// Capture the exact durable `FreshDbInvalidated` boot plan without
    /// effects. Only the phase-specific writer-first startup child can
    /// construct the production route seal.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackNewStateBootRepairStartSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackNewStateBootRepairStartAdmission<'reservation>,
        UsrRollbackNewStateBootRepairStartAuthorityError,
    > {
        if record.phase != Phase::BootRepairRequired || record.operation != Operation::NewState {
            return Ok(UsrRollbackNewStateBootRepairStartAdmission::NotApplicable);
        }
        if record.rollback.is_none() || !boot_repair_required_plan_is_exact(record) {
            return Ok(UsrRollbackNewStateBootRepairStartAdmission::NotApplicable);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding = journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        installation.revalidate_mutable_namespace()?;

        let database_before = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackNewStateBootRepairStartAdmission::Deferred);
            }
        };
        let namespace_inspection = match UsrRollbackNewStateBootRepairNamespaceInspection::begin(installation, journal, record) {
            Ok(inspection) => inspection,
            Err(_) => return Ok(UsrRollbackNewStateBootRepairStartAdmission::Deferred),
        };
        run_between_database_captures();
        let namespace = match namespace_inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackNewStateBootRepairStartAdmission::Deferred),
        };
        let database_after = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackNewStateBootRepairStartAdmission::Deferred);
            }
        };
        if database_before != database_after || !boot_repair_required_plan_is_exact(record) {
            return Ok(UsrRollbackNewStateBootRepairStartAdmission::Deferred);
        }

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        require_journal_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackNewStateBootRepairStartAdmission::Ready(Self {
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
    ) -> Result<(), UsrRollbackNewStateBootRepairStartAuthorityError> {
        require_journal_record_binding(&self.installation, journal, &self.journal_record_binding, &self.record)?;
        self.installation.revalidate_mutable_namespace()?;
        let database_before =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        self.namespace.revalidate(&self.installation, journal, &self.record)?;
        let database_after =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        if database_before != database_after || !boot_repair_required_plan_is_exact(&self.record) {
            return Err(UsrRollbackNewStateBootRepairStartAuthorityErrorKind::RouteEvidenceMismatch.into());
        }
        require_journal_record_binding(&self.installation, journal, &self.journal_record_binding, &self.record)?;
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
    ) -> Result<TransitionJournalRecordBinding, UsrRollbackNewStateBootRepairStartRecordAdvanceError> {
        self.revalidate(journal)?;
        let cast = self.installation.retained_mutable_cast_directory()?;
        journal
            .advance_record_binding(cast, self.journal_record_binding, next)
            .map_err(UsrRollbackNewStateBootRepairStartRecordAdvanceError::Storage)
    }
}

fn require_journal_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), UsrRollbackNewStateBootRepairStartAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(UsrRollbackNewStateBootRepairStartAuthorityErrorKind::JournalRecordBindingMismatch.into());
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, binding, record)? {
        Ok(())
    } else {
        Err(UsrRollbackNewStateBootRepairStartAuthorityErrorKind::JournalRecordBindingMismatch.into())
    }
}

/// Exact narrow plan accepted by the boot-repair boundary.
#[cfg(test)]
pub(in crate::client) fn usr_rollback_new_state_boot_repair_start_plan_is_exact_for_test(
    record: &TransitionRecord,
) -> bool {
    boot_repair_required_plan_is_exact(record)
}

fn boot_repair_required_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    record.operation == Operation::NewState
        && record.phase == Phase::BootRepairRequired
        && record.candidate.id.is_some()
        && crate::transition_journal::rollback_evidence_is_on_chain(record)
        && if record.previous_restore_rollback_is_possible(rollback.source) {
            rollback.previous_archive.resolved()
        } else {
            rollback.previous_archive == RollbackAction::NotRequired
        }
        && super::rollback_usr_exchange_is_settled(rollback.usr_exchange, rollback.source)
        && matches!(
            rollback.candidate.action,
            RollbackAction::Applied | RollbackAction::AlreadySatisfied
        )
        && rollback.candidate.disposition == record.candidate_disposition_for(rollback.source)
        && matches!(
            rollback.fresh_db,
            RollbackAction::Applied | RollbackAction::AlreadySatisfied
        )
        // The one field that separates this route from rollback completion at
        // the same phase. `next_rollback_phase` selects `BootRepairRequired`
        // for exactly this value once every rollback action is drained.
        && rollback.boot == BootRollback::PendingUnverifiable
        && rollback.external_effects_may_remain == record.expected_external_effects_may_remain(rollback.source)
}

/// Inspect exact-before -> generic context -> exact-after so neither evidence
/// source can be paired with a different database moment.
fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseInspection, UsrRollbackNewStateBootRepairStartAuthorityError> {
    let candidate = record
        .candidate
        .id
        .map(crate::state::Id::from)
        .ok_or(UsrRollbackNewStateBootRepairStartAuthorityErrorKind::RouteEvidenceMismatch)?;
    let exact_before = state_db.inspect_exact_fresh_transition(candidate, &record.transition_id)?;
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    let exact_after = state_db.inspect_exact_fresh_transition(candidate, &record.transition_id)?;
    if exact_before != exact_after {
        return Err(UsrRollbackNewStateBootRepairStartAuthorityErrorKind::DatabaseChanged.into());
    }
    let db::state::ExactFreshTransitionObservation::JointlyAbsent(absence) = exact_after else {
        return Ok(DatabaseInspection::Incompatible(context));
    };
    let evidence = UsrRollbackNewStateBootRepairStartDatabaseEvidence { context, absence };
    if database_pair_is_exact(record, &evidence) {
        Ok(DatabaseInspection::Exact(evidence))
    } else {
        Ok(DatabaseInspection::Incompatible(evidence.context))
    }
}

fn database_pair_is_exact(
    record: &TransitionRecord,
    evidence: &UsrRollbackNewStateBootRepairStartDatabaseEvidence,
) -> bool {
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
    expected: &UsrRollbackNewStateBootRepairStartDatabaseEvidence,
    actual: DatabaseInspection,
) -> Result<
    UsrRollbackNewStateBootRepairStartDatabaseEvidence,
    UsrRollbackNewStateBootRepairStartAuthorityError,
> {
    match actual {
        DatabaseInspection::Exact(actual) if actual == *expected => Ok(actual),
        DatabaseInspection::Exact(_) => {
            Err(UsrRollbackNewStateBootRepairStartAuthorityErrorKind::DatabaseChanged.into())
        }
        DatabaseInspection::Incompatible(evidence) => {
            Err(UsrRollbackNewStateBootRepairStartAuthorityErrorKind::DatabaseIncompatible {
                evidence: Box::new(evidence),
            }
            .into())
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackNewStateBootRepairStartAuthorityError(
    #[from] UsrRollbackNewStateBootRepairStartAuthorityErrorKind,
);

impl From<InspectionError> for UsrRollbackNewStateBootRepairStartAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackNewStateBootRepairStartAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<db::state::ExactFreshTransitionInspectionError> for UsrRollbackNewStateBootRepairStartAuthorityError {
    fn from(source: db::state::ExactFreshTransitionInspectionError) -> Self {
        UsrRollbackNewStateBootRepairStartAuthorityErrorKind::ExactInspection(source).into()
    }
}

impl From<UsrRollbackNewStateBootRepairNamespaceError> for UsrRollbackNewStateBootRepairStartAuthorityError {
    fn from(source: UsrRollbackNewStateBootRepairNamespaceError) -> Self {
        UsrRollbackNewStateBootRepairStartAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackNewStateBootRepairStartAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackNewStateBootRepairStartAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for UsrRollbackNewStateBootRepairStartAuthorityError {
    fn from(source: StorageError) -> Self {
        UsrRollbackNewStateBootRepairStartAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackNewStateBootRepairStartRecordAdvanceError {
    #[error("revalidate exact NewState boot-repair-required authority before the bound journal advance")]
    Authority(#[from] UsrRollbackNewStateBootRepairStartAuthorityError),
    #[error("revalidate retained installation before the bound NewState boot-repair-required advance")]
    Installation(#[from] crate::installation::Error),
    #[error("advance the exact bound NewState boot-repair-required record")]
    Storage(#[source] StorageError),
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackNewStateBootRepairStartAuthorityErrorKind {
    #[error("NewState boot-repair-required authority lost its exact journal record binding")]
    JournalRecordBindingMismatch,
    #[error("capture or revalidate the exact NewState boot-repair-required journal record")]
    Journal(#[source] StorageError),
    #[error("exact FreshDbInvalidated evidence no longer selects boot repair")]
    RouteEvidenceMismatch,
    #[error("inspect NewState boot-repair-required startup context")]
    Inspection(#[source] InspectionError),
    #[error("inspect the exact NewState boot-repair-required fresh transition")]
    ExactInspection(#[source] db::state::ExactFreshTransitionInspectionError),
    #[error("the NewState boot-repair-required database evidence changed during proof")]
    DatabaseChanged,
    #[error("the NewState boot-repair-required database evidence is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("prove the exact NewState boot-repair-required namespace")]
    Namespace(#[source] UsrRollbackNewStateBootRepairNamespaceError),
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[source] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_usr_rollback_new_state_boot_repair_start_database_captures(
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
