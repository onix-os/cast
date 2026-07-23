//! Sealed authority for one exact fresh-database invalidation effect.
//!
//! Admission pairs the broader startup database context with the non-cloneable
//! exact fresh-transition observation. Present and jointly-absent observations
//! become disjoint Apply and Finish authorities only after a binding-first
//! DB -> namespace -> DB proof. Only the phase-specific writer-first startup
//! child and consuming invalidation leaf can reach this checkpoint.

mod effect_reconciliation;

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, StorageError,
        TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackFreshDbInvalidationSeal};
use super::{
    DatabaseEvidence, InspectionError, UsrRollbackFreshDbInvalidationNamespaceError,
    UsrRollbackFreshDbInvalidationNamespaceInspection, UsrRollbackFreshDbInvalidationNamespaceProof,
    database_ownership_evidence_compatible, inspect_database, metadata_provenance_evidence_compatible,
};

#[cfg(test)]
pub(in crate::client) use effect_reconciliation::fresh_db_invalidation_removal_call_count;
pub(in crate::client) use effect_reconciliation::{
    UsrRollbackFreshDbInvalidationApplyReconciliation, UsrRollbackFreshDbInvalidationEffectAuthority,
    UsrRollbackFreshDbInvalidationRecordAdvanceError,
};

/// Exact result of read-only `FreshDbInvalidationIntent` admission.
pub(in crate::client) enum UsrRollbackFreshDbInvalidationAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Apply(UsrRollbackFreshDbInvalidationApplyAuthority<'reservation>),
    Finish(UsrRollbackFreshDbInvalidationFinishAuthority<'reservation>),
}

/// Common evidence retained privately behind the disjoint Present/Absent
/// typestates.
pub(in crate::client) struct UsrRollbackFreshDbInvalidationAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: UsrRollbackFreshDbInvalidationDatabaseEvidence,
    namespace: UsrRollbackFreshDbInvalidationNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Exact Present authority. It can perform at most one removal call.
pub(in crate::client) struct UsrRollbackFreshDbInvalidationApplyAuthority<'reservation> {
    evidence: UsrRollbackFreshDbInvalidationAuthority<'reservation>,
}

/// Exact jointly-absent authority. It cannot issue a removal call.
pub(in crate::client) struct UsrRollbackFreshDbInvalidationFinishAuthority<'reservation> {
    evidence: UsrRollbackFreshDbInvalidationAuthority<'reservation>,
}

/// General startup context paired with an exact source-database-bound
/// observation. This type is intentionally not Clone.
#[derive(Debug, Eq, PartialEq)]
enum UsrRollbackFreshDbInvalidationDatabaseEvidence {
    Present {
        context: DatabaseEvidence,
        preimage: db::state::ExactFreshTransitionPreimage,
    },
    JointlyAbsent {
        context: DatabaseEvidence,
        absence: db::state::ExactFreshTransitionAbsence,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FreshDbInvalidationDatabaseKind {
    Present,
    JointlyAbsent,
}

enum DatabaseInspection {
    Exact(UsrRollbackFreshDbInvalidationDatabaseEvidence),
    Incompatible(DatabaseEvidence),
}

impl UsrRollbackFreshDbInvalidationDatabaseEvidence {
    fn kind(&self) -> FreshDbInvalidationDatabaseKind {
        match self {
            Self::Present { .. } => FreshDbInvalidationDatabaseKind::Present,
            Self::JointlyAbsent { .. } => FreshDbInvalidationDatabaseKind::JointlyAbsent,
        }
    }

    fn context(&self) -> &DatabaseEvidence {
        match self {
            Self::Present { context, .. } | Self::JointlyAbsent { context, .. } => context,
        }
    }
}

impl<'reservation> UsrRollbackFreshDbInvalidationAuthority<'reservation> {
    /// Capture exact intent evidence without effects.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackFreshDbInvalidationSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackFreshDbInvalidationAdmission<'reservation>, UsrRollbackFreshDbInvalidationAuthorityError>
    {
        if record.phase != Phase::FreshDbInvalidationIntent || record.operation != Operation::NewState {
            return Ok(UsrRollbackFreshDbInvalidationAdmission::NotApplicable);
        }
        if record.rollback.is_none() || !fresh_db_invalidation_plan_is_exact(record) {
            return Ok(UsrRollbackFreshDbInvalidationAdmission::Deferred);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding =
            journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        installation.revalidate_mutable_namespace()?;

        let database_before = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackFreshDbInvalidationAdmission::Deferred);
            }
        };
        let namespace_inspection =
            match UsrRollbackFreshDbInvalidationNamespaceInspection::begin(
                installation,
                journal,
                &journal_record_binding,
                record,
            ) {
                Ok(inspection) => inspection,
                Err(_) => return Ok(UsrRollbackFreshDbInvalidationAdmission::Deferred),
            };
        run_between_database_captures();
        let namespace = match namespace_inspection.finish(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackFreshDbInvalidationAdmission::Deferred),
        };
        let database_after = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible(_) => {
                return Ok(UsrRollbackFreshDbInvalidationAdmission::Deferred);
            }
        };
        if database_before != database_after || !fresh_db_invalidation_plan_is_exact(record) {
            return Ok(UsrRollbackFreshDbInvalidationAdmission::Deferred);
        }

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        require_journal_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;
        let kind = database_after.kind();
        let authority = Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database: database_after,
            namespace,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        };
        Ok(match kind {
            FreshDbInvalidationDatabaseKind::Present => {
                UsrRollbackFreshDbInvalidationAdmission::Apply(UsrRollbackFreshDbInvalidationApplyAuthority {
                    evidence: authority,
                })
            }
            FreshDbInvalidationDatabaseKind::JointlyAbsent => {
                UsrRollbackFreshDbInvalidationAdmission::Finish(UsrRollbackFreshDbInvalidationFinishAuthority {
                    evidence: authority,
                })
            }
        })
    }

    /// Revalidate the fixed database kind through one binding-first
    /// DB -> namespace -> DB sandwich.
    fn revalidate(
        &self,
        journal: &TransitionJournalStore,
        expected_kind: FreshDbInvalidationDatabaseKind,
    ) -> Result<(), UsrRollbackFreshDbInvalidationAuthorityError> {
        self.require_journal_record_binding(journal)?;
        if self.database.kind() != expected_kind {
            return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::EvidenceMismatch.into());
        }
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
        if database_before != database_after
            || !fresh_db_invalidation_plan_is_exact(&self.record)
            || self.database.kind() != expected_kind
        {
            return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::EvidenceMismatch.into());
        }
        self.require_journal_record_binding(journal)?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    fn require_journal_record_binding(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackFreshDbInvalidationAuthorityError> {
        require_journal_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )
    }
}

/// Exact plan accepted by the invalidation checkpoint.
fn fresh_db_invalidation_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    record.operation == Operation::NewState
        && record.phase == Phase::FreshDbInvalidationIntent
        && record.candidate.id.is_some()
        && (matches!(
            rollback.source,
            ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged | ForwardPhase::RootLinksComplete
        ) || matches!(
            (rollback.source, record.generation),
            (ForwardPhase::SystemTriggersStarted, 17) | (ForwardPhase::SystemTriggersComplete, 18)
        ))
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

fn require_journal_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), UsrRollbackFreshDbInvalidationAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::JournalRecordBindingMismatch.into());
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, binding, record)? {
        Ok(())
    } else {
        Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::JournalRecordBindingMismatch.into())
    }
}

fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseInspection, UsrRollbackFreshDbInvalidationAuthorityError> {
    let candidate = record
        .candidate
        .id
        .map(crate::state::Id::from)
        .ok_or(UsrRollbackFreshDbInvalidationAuthorityErrorKind::EvidenceMismatch)?;
    let exact_before = state_db.inspect_exact_fresh_transition(candidate, &record.transition_id)?;
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    let exact_after = state_db.inspect_exact_fresh_transition(candidate, &record.transition_id)?;
    if exact_before != exact_after {
        return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::DatabaseChanged.into());
    }
    let evidence = match exact_after {
        db::state::ExactFreshTransitionObservation::Present(preimage) => {
            UsrRollbackFreshDbInvalidationDatabaseEvidence::Present { context, preimage }
        }
        db::state::ExactFreshTransitionObservation::JointlyAbsent(absence) => {
            UsrRollbackFreshDbInvalidationDatabaseEvidence::JointlyAbsent { context, absence }
        }
    };
    if database_pair_is_exact(record, &evidence) {
        Ok(DatabaseInspection::Exact(evidence))
    } else {
        let context = match evidence {
            UsrRollbackFreshDbInvalidationDatabaseEvidence::Present { context, .. }
            | UsrRollbackFreshDbInvalidationDatabaseEvidence::JointlyAbsent { context, .. } => context,
        };
        Ok(DatabaseInspection::Incompatible(context))
    }
}

fn database_pair_is_exact(
    record: &TransitionRecord,
    evidence: &UsrRollbackFreshDbInvalidationDatabaseEvidence,
) -> bool {
    if !database_ownership_evidence_compatible(record, evidence.context())
        || !metadata_provenance_evidence_compatible(record, evidence.context())
    {
        return false;
    }
    let Some(candidate) = record.candidate.id.map(crate::state::Id::from) else {
        return false;
    };
    match evidence {
        UsrRollbackFreshDbInvalidationDatabaseEvidence::Present { context, preimage } => {
            matches!(
                context,
                DatabaseEvidence::CandidateOwnership {
                    state,
                    ownership: db::state::TransitionOwnership::Matching,
                    provenance: Some(provenance),
                    ..
                } if *state == candidate && provenance == preimage.metadata_provenance()
            ) && preimage.state().id == candidate
                && preimage.transition_id() == &record.transition_id
        }
        UsrRollbackFreshDbInvalidationDatabaseEvidence::JointlyAbsent { context, absence } => {
            matches!(
                context,
                DatabaseEvidence::CandidateOwnership {
                    state,
                    ownership: db::state::TransitionOwnership::Missing,
                    provenance: None,
                    ..
                } if *state == candidate
            ) && absence.state_id() == candidate
                && absence.transition_id() == &record.transition_id
        }
    }
}

fn require_exact_database(
    expected: &UsrRollbackFreshDbInvalidationDatabaseEvidence,
    actual: DatabaseInspection,
) -> Result<UsrRollbackFreshDbInvalidationDatabaseEvidence, UsrRollbackFreshDbInvalidationAuthorityError> {
    match actual {
        DatabaseInspection::Exact(actual) if actual == *expected => Ok(actual),
        DatabaseInspection::Exact(_) => Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::DatabaseChanged.into()),
        DatabaseInspection::Incompatible(evidence) => {
            Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::DatabaseIncompatible {
                evidence: Box::new(evidence),
            }
            .into())
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackFreshDbInvalidationAuthorityError(
    #[from] UsrRollbackFreshDbInvalidationAuthorityErrorKind,
);

impl From<InspectionError> for UsrRollbackFreshDbInvalidationAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackFreshDbInvalidationAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<db::state::ExactFreshTransitionInspectionError> for UsrRollbackFreshDbInvalidationAuthorityError {
    fn from(source: db::state::ExactFreshTransitionInspectionError) -> Self {
        UsrRollbackFreshDbInvalidationAuthorityErrorKind::ExactInspection(source).into()
    }
}

impl From<UsrRollbackFreshDbInvalidationNamespaceError> for UsrRollbackFreshDbInvalidationAuthorityError {
    fn from(source: UsrRollbackFreshDbInvalidationNamespaceError) -> Self {
        UsrRollbackFreshDbInvalidationAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackFreshDbInvalidationAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackFreshDbInvalidationAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for UsrRollbackFreshDbInvalidationAuthorityError {
    fn from(source: StorageError) -> Self {
        UsrRollbackFreshDbInvalidationAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackFreshDbInvalidationAuthorityErrorKind {
    #[error("fresh-database invalidation authority lost its exact canonical record binding")]
    JournalRecordBindingMismatch,
    #[error("exact FreshDbInvalidationIntent evidence no longer selects its retained typestate")]
    EvidenceMismatch,
    #[error("inspect fresh-database invalidation startup context")]
    Inspection(#[source] InspectionError),
    #[error("inspect the exact fresh transition and provenance")]
    ExactInspection(#[source] db::state::ExactFreshTransitionInspectionError),
    #[error("revalidate the independent fresh-database invalidation namespace proof")]
    Namespace(#[source] UsrRollbackFreshDbInvalidationNamespaceError),
    #[error("revalidate retained mutable installation namespace around fresh-database invalidation")]
    Installation(#[source] crate::installation::Error),
    #[error("capture or revalidate the exact canonical fresh-database invalidation journal record")]
    Journal(#[source] StorageError),
    #[error("fresh-database invalidation context is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("fresh-database invalidation evidence changed across its DB -> namespace -> DB sandwich")]
    DatabaseChanged,
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_usr_rollback_fresh_db_invalidation_database_captures(
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

#[cfg(test)]
#[allow(dead_code)]
#[path = "usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod candidate_test_support;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../startup_recovery/test_support.rs"]
mod test_fixture;
#[cfg(test)]
#[path = "usr_rollback_fresh_db_invalidation_authority/tests/support.rs"]
mod test_support;
#[cfg(test)]
mod tests;
