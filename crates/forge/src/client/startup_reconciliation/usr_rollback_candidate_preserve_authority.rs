//! Sealed, read-only admission for one candidate-preservation checkpoint.
//!
//! Admission retains exact journal, database, provenance, and independent
//! namespace evidence.  It classifies staged/crash-prefix evidence separately
//! from already-preserved evidence, but deliberately exposes no effect,
//! persistence, cleanup, trigger, or startup-dispatch capability.

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, TransitionJournalBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackCandidatePreserveSeal};
use super::{
    DatabaseEvidence, InspectionError, UsrRollbackCandidatePreserveNamespaceError,
    UsrRollbackCandidatePreserveNamespaceInspection, UsrRollbackCandidatePreserveNamespaceProof,
    UsrRollbackCandidatePreserveTopology, database_ownership_evidence_compatible, inspect_database,
    metadata_provenance_evidence_compatible,
};

/// Exact result of read-only candidate-preservation admission.
pub(in crate::client) enum UsrRollbackCandidatePreserveAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Apply(UsrRollbackCandidatePreserveApplyAuthority<'reservation>),
    Finish(UsrRollbackCandidatePreserveFinishAuthority<'reservation>),
}

/// Common evidence retained privately behind staged/preserved typestates.
pub(in crate::client) struct UsrRollbackCandidatePreserveAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackCandidatePreserveNamespaceProof,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Exact staged or authorized crash-prefix evidence.  No effect API exists.
pub(in crate::client) struct UsrRollbackCandidatePreserveApplyAuthority<'reservation> {
    evidence: UsrRollbackCandidatePreserveAuthority<'reservation>,
}

/// Exact already-preserved evidence.  No persistence API exists.
pub(in crate::client) struct UsrRollbackCandidatePreserveFinishAuthority<'reservation> {
    evidence: UsrRollbackCandidatePreserveAuthority<'reservation>,
}

impl<'reservation> UsrRollbackCandidatePreserveAuthority<'reservation> {
    /// Capture is sealed and read-only.  Checkpoint one has only a test seal;
    /// production startup cannot yet construct or dispatch this authority.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackCandidatePreserveSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<UsrRollbackCandidatePreserveAdmission<'reservation>, UsrRollbackCandidatePreserveAuthorityError> {
        if record.phase != Phase::CandidatePreserveIntent {
            return Ok(UsrRollbackCandidatePreserveAdmission::NotApplicable);
        }
        let Some(rollback) = record.rollback.as_ref() else {
            return Ok(UsrRollbackCandidatePreserveAdmission::Deferred);
        };
        if !matches!(
            rollback.source,
            ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged
        ) {
            return Ok(UsrRollbackCandidatePreserveAdmission::NotApplicable);
        }

        let journal_binding = journal.binding();
        installation.revalidate_mutable_namespace()?;
        let namespace_inspection =
            match UsrRollbackCandidatePreserveNamespaceInspection::begin(installation, journal, record) {
                Ok(inspection) => inspection,
                Err(_) => return Ok(UsrRollbackCandidatePreserveAdmission::Deferred),
            };
        let database = inspect_database(record, state_db, initial_in_flight)?;
        if !database_is_compatible(record, &database) || !candidate_preserve_plan_is_exact(record) {
            return Ok(UsrRollbackCandidatePreserveAdmission::Deferred);
        }

        run_between_initial_database_captures();
        let in_flight_after = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
        let database_after = inspect_database(record, state_db, in_flight_after)?;
        if !database_is_compatible(record, &database_after) || database != database_after {
            return Ok(UsrRollbackCandidatePreserveAdmission::Deferred);
        }
        let namespace = match namespace_inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackCandidatePreserveAdmission::Deferred),
        };

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        let topology = namespace.topology();
        let authority = Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database,
            namespace,
            journal_binding,
            _active_state_reservation: active_state_reservation,
        };
        Ok(if topology.is_preserved() {
            UsrRollbackCandidatePreserveAdmission::Finish(UsrRollbackCandidatePreserveFinishAuthority {
                evidence: authority,
            })
        } else {
            UsrRollbackCandidatePreserveAdmission::Apply(UsrRollbackCandidatePreserveApplyAuthority {
                evidence: authority,
            })
        })
    }

    fn revalidate(
        &self,
        journal: &TransitionJournalStore,
        expected_topology: UsrRollbackCandidatePreserveTopology,
    ) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
        if !journal.has_binding(&self.journal_binding) {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        let database_before = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_before)?;
        self.namespace.revalidate(&self.installation, journal, &self.record)?;
        let database_after = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_after)?;
        if !candidate_preserve_plan_is_exact(&self.record) || self.namespace.topology() != expected_topology {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

impl<'reservation> UsrRollbackCandidatePreserveApplyAuthority<'reservation> {
    #[cfg(test)]
    pub(in crate::client::startup_reconciliation) fn topology(&self) -> UsrRollbackCandidatePreserveTopology {
        self.evidence.namespace.topology()
    }

    /// Revalidate only the retained staged/crash-prefix typestate.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
        let topology = self.evidence.namespace.topology();
        if topology.is_preserved() {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into());
        }
        self.evidence.revalidate(journal, topology)
    }
}

impl<'reservation> UsrRollbackCandidatePreserveFinishAuthority<'reservation> {
    #[cfg(test)]
    pub(in crate::client::startup_reconciliation) fn topology(&self) -> UsrRollbackCandidatePreserveTopology {
        self.evidence.namespace.topology()
    }

    /// Revalidate only the retained already-preserved typestate.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
        let topology = self.evidence.namespace.topology();
        if !topology.is_preserved() {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into());
        }
        self.evidence.revalidate(journal, topology)
    }
}

fn candidate_preserve_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    if record.phase != Phase::CandidatePreserveIntent
        || !matches!(
            rollback.source,
            ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged
        )
        || rollback.previous_archive != RollbackAction::NotRequired
        || !matches!(
            rollback.usr_exchange,
            RollbackAction::Applied | RollbackAction::AlreadySatisfied
        )
        || rollback.candidate.action != RollbackAction::Pending
        || rollback.boot != BootRollback::NotRequired
    {
        return false;
    }
    let fresh_is_exact = match record.operation {
        Operation::NewState => rollback.fresh_db == RollbackAction::Pending,
        Operation::ActivateArchived | Operation::ActiveReblit => rollback.fresh_db == RollbackAction::NotRequired,
    };
    let disposition_is_exact = match record.operation {
        Operation::ActivateArchived => rollback.candidate.disposition == AbortDisposition::Rearchive,
        Operation::NewState | Operation::ActiveReblit => rollback.candidate.disposition == AbortDisposition::Quarantine,
    };
    fresh_is_exact
        && disposition_is_exact
        && rollback.external_effects_may_remain == (record.operation != Operation::ActivateArchived)
}

fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseEvidence, UsrRollbackCandidatePreserveAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let evidence = inspect_database(record, state_db, in_flight)?;
    if database_is_compatible(record, &evidence) {
        Ok(evidence)
    } else {
        Err(UsrRollbackCandidatePreserveAuthorityErrorKind::DatabaseIncompatible {
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
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    if *expected == actual {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveAuthorityErrorKind::DatabaseChanged {
            expected: Box::new(expected.clone()),
            actual: Box::new(actual),
        }
        .into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackCandidatePreserveAuthorityError(
    #[from] UsrRollbackCandidatePreserveAuthorityErrorKind,
);

impl From<InspectionError> for UsrRollbackCandidatePreserveAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackCandidatePreserveAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<UsrRollbackCandidatePreserveNamespaceError> for UsrRollbackCandidatePreserveAuthorityError {
    fn from(source: UsrRollbackCandidatePreserveNamespaceError) -> Self {
        UsrRollbackCandidatePreserveAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackCandidatePreserveAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackCandidatePreserveAuthorityErrorKind::Installation(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackCandidatePreserveAuthorityErrorKind {
    #[error("candidate-preservation authority was paired with a different open journal store")]
    JournalBindingMismatch,
    #[error("exact candidate-preservation evidence no longer selects its retained typestate")]
    EvidenceMismatch,
    #[error("inspect exact candidate-preservation database evidence")]
    Inspection(#[source] InspectionError),
    #[error("revalidate the independent candidate-preservation namespace proof")]
    Namespace(#[source] UsrRollbackCandidatePreserveNamespaceError),
    #[error("revalidate retained mutable installation namespace around candidate-preservation authority")]
    Installation(#[source] crate::installation::Error),
    #[error("candidate-preservation database evidence is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("candidate-preservation database evidence changed from {expected:?} to {actual:?}")]
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
pub(in crate::client) fn arm_between_usr_rollback_candidate_preserve_database_captures(hook: impl FnOnce() + 'static) {
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

#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider startup-recovery helpers
#[path = "../startup_recovery/test_support.rs"]
mod test_fixture;
#[cfg(test)]
#[path = "usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod test_support;
#[cfg(test)]
mod tests;
