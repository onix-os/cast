//! Sealed admission and one NewState candidate-preservation effect checkpoint.
//!
//! Admission retains exact journal, database, provenance, and independent
//! namespace evidence. It remains read-only and classifies staged/crash-prefix
//! evidence separately from already-preserved evidence. Only the exact
//! NewState target prefixes can be consumed into disjoint test-sealed create,
//! normalize, or move leases. Production dispatch, mutation, durability,
//! persistence, cleanup, and triggers remain absent.

mod effect_reconciliation;

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, TransitionJournalBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackCandidatePreserveSeal,
    startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
};
use super::{
    DatabaseEvidence, InspectionError, UsrRollbackCandidatePreserveNamespaceError,
    UsrRollbackCandidatePreserveNamespaceInspection, UsrRollbackCandidatePreserveNamespaceProof,
    UsrRollbackCandidatePreserveTopology, UsrRollbackNewStateCandidatePreserveNamespaceEffectEvidence,
    UsrRollbackNewStateTargetCreateNamespaceEvidence, UsrRollbackNewStateTargetNormalizeNamespaceEvidence,
    database_ownership_evidence_compatible, inspect_database, metadata_provenance_evidence_compatible,
};

pub(in crate::client) use effect_reconciliation::{
    UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority, UsrRollbackNewStateCandidatePreserveApplyReconciliation,
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

/// Exact staged or authorized crash-prefix evidence.
pub(in crate::client) struct UsrRollbackCandidatePreserveApplyAuthority<'reservation> {
    evidence: UsrRollbackCandidatePreserveAuthority<'reservation>,
}

/// Exact already-preserved evidence.  No persistence API exists.
pub(in crate::client) struct UsrRollbackCandidatePreserveFinishAuthority<'reservation> {
    evidence: UsrRollbackCandidatePreserveAuthority<'reservation>,
}

/// Consuming effect selection derived without exposing namespace selectors.
///
/// `Unsupported` deliberately carries no retained authority. At this
/// checkpoint it covers archived activation and ActiveReblit evidence without
/// leaking a quarantine name, descriptor, or ActiveReblit wrapper index.
#[must_use = "a consumed candidate-preservation Apply authority must be handled"]
pub(in crate::client) enum UsrRollbackCandidatePreserveApplyEffectSelection<'reservation> {
    CreateNewStateTarget(UsrRollbackNewStateCandidatePreserveCreateTargetLease<'reservation>),
    NormalizeNewStateTarget(UsrRollbackNewStateCandidatePreserveNormalizeTargetLease<'reservation>),
    MoveNewState(UsrRollbackNewStateCandidatePreserveEffectLease<'reservation>),
    Unsupported,
}

/// Exact authority retained for a future one-shot absent-target creation.
struct UsrRollbackNewStateCandidatePreserveCreateTargetEffect<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackNewStateTargetCreateNamespaceEvidence,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Opaque absent-target capability. It intentionally has no operational API.
#[must_use = "a NewState create-target lease must be reconciled by a later checkpoint"]
pub(in crate::client) struct UsrRollbackNewStateCandidatePreserveCreateTargetLease<'reservation> {
    effect: UsrRollbackNewStateCandidatePreserveCreateTargetEffect<'reservation>,
}

/// Exact authority retained for future descriptor-bound residue normalization.
struct UsrRollbackNewStateCandidatePreserveNormalizeTargetEffect<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackNewStateTargetNormalizeNamespaceEvidence,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Opaque restrictive-residue capability. It has no operational API.
#[must_use = "a NewState normalize-target lease must be reconciled by a later checkpoint"]
pub(in crate::client) struct UsrRollbackNewStateCandidatePreserveNormalizeTargetLease<'reservation> {
    effect: UsrRollbackNewStateCandidatePreserveNormalizeTargetEffect<'reservation>,
}

/// Common journal, database, namespace, and reservation evidence retained by
/// the exact empty-prefix NewState move lease.
struct UsrRollbackNewStateCandidatePreserveEffect<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackNewStateCandidatePreserveNamespaceEffectEvidence,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Consumed exact NewState + staged-candidate + empty-destination typestate.
///
/// The lease exposes no record, namespace selector, path, name, descriptor, or
/// retry accessor. Only the sealed reconciliation child can consume it.
#[must_use = "a NewState candidate-preservation move lease must be reconciled"]
pub(in crate::client) struct UsrRollbackNewStateCandidatePreserveEffectLease<'reservation> {
    effect: UsrRollbackNewStateCandidatePreserveEffect<'reservation>,
}

impl<'reservation> UsrRollbackCandidatePreserveAuthority<'reservation> {
    /// Capture is sealed and read-only. Admission still has only a test seal;
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

    fn require_journal_binding(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
        if journal.has_binding(&self.journal_binding) {
            Ok(())
        } else {
            Err(UsrRollbackCandidatePreserveAuthorityErrorKind::JournalBindingMismatch.into())
        }
    }

    /// Revalidate every retained authority after the caller has proved the
    /// per-open journal binding. Splitting this suffix prevents a generic
    /// Apply authority from consulting its retained topology before the
    /// binding check selects the correct journal store.
    fn revalidate_after_binding(
        &self,
        journal: &TransitionJournalStore,
        expected_topology: UsrRollbackCandidatePreserveTopology,
    ) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
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

    fn revalidate_kind(
        &self,
        journal: &TransitionJournalStore,
        expect_preserved: bool,
    ) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
        // This must remain the first retained-evidence observation.
        self.require_journal_binding(journal)?;
        let topology = self.namespace.topology();
        if topology.is_preserved() != expect_preserved {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into());
        }
        self.revalidate_after_binding(journal, topology)
    }

    fn into_apply_effect_selection(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackCandidatePreserveApplyEffectSelection<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        // Do not inspect `namespace.topology()` until the per-open journal
        // binding succeeds. A mixed store must never select an effect type.
        self.require_journal_binding(journal)?;
        let topology = self.namespace.topology();
        if topology.is_preserved() {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into());
        }
        self.revalidate_after_binding(journal, topology)?;

        match topology {
            UsrRollbackCandidatePreserveTopology::NewStateStaged => {
                let Self {
                    installation,
                    state_db,
                    record,
                    database,
                    namespace,
                    journal_binding,
                    _active_state_reservation,
                } = self;
                let namespace = namespace.into_new_state_target_create_evidence(&record)?;
                Ok(UsrRollbackCandidatePreserveApplyEffectSelection::CreateNewStateTarget(
                    UsrRollbackNewStateCandidatePreserveCreateTargetLease {
                        effect: UsrRollbackNewStateCandidatePreserveCreateTargetEffect {
                            installation,
                            state_db,
                            record,
                            database,
                            namespace,
                            journal_binding,
                            _active_state_reservation,
                        },
                    },
                ))
            }
            UsrRollbackCandidatePreserveTopology::NewStateStagedWithTargetResidue => {
                let Self {
                    installation,
                    state_db,
                    record,
                    database,
                    namespace,
                    journal_binding,
                    _active_state_reservation,
                } = self;
                let namespace = namespace.into_new_state_target_normalize_evidence(&record)?;
                Ok(
                    UsrRollbackCandidatePreserveApplyEffectSelection::NormalizeNewStateTarget(
                        UsrRollbackNewStateCandidatePreserveNormalizeTargetLease {
                            effect: UsrRollbackNewStateCandidatePreserveNormalizeTargetEffect {
                                installation,
                                state_db,
                                record,
                                database,
                                namespace,
                                journal_binding,
                                _active_state_reservation,
                            },
                        },
                    ),
                )
            }
            UsrRollbackCandidatePreserveTopology::NewStateStagedWithEmptyQuarantine => {
                let Self {
                    installation,
                    state_db,
                    record,
                    database,
                    namespace,
                    journal_binding,
                    _active_state_reservation,
                } = self;
                let namespace = namespace.into_new_state_move_effect_evidence(&record)?;
                Ok(UsrRollbackCandidatePreserveApplyEffectSelection::MoveNewState(
                    UsrRollbackNewStateCandidatePreserveEffectLease {
                        effect: UsrRollbackNewStateCandidatePreserveEffect {
                            installation,
                            state_db,
                            record,
                            database,
                            namespace,
                            journal_binding,
                            _active_state_reservation,
                        },
                    },
                ))
            }
            UsrRollbackCandidatePreserveTopology::NewStatePreserved
            | UsrRollbackCandidatePreserveTopology::ArchivedStagedWithCanonicalSlot
            | UsrRollbackCandidatePreserveTopology::ArchivedPreserved
            | UsrRollbackCandidatePreserveTopology::ActiveReblitStaged { .. }
            | UsrRollbackCandidatePreserveTopology::ActiveReblitPreserved { .. } => {
                Ok(UsrRollbackCandidatePreserveApplyEffectSelection::Unsupported)
            }
        }
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
        self.evidence.revalidate_kind(journal, false)
    }

    /// Consume generic Apply admission into one exact target-prefix lease or a
    /// fieldless unsupported result. Possessing admission is insufficient:
    /// production cannot construct the distinct effect seal at this checkpoint.
    pub(in crate::client) fn into_effect_selection(
        self,
        _effect_seal: &UsrRollbackCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackCandidatePreserveApplyEffectSelection<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        self.evidence.into_apply_effect_selection(journal)
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
        self.evidence.revalidate_kind(journal, true)
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
    #[error("read the exact candidate-preservation journal during effect reconciliation")]
    JournalReadDuringEffect(#[source] crate::transition_journal::StorageError),
    #[error("the exact candidate-preservation journal changed during effect reconciliation")]
    JournalChangedDuringEffect,
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
