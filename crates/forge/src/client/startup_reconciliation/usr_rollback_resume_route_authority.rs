//! Sealed evidence authority for routing one journal-only rollback intent.

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, StorageError,
        TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackResumeRouteSeal};
use super::{
    DatabaseEvidence, InspectionError, UsrExchangeLayout, UsrRollbackResumeRouteNamespaceError,
    UsrRollbackResumeRouteNamespaceInspection, UsrRollbackResumeRouteNamespaceProof,
    database_ownership_evidence_compatible, inspect_database, metadata_provenance_evidence_compatible,
};

/// Admission result for the narrow journal-only rollback-resume route.
pub(in crate::client) enum UsrRollbackResumeRouteAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Ready(UsrRollbackResumeRouteAuthority<'reservation>),
}

/// Exact retained evidence for one rollback routing advance.
///
/// The active-state reservation is writer exclusion only. It is never treated
/// as live-tree identity or active-selection evidence.
pub(in crate::client) struct UsrRollbackResumeRouteAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackResumeRouteNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> UsrRollbackResumeRouteAuthority<'reservation> {
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackResumeRouteSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<UsrRollbackResumeRouteAdmission<'reservation>, UsrRollbackResumeRouteAuthorityError> {
        if !matches!(record.phase, Phase::RollbackDecided | Phase::UsrRestored)
            || !is_usr_exchange_rollback_source(record)
        {
            return Ok(UsrRollbackResumeRouteAdmission::NotApplicable);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding =
            journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        installation.revalidate_mutable_namespace()?;
        let namespace_inspection = match UsrRollbackResumeRouteNamespaceInspection::begin(installation, journal, record)
        {
            Ok(inspection) => inspection,
            Err(_) => return Ok(UsrRollbackResumeRouteAdmission::Deferred),
        };
        let database = inspect_database(record, state_db, initial_in_flight)?;
        if !database_is_compatible(record, &database) {
            return Ok(UsrRollbackResumeRouteAdmission::Deferred);
        }

        run_between_initial_database_captures();
        let in_flight_after = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
        let database_after = inspect_database(record, state_db, in_flight_after)?;
        if !database_is_compatible(record, &database_after) || database != database_after {
            return Ok(UsrRollbackResumeRouteAdmission::Deferred);
        }
        let namespace = match namespace_inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackResumeRouteAdmission::Deferred),
        };
        if !route_evidence_is_exact(record, namespace.layout()) {
            return Ok(UsrRollbackResumeRouteAdmission::Deferred);
        }

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        require_journal_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackResumeRouteAdmission::Ready(Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        }))
    }

    /// Revalidate an exact database/namespace/database sandwich. Per-open
    /// journal identity is deliberately the first action.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackResumeRouteAuthorityError> {
        require_journal_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        self.installation.revalidate_mutable_namespace()?;
        let database_before = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_before)?;
        self.namespace.revalidate(&self.installation, journal, &self.record)?;
        let database_after = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_after)?;
        if !route_evidence_is_exact(&self.record, self.namespace.layout()) {
            return Err(UsrRollbackResumeRouteAuthorityErrorKind::RouteEvidenceMismatch.into());
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

    /// Revalidate, then consume this complete authority through the exact
    /// predecessor-to-successor journal boundary. The retained database,
    /// namespace, reservation, and record evidence stay owned until the
    /// one-shot store call consumes the predecessor binding.
    pub(in crate::client) fn advance_record_binding(
        self,
        journal: &TransitionJournalStore,
        next: &TransitionRecord,
    ) -> Result<TransitionJournalRecordBinding, UsrRollbackResumeRouteRecordAdvanceError> {
        self.revalidate(journal)?;
        let cast = self.installation.retained_mutable_cast_directory()?;
        journal
            .advance_record_binding(cast, self.journal_record_binding, next)
            .map_err(UsrRollbackResumeRouteRecordAdvanceError::Storage)
    }
}

fn is_usr_exchange_rollback_source(record: &TransitionRecord) -> bool {
    record.rollback.as_ref().is_some_and(|rollback| {
        matches!(
            rollback.source,
            ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged | ForwardPhase::RootLinksComplete
        )
            || (record.operation == Operation::ActiveReblit && rollback.source == ForwardPhase::BootSyncStarted)
    })
}

fn require_journal_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), UsrRollbackResumeRouteAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(UsrRollbackResumeRouteAuthorityErrorKind::JournalRecordBindingMismatch.into());
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, binding, record)? {
        Ok(())
    } else {
        Err(UsrRollbackResumeRouteAuthorityErrorKind::JournalRecordBindingMismatch.into())
    }
}

fn route_evidence_is_exact(record: &TransitionRecord, layout: UsrExchangeLayout) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    let boot_source = record.operation == Operation::ActiveReblit && rollback.source == ForwardPhase::BootSyncStarted;
    if rollback.previous_archive != RollbackAction::NotRequired
        || rollback.candidate.action != RollbackAction::Pending
        || rollback.boot
            != if boot_source {
                BootRollback::PendingUnverifiable
            } else {
                BootRollback::NotRequired
            }
    {
        return false;
    }
    let fresh_is_exact = match record.operation {
        Operation::NewState => rollback.fresh_db == RollbackAction::Pending,
        Operation::ActivateArchived | Operation::ActiveReblit => rollback.fresh_db == RollbackAction::NotRequired,
    };
    let candidate_disposition_is_exact = match record.operation {
        Operation::ActivateArchived => rollback.candidate.disposition == AbortDisposition::Rearchive,
        Operation::NewState | Operation::ActiveReblit => rollback.candidate.disposition == AbortDisposition::Quarantine,
    };
    let external_effects_are_exact =
        rollback.external_effects_may_remain == (record.operation != Operation::ActivateArchived);
    fresh_is_exact
        && candidate_disposition_is_exact
        && external_effects_are_exact
        && match record.phase {
            Phase::RollbackDecided => matches!(
                (rollback.usr_exchange, layout),
                (RollbackAction::Pending, UsrExchangeLayout::Post)
                    | (RollbackAction::AlreadySatisfied, UsrExchangeLayout::Pre)
            ),
            Phase::UsrRestored => matches!(
                (rollback.usr_exchange, layout),
                (
                    RollbackAction::Applied | RollbackAction::AlreadySatisfied,
                    UsrExchangeLayout::Pre
                )
            ),
            _ => false,
        }
}

#[cfg(test)]
pub(in crate::client) fn usr_rollback_resume_route_plan_is_exact_for_test(record: &TransitionRecord) -> bool {
    let layout = match record.phase {
        Phase::RollbackDecided => UsrExchangeLayout::Post,
        Phase::UsrRestored => UsrExchangeLayout::Pre,
        _ => return false,
    };
    is_usr_exchange_rollback_source(record) && route_evidence_is_exact(record, layout)
}

fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseEvidence, UsrRollbackResumeRouteAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let evidence = inspect_database(record, state_db, in_flight)?;
    if database_is_compatible(record, &evidence) {
        Ok(evidence)
    } else {
        Err(UsrRollbackResumeRouteAuthorityErrorKind::DatabaseIncompatible {
            evidence: Box::new(evidence),
        }
        .into())
    }
}

fn database_is_compatible(record: &TransitionRecord, evidence: &DatabaseEvidence) -> bool {
    database_ownership_evidence_compatible(record, evidence)
        && metadata_provenance_evidence_compatible(record, evidence)
}

fn require_exact_database(
    expected: &DatabaseEvidence,
    actual: DatabaseEvidence,
) -> Result<(), UsrRollbackResumeRouteAuthorityError> {
    if *expected == actual {
        Ok(())
    } else {
        Err(UsrRollbackResumeRouteAuthorityErrorKind::DatabaseChanged {
            expected: Box::new(expected.clone()),
            actual: Box::new(actual),
        }
        .into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackResumeRouteAuthorityError(#[from] UsrRollbackResumeRouteAuthorityErrorKind);

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackResumeRouteRecordAdvanceError {
    #[error("revalidate exact rollback-resume route authority before the bound journal advance")]
    Authority(#[from] UsrRollbackResumeRouteAuthorityError),
    #[error("revalidate retained installation before the bound rollback-resume route journal advance")]
    Installation(#[from] crate::installation::Error),
    #[error("advance the exact bound rollback-resume route journal record")]
    Storage(#[source] StorageError),
}

impl From<InspectionError> for UsrRollbackResumeRouteAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackResumeRouteAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<UsrRollbackResumeRouteNamespaceError> for UsrRollbackResumeRouteAuthorityError {
    fn from(source: UsrRollbackResumeRouteNamespaceError) -> Self {
        UsrRollbackResumeRouteAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackResumeRouteAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackResumeRouteAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for UsrRollbackResumeRouteAuthorityError {
    fn from(source: StorageError) -> Self {
        UsrRollbackResumeRouteAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackResumeRouteAuthorityErrorKind {
    #[error("startup rollback-resume authority lost its exact canonical journal record binding")]
    JournalRecordBindingMismatch,
    #[error("exact startup rollback-resume evidence no longer selects the persisted first intent")]
    RouteEvidenceMismatch,
    #[error("inspect exact rollback-resume database evidence")]
    Inspection(#[source] InspectionError),
    #[error("revalidate the independent rollback-resume namespace proof")]
    Namespace(#[source] UsrRollbackResumeRouteNamespaceError),
    #[error("revalidate retained mutable installation namespace around rollback-resume authority")]
    Installation(#[source] crate::installation::Error),
    #[error("capture or revalidate the exact rollback-resume journal record binding")]
    Journal(#[source] StorageError),
    #[error("rollback-resume database evidence is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("rollback-resume database evidence changed from {expected:?} to {actual:?}")]
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
pub(in crate::client) fn arm_between_usr_rollback_resume_route_database_captures(hook: impl FnOnce() + 'static) {
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
