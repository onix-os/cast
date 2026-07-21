//! Sealed read-only authority for routing an ActiveReblit
//! `CandidatePreserved` record to `BootRepairRequired`.
//!
//! Admission is deliberately disjoint from ordinary rollback completion. It
//! accepts only the exact `BootSyncStarted` rollback source whose boot repair
//! remains `PendingUnverifiable`. The authority retains the cleared
//! existing-state row and metadata provenance, the preserved whole-wrapper
//! namespace, exact journal binding, installation capability, and active-state
//! reservation. It exposes no boot, database, namespace, journal, cleanup,
//! retry, or finalization effect.

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, CodecError, ForwardPhase, Operation, Phase, RollbackAction,
        TransitionJournalBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackActiveReblitBootRepairRequiredSeal,
};
use super::{
    DatabaseEvidence, InspectionError, UsrRollbackActiveReblitBootRepairRequiredNamespaceError,
    UsrRollbackActiveReblitBootRepairRequiredNamespaceInspection,
    UsrRollbackActiveReblitBootRepairRequiredNamespaceProof, database_ownership_evidence_compatible, inspect_database,
    metadata_provenance_evidence_compatible,
};

/// Exact result of read-only ActiveReblit boot-repair-required admission.
pub(in crate::client) enum UsrRollbackActiveReblitBootRepairRequiredAdmission<'reservation> {
    NotApplicable,
    Deferred,
    /// A v3 record is correlated to the exact pending state-database receipt.
    ReadyAuthenticated(UsrRollbackActiveReblitBootRepairRequiredAuthority<'reservation>),
    /// A v1/v2 record carries no receipt and may take only the existing
    /// journal-only route toward conservative unverified recovery.
    ReadyLegacyUnverified(UsrRollbackActiveReblitBootRepairRequiredAuthority<'reservation>),
}

/// Retained evidence authorizing only the next journal-only boot-repair route.
pub(in crate::client) struct UsrRollbackActiveReblitBootRepairRequiredAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: UsrRollbackActiveReblitBootRepairRequiredDatabaseEvidence,
    namespace: UsrRollbackActiveReblitBootRepairRequiredNamespaceProof,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Exact existing-state observation bound to this route. This type is
/// intentionally not `Clone`.
#[derive(Debug, Eq, PartialEq)]
struct UsrRollbackActiveReblitBootRepairRequiredDatabaseEvidence {
    context: DatabaseEvidence,
    receipt_head: db::state::BootPublicationReceiptHead,
    receipt_correlation: BootPublicationReceiptCorrelation,
}

enum DatabaseInspection {
    Exact(UsrRollbackActiveReblitBootRepairRequiredDatabaseEvidence),
    Incompatible {
        context: DatabaseEvidence,
        receipt_head: db::state::BootPublicationReceiptHead,
    },
}

#[derive(Debug, Eq, PartialEq)]
enum BootPublicationReceiptCorrelation {
    Authenticated,
    LegacyUnverified,
}

impl<'reservation> UsrRollbackActiveReblitBootRepairRequiredAuthority<'reservation> {
    /// Capture the exact durable ActiveReblit `CandidatePreserved` boot plan
    /// without effects. Only the phase-specific startup child can construct
    /// the production seal.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackActiveReblitBootRepairRequiredSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitBootRepairRequiredAdmission<'reservation>,
        UsrRollbackActiveReblitBootRepairRequiredAuthorityError,
    > {
        if record.operation != Operation::ActiveReblit || record.phase != Phase::CandidatePreserved {
            return Ok(UsrRollbackActiveReblitBootRepairRequiredAdmission::NotApplicable);
        }
        if record.rollback.is_none() || !active_reblit_boot_repair_required_plan_is_exact(record) {
            return Ok(UsrRollbackActiveReblitBootRepairRequiredAdmission::Deferred);
        }

        let journal_binding = journal.binding();
        if !journal.has_binding(&journal_binding) {
            return Err(UsrRollbackActiveReblitBootRepairRequiredAuthorityErrorKind::JournalBindingMismatch.into());
        }
        installation.revalidate_mutable_namespace()?;

        let database_before = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible { .. } => {
                return Ok(UsrRollbackActiveReblitBootRepairRequiredAdmission::Deferred);
            }
        };
        let namespace_inspection =
            match UsrRollbackActiveReblitBootRepairRequiredNamespaceInspection::begin(installation, journal, record) {
                Ok(inspection) => inspection,
                Err(_) => return Ok(UsrRollbackActiveReblitBootRepairRequiredAdmission::Deferred),
            };
        run_between_database_captures();
        let namespace = match namespace_inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackActiveReblitBootRepairRequiredAdmission::Deferred),
        };
        let database_after = match inspect_current_database(record, state_db)? {
            DatabaseInspection::Exact(database) => database,
            DatabaseInspection::Incompatible { .. } => {
                return Ok(UsrRollbackActiveReblitBootRepairRequiredAdmission::Deferred);
            }
        };
        if database_before != database_after || !active_reblit_boot_repair_required_plan_is_exact(record) {
            return Ok(UsrRollbackActiveReblitBootRepairRequiredAdmission::Deferred);
        }

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        let admission = Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database: database_after,
            namespace,
            journal_binding,
            _active_state_reservation: active_state_reservation,
        };
        Ok(match &admission.database.receipt_correlation {
            BootPublicationReceiptCorrelation::Authenticated => {
                UsrRollbackActiveReblitBootRepairRequiredAdmission::ReadyAuthenticated(admission)
            }
            BootPublicationReceiptCorrelation::LegacyUnverified => {
                UsrRollbackActiveReblitBootRepairRequiredAdmission::ReadyLegacyUnverified(admission)
            }
        })
    }

    /// Revalidate the exact binding-first DB -> namespace -> DB sandwich.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackActiveReblitBootRepairRequiredAuthorityError> {
        if !journal.has_binding(&self.journal_binding) {
            return Err(UsrRollbackActiveReblitBootRepairRequiredAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        let database_before =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        self.namespace.revalidate(&self.installation, journal, &self.record)?;
        let database_after =
            require_exact_database(&self.database, inspect_current_database(&self.record, &self.state_db)?)?;
        if database_before != database_after || !active_reblit_boot_repair_required_plan_is_exact(&self.record) {
            return Err(UsrRollbackActiveReblitBootRepairRequiredAuthorityErrorKind::RouteEvidenceMismatch.into());
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

    #[cfg(test)]
    pub(in crate::client) fn wrapper_index(&self) -> usize {
        self.namespace.wrapper_index()
    }
}

/// Exact narrow plan accepted by the ActiveReblit boot-repair boundary.
fn active_reblit_boot_repair_required_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    record.operation == Operation::ActiveReblit
        && record.phase == Phase::CandidatePreserved
        && record.candidate.id.is_some()
        && record.candidate.id == record.previous.id
        && rollback.source == ForwardPhase::BootSyncStarted
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
        && rollback.boot == BootRollback::PendingUnverifiable
        && rollback.external_effects_may_remain
}

/// Inspect exact existing-state evidence around the general startup context
/// so no candidate row, ownership, or provenance observation can be paired
/// with a different database moment.
fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseInspection, UsrRollbackActiveReblitBootRepairRequiredAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    let receipt_head = state_db.boot_publication_receipt_head()?;
    let receipt_correlation = match record.boot_publication_receipt_correlation()? {
        Some(pair) if receipt_head.receipt_pair_for(&record.transition_id) == Some(pair) => {
            Some(BootPublicationReceiptCorrelation::Authenticated)
        }
        None if receipt_head.pending().is_none() => Some(BootPublicationReceiptCorrelation::LegacyUnverified),
        Some(_) | None => None,
    };
    if active_reblit_database_pair_is_exact(record, &context) && receipt_correlation.is_some() {
        Ok(DatabaseInspection::Exact(
            UsrRollbackActiveReblitBootRepairRequiredDatabaseEvidence {
                context,
                receipt_head,
                receipt_correlation: receipt_correlation.expect("checked exact receipt correlation"),
            },
        ))
    } else {
        Ok(DatabaseInspection::Incompatible {
            context,
            receipt_head,
        })
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
    expected: &UsrRollbackActiveReblitBootRepairRequiredDatabaseEvidence,
    actual: DatabaseInspection,
) -> Result<
    UsrRollbackActiveReblitBootRepairRequiredDatabaseEvidence,
    UsrRollbackActiveReblitBootRepairRequiredAuthorityError,
> {
    match actual {
        DatabaseInspection::Exact(actual) if actual == *expected => Ok(actual),
        DatabaseInspection::Exact(_) => {
            Err(UsrRollbackActiveReblitBootRepairRequiredAuthorityErrorKind::DatabaseChanged.into())
        }
        DatabaseInspection::Incompatible {
            context,
            receipt_head,
        } => Err(
            UsrRollbackActiveReblitBootRepairRequiredAuthorityErrorKind::DatabaseIncompatible {
                evidence: Box::new(context),
                receipt_head: Box::new(receipt_head),
            }
            .into(),
        ),
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackActiveReblitBootRepairRequiredAuthorityError(
    #[from] UsrRollbackActiveReblitBootRepairRequiredAuthorityErrorKind,
);

impl From<InspectionError> for UsrRollbackActiveReblitBootRepairRequiredAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackActiveReblitBootRepairRequiredAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<db::state::BootPublicationReceiptHeadError>
    for UsrRollbackActiveReblitBootRepairRequiredAuthorityError
{
    fn from(source: db::state::BootPublicationReceiptHeadError) -> Self {
        UsrRollbackActiveReblitBootRepairRequiredAuthorityErrorKind::ReceiptHead(source).into()
    }
}

impl From<CodecError> for UsrRollbackActiveReblitBootRepairRequiredAuthorityError {
    fn from(source: CodecError) -> Self {
        UsrRollbackActiveReblitBootRepairRequiredAuthorityErrorKind::Record(source).into()
    }
}

impl From<UsrRollbackActiveReblitBootRepairRequiredNamespaceError>
    for UsrRollbackActiveReblitBootRepairRequiredAuthorityError
{
    fn from(source: UsrRollbackActiveReblitBootRepairRequiredNamespaceError) -> Self {
        UsrRollbackActiveReblitBootRepairRequiredAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackActiveReblitBootRepairRequiredAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackActiveReblitBootRepairRequiredAuthorityErrorKind::Installation(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackActiveReblitBootRepairRequiredAuthorityErrorKind {
    #[error("ActiveReblit boot-repair-required authority was paired with a different open journal store")]
    JournalBindingMismatch,
    #[error("exact ActiveReblit CandidatePreserved evidence no longer selects boot repair")]
    RouteEvidenceMismatch,
    #[error("inspect ActiveReblit boot-repair-required startup database context")]
    Inspection(#[source] InspectionError),
    #[error("inspect exact ActiveReblit boot-publication receipt head")]
    ReceiptHead(#[source] db::state::BootPublicationReceiptHeadError),
    #[error("validate ActiveReblit boot-publication receipt correlation in the journal")]
    Record(#[source] CodecError),
    #[error("revalidate the independent ActiveReblit boot-repair-required namespace proof")]
    Namespace(#[source] UsrRollbackActiveReblitBootRepairRequiredNamespaceError),
    #[error("revalidate retained mutable installation namespace around ActiveReblit boot-repair-required routing")]
    Installation(#[source] crate::installation::Error),
    #[error(
        "ActiveReblit boot-repair-required database context or receipt correlation is incompatible: context={evidence:?}, receipt_head={receipt_head:?}"
    )]
    DatabaseIncompatible {
        evidence: Box<DatabaseEvidence>,
        receipt_head: Box<db::state::BootPublicationReceiptHead>,
    },
    #[error("ActiveReblit boot-repair-required database evidence changed across its DB -> namespace -> DB sandwich")]
    DatabaseChanged,
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_usr_rollback_active_reblit_boot_repair_required_database_captures(
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
