//! Sealed evidence authority for persisting one journal-only rollback decision.

use crate::{
    Installation, db,
    transition_journal::{
        InitialRollbackAction, Operation, Phase, RollbackObservations, TransitionJournalBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackDecisionSeal,
    startup_recovery::UsrExchangeParentDurabilityCompletionSeal,
};
use super::{
    DatabaseEvidence, InspectionError, UsrExchangeLayout, UsrRollbackDecisionNamespaceError,
    UsrRollbackDecisionNamespaceInspection, UsrRollbackDecisionNamespaceProof, database_ownership_evidence_compatible,
    inspect_database, metadata_provenance_evidence_compatible,
};

/// Result of asking whether the exact startup evidence admits the narrow
/// journal-only rollback-decision slice.
#[allow(dead_code)] // deferral detail is consumed by focused startup contracts
pub(in crate::client) enum UsrRollbackDecisionAdmission<'reservation> {
    NotApplicable,
    Deferred(UsrRollbackDecisionDeferral),
    ParentDurabilityRequired(UsrExchangeParentDurabilityAuthority<'reservation>),
    Ready(UsrRollbackDecisionAuthority<'reservation>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum UsrRollbackDecisionDeferral {
    IncompatibleEvidence,
}

/// Exact, retained database/namespace evidence plus the cooperating-writer
/// reservation under which the evidence was captured.
///
/// The reservation is intentionally not interpreted as an active-selection
/// witness. Its only role here is to keep cooperating namespace writers
/// excluded until the executor either persists the decision or fails stop.
pub(in crate::client) struct UsrRollbackDecisionAuthority<'reservation> {
    evidence: UsrRollbackDecisionEvidence<'reservation>,
    observations: RollbackObservations,
}

/// Exact Intent+POST evidence which may become rollback-decision authority
/// only after both exchange-parent durability barriers complete.
pub(in crate::client) struct UsrExchangeParentDurabilityAuthority<'reservation> {
    evidence: UsrRollbackDecisionEvidence<'reservation>,
}

/// Evidence shared by direct rollback-decision admission and the narrower
/// parent-durability normalization typestate.
struct UsrRollbackDecisionEvidence<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackDecisionNamespaceProof,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> UsrRollbackDecisionAuthority<'reservation> {
    /// Capture authority only when presented with the unforgeable startup-gate
    /// seal. Safe code outside that writer-first gate cannot admit persistence.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackDecisionSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<UsrRollbackDecisionAdmission<'reservation>, UsrRollbackDecisionAuthorityError> {
        Self::capture_from_context(
            installation,
            journal,
            state_db,
            active_state_reservation,
            record,
            initial_in_flight,
        )
    }

    fn capture_from_context(
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<UsrRollbackDecisionAdmission<'reservation>, UsrRollbackDecisionAuthorityError> {
        if !matches!(record.phase, Phase::UsrExchangeIntent | Phase::UsrExchanged) {
            return Ok(UsrRollbackDecisionAdmission::NotApplicable);
        }

        let journal_binding = journal.binding();
        installation.revalidate_mutable_namespace()?;
        let namespace_inspection = match UsrRollbackDecisionNamespaceInspection::begin(installation, journal, record) {
            Ok(inspection) => inspection,
            Err(_) => {
                return Ok(UsrRollbackDecisionAdmission::Deferred(
                    UsrRollbackDecisionDeferral::IncompatibleEvidence,
                ));
            }
        };
        let database = inspect_database(record, state_db, initial_in_flight)?;
        if !database_is_compatible(record, &database) {
            return Ok(UsrRollbackDecisionAdmission::Deferred(
                UsrRollbackDecisionDeferral::IncompatibleEvidence,
            ));
        }

        run_between_initial_database_captures();
        let in_flight_after = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
        let database_after = inspect_database(record, state_db, in_flight_after)?;
        if !database_is_compatible(record, &database_after) || database != database_after {
            return Ok(UsrRollbackDecisionAdmission::Deferred(
                UsrRollbackDecisionDeferral::IncompatibleEvidence,
            ));
        }
        let namespace = match namespace_inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(_) => {
                return Ok(UsrRollbackDecisionAdmission::Deferred(
                    UsrRollbackDecisionDeferral::IncompatibleEvidence,
                ));
            }
        };

        let usr_exchange = match (record.phase, namespace.layout()) {
            (Phase::UsrExchangeIntent, UsrExchangeLayout::Pre) => Some(InitialRollbackAction::AlreadySatisfied),
            (Phase::UsrExchangeIntent, UsrExchangeLayout::Post) => None,
            (Phase::UsrExchanged, UsrExchangeLayout::Post) => Some(InitialRollbackAction::Pending),
            (Phase::UsrExchanged, UsrExchangeLayout::Pre) => {
                return Ok(UsrRollbackDecisionAdmission::Deferred(
                    UsrRollbackDecisionDeferral::IncompatibleEvidence,
                ));
            }
            _ => unreachable!("rollback-decision admission is restricted to /usr exchange phases"),
        };
        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        let evidence = UsrRollbackDecisionEvidence {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database,
            namespace,
            journal_binding,
            _active_state_reservation: active_state_reservation,
        };
        Ok(match usr_exchange {
            Some(usr_exchange) => UsrRollbackDecisionAdmission::Ready(Self {
                observations: rollback_observations(record.operation, usr_exchange),
                evidence,
            }),
            None => UsrRollbackDecisionAdmission::ParentDurabilityRequired(UsrExchangeParentDurabilityAuthority {
                evidence,
            }),
        })
    }

    /// Revalidate the owned source record, retained namespace inventories, and
    /// an exact database/namespace/database sandwich immediately around use.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackDecisionAuthorityError> {
        self.evidence.revalidate(journal)
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.evidence.installation
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.evidence.record
    }

    pub(in crate::client) fn observations(&self) -> RollbackObservations {
        self.observations
    }
}

impl UsrRollbackDecisionEvidence<'_> {
    /// Revalidate the owned source record, retained namespace inventories, and
    /// an exact database/namespace/database sandwich immediately around use.
    fn revalidate(&self, journal: &TransitionJournalStore) -> Result<(), UsrRollbackDecisionAuthorityError> {
        // Per-open journal identity is deliberately the first check. No
        // namespace, database, or durability action may run for a mixed store.
        if !journal.has_binding(&self.journal_binding) {
            return Err(UsrRollbackDecisionAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        let database_before = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_before)?;
        self.namespace.revalidate(&self.installation, journal, &self.record)?;
        let database_after = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_after)?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

impl<'reservation> UsrExchangeParentDurabilityAuthority<'reservation> {
    /// Revalidate exact Intent+POST normalization authority. The shared
    /// evidence routine performs the per-open journal binding check first.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackDecisionAuthorityError> {
        self.evidence.revalidate(journal)?;
        self.require_intent_post()
    }

    /// Apply only the retained staging-parent durability barrier.
    pub(in crate::client) fn sync_retained_staging_parent(
        &self,
        before_sync: impl FnOnce() -> std::io::Result<()>,
    ) -> Result<(u64, u64), UsrRollbackDecisionAuthorityError> {
        self.evidence
            .namespace
            .sync_retained_staging_parent(before_sync)
            .map_err(UsrRollbackDecisionAuthorityError::from)
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.evidence.installation
    }

    /// Consume completed normalization authority into the existing sealed
    /// rollback-decision capability. Only the normalizer can construct the
    /// completion seal.
    pub(in crate::client) fn complete(
        self,
        _seal: UsrExchangeParentDurabilityCompletionSeal,
    ) -> Result<UsrRollbackDecisionAuthority<'reservation>, UsrRollbackDecisionAuthorityError> {
        self.require_intent_post()?;
        let operation = self.evidence.record.operation;
        Ok(UsrRollbackDecisionAuthority {
            evidence: self.evidence,
            observations: rollback_observations(operation, InitialRollbackAction::Pending),
        })
    }

    fn require_intent_post(&self) -> Result<(), UsrRollbackDecisionAuthorityError> {
        if self.evidence.record.phase == Phase::UsrExchangeIntent
            && self.evidence.namespace.layout() == UsrExchangeLayout::Post
        {
            Ok(())
        } else {
            Err(UsrRollbackDecisionAuthorityErrorKind::ParentDurabilitySourceMismatch.into())
        }
    }
}

fn rollback_observations(operation: Operation, usr_exchange: InitialRollbackAction) -> RollbackObservations {
    RollbackObservations {
        allocated_candidate_id: None,
        previous_archive: None,
        usr_exchange: Some(usr_exchange),
        candidate: InitialRollbackAction::Pending,
        fresh_db: (operation == Operation::NewState).then_some(InitialRollbackAction::Pending),
    }
}

fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseEvidence, UsrRollbackDecisionAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let evidence = inspect_database(record, state_db, in_flight)?;
    if database_is_compatible(record, &evidence) {
        Ok(evidence)
    } else {
        Err(UsrRollbackDecisionAuthorityErrorKind::DatabaseIncompatible {
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
) -> Result<(), UsrRollbackDecisionAuthorityError> {
    if *expected == actual {
        Ok(())
    } else {
        Err(UsrRollbackDecisionAuthorityErrorKind::DatabaseChanged {
            expected: Box::new(expected.clone()),
            actual: Box::new(actual),
        }
        .into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackDecisionAuthorityError(#[from] UsrRollbackDecisionAuthorityErrorKind);

impl From<InspectionError> for UsrRollbackDecisionAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackDecisionAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<UsrRollbackDecisionNamespaceError> for UsrRollbackDecisionAuthorityError {
    fn from(source: UsrRollbackDecisionNamespaceError) -> Self {
        UsrRollbackDecisionAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackDecisionAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackDecisionAuthorityErrorKind::Installation(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackDecisionAuthorityErrorKind {
    #[error("startup rollback-decision authority was paired with a different open journal store")]
    JournalBindingMismatch,
    #[error("startup parent-durability authority is not bound to exact UsrExchangeIntent + POST evidence")]
    ParentDurabilitySourceMismatch,
    #[error("inspect exact rollback-decision database evidence")]
    Inspection(#[source] InspectionError),
    #[error("revalidate the independent rollback-decision namespace proof")]
    Namespace(#[source] UsrRollbackDecisionNamespaceError),
    #[error("revalidate the retained mutable installation namespace around rollback-decision authority")]
    Installation(#[source] crate::installation::Error),
    #[error("rollback-decision database evidence is incompatible with the persisted source: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("rollback-decision database evidence changed from {expected:?} to {actual:?}")]
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
#[allow(dead_code)] // armed by focused rollback-decision race contracts
pub(in crate::client) fn arm_between_usr_rollback_decision_database_captures(hook: impl FnOnce() + 'static) {
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
