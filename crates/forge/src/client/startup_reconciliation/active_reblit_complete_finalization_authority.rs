//! Sealed authority for exact forward ActiveReblit `Complete` finalization.
//!
//! This is deliberately separate from rollback finalization. Admission binds
//! the exact v3 terminal record before retaining its current exact receipt-chain
//! state (which may be empty), cleared existing-state database context, full
//! selected state, active-state selection, and completed cleanup namespace. Its
//! only mutation surface is one record-bound terminal deletion on the same
//! locked journal store.

use crate::{
    Installation, State, db, state,
    transition_journal::{
        CodecError, Operation, Phase, StorageError, TransitionJournalBinding,
        TransitionJournalRecordBinding, TransitionJournalRecordDeleteError,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::{ActiveStateReservation, ActiveStateSnapshot},
    startup_gate::ActiveReblitCompleteFinalizationSeal,
};
use super::{
    DatabaseEvidence, InspectionError, database_ownership_evidence_compatible,
    inspect_database, metadata_provenance_evidence_compatible,
};
use super::activation_namespace::{
    ActiveReblitCommitCleanupFinishNamespaceProof,
    ActiveReblitCommitCleanupNamespaceError,
    ActiveReblitCommitCleanupNamespaceInspection,
    ActiveReblitCommitCleanupNamespaceProof,
    active_reblit_commit_cleanup_namespace_error_is_mismatch,
};

/// Read-only result for exact forward terminal admission.
pub(in crate::client) enum ActiveReblitCompleteFinalizationAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Ready(ActiveReblitCompleteFinalizationAuthority<'reservation>),
}

pub(in crate::client) struct ActiveReblitCompleteFinalizationAuthority<'reservation> {
    evidence: ActiveReblitCompleteFinalizationEvidence<'reservation>,
    journal_binding: TransitionJournalBinding,
    journal_record_binding: TransitionJournalRecordBinding,
}

/// One-shot evidence retained after the exact record binding is consumed by
/// the sole deletion attempt. This type intentionally is not `Clone`.
pub(in crate::client) struct ActiveReblitCompleteFinalizationAfterDeleteAuthority<'reservation> {
    evidence: ActiveReblitCompleteFinalizationEvidence<'reservation>,
    journal_binding: TransitionJournalBinding,
}

struct ActiveReblitCompleteFinalizationEvidence<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: ActiveReblitCompleteFinalizationDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: ActiveReblitCommitCleanupFinishNamespaceProof,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[derive(Debug, Eq, PartialEq)]
struct ActiveReblitCompleteFinalizationDatabaseEvidence {
    route: ActiveReblitCompleteFinalizationRouteEvidence,
    context: DatabaseEvidence,
    state: State,
}

/// Disjoint exact terminal-route evidence. A no-boot record retains the
/// current receipt chain only as inert equality evidence and never receives
/// authority to correlate or mutate it.
#[derive(Debug, Eq, PartialEq)]
enum ActiveReblitCompleteFinalizationRouteEvidence {
    ReceiptBacked {
        pair: crate::boot_publication::BootPublicationReceiptPair,
        receipt: db::state::BootPublicationReceiptState,
    },
    NoBoot {
        inert_receipt_chain: db::state::CurrentExactPromotedBootPublicationReceiptChain,
    },
}

enum ActiveReblitCompleteFinalizationRoutePlan {
    ReceiptBacked(crate::boot_publication::BootPublicationReceiptPair),
    NoBoot,
}

enum ActiveReblitCompleteFinalizationDatabaseInspection {
    Exact(ActiveReblitCompleteFinalizationDatabaseEvidence),
    Incompatible,
}

impl<'reservation> ActiveReblitCompleteFinalizationAuthority<'reservation> {
    pub(in crate::client) fn capture(
        _seal: &ActiveReblitCompleteFinalizationSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<
        ActiveReblitCompleteFinalizationAdmission<'reservation>,
        ActiveReblitCompleteFinalizationAuthorityError,
    > {
        if record.operation != Operation::ActiveReblit || record.phase != Phase::Complete {
            return Ok(ActiveReblitCompleteFinalizationAdmission::NotApplicable);
        }

        // Bind the non-clone terminal inode before reading any mutable
        // database, selected-state, or activation-namespace evidence.
        installation.revalidate_mutable_namespace()?;
        let journal_binding = journal.binding();
        let journal_record_binding = journal.record_binding(
            installation.retained_mutable_cast_directory()?,
            record,
        )?;
        installation.revalidate_mutable_namespace()?;

        let receipt_correlation = record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitCompleteFinalizationAuthorityErrorKind::Record)?;
        let Some(route_plan) = exact_route_plan(record, receipt_correlation) else {
            return Ok(ActiveReblitCompleteFinalizationAdmission::Deferred);
        };
        let database_before = match inspect_current_database_for_plan(record, &route_plan, state_db)? {
            ActiveReblitCompleteFinalizationDatabaseInspection::Exact(database) => database,
            ActiveReblitCompleteFinalizationDatabaseInspection::Incompatible => {
                return Ok(ActiveReblitCompleteFinalizationAdmission::Deferred);
            }
        };
        let active_state = match capture_exact_active_state(
            record,
            installation,
            active_state_reservation,
        )? {
            Some(active_state) => active_state,
            None => return Ok(ActiveReblitCompleteFinalizationAdmission::Deferred),
        };
        let inspection = match ActiveReblitCommitCleanupNamespaceInspection::begin(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(inspection) => inspection,
            Err(source) if active_reblit_commit_cleanup_namespace_error_is_mismatch(&source) => {
                return Ok(ActiveReblitCompleteFinalizationAdmission::Deferred);
            }
            Err(source) => return Err(source.into()),
        };
        run_between_database_captures();
        let namespace = match inspection.finish(
            installation,
            journal,
            &journal_record_binding,
            record,
        )? {
            ActiveReblitCommitCleanupNamespaceProof::Finish(namespace) => namespace,
            ActiveReblitCommitCleanupNamespaceProof::Apply(_) => {
                return Ok(ActiveReblitCompleteFinalizationAdmission::Deferred);
            }
        };
        let database_after = require_exact_database(
            &database_before,
            inspect_current_database(record, &database_before.route, state_db)?,
        )?;
        require_exact_active_state(record, installation, &active_state)?;
        if database_before != database_after
            || !record_plan_is_exact(record, &database_after.route)
        {
            return Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::EvidenceChanged.into());
        }
        require_exact_record_binding(
            installation,
            journal,
            &journal_record_binding,
            record,
        )?;
        installation.revalidate_mutable_namespace()?;

        Ok(ActiveReblitCompleteFinalizationAdmission::Ready(Self {
            evidence: ActiveReblitCompleteFinalizationEvidence {
                installation: installation.clone(),
                state_db: state_db.clone(),
                record: record.clone(),
                database: database_after,
                active_state,
                namespace,
                _active_state_reservation: active_state_reservation,
            },
            journal_binding,
            journal_record_binding,
        }))
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.evidence.record
    }

    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), ActiveReblitCompleteFinalizationAuthorityError> {
        require_exact_record_binding(
            &self.evidence.installation,
            journal,
            &self.journal_record_binding,
            &self.evidence.record,
        )?;
        if !journal.has_binding(&self.journal_binding) {
            return Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::JournalBindingChanged.into());
        }
        self.evidence.installation.revalidate_mutable_namespace()?;
        let database_before = require_exact_database(
            &self.evidence.database,
            inspect_current_database(
                &self.evidence.record,
                &self.evidence.database.route,
                &self.evidence.state_db,
            )?,
        )?;
        require_exact_active_state(
            &self.evidence.record,
            &self.evidence.installation,
            &self.evidence.active_state,
        )?;
        self.evidence.namespace.revalidate(
            &self.evidence.installation,
            journal,
            &self.journal_record_binding,
            &self.evidence.record,
        )?;
        let database_after = require_exact_database(
            &self.evidence.database,
            inspect_current_database(
                &self.evidence.record,
                &self.evidence.database.route,
                &self.evidence.state_db,
            )?,
        )?;
        require_exact_active_state(
            &self.evidence.record,
            &self.evidence.installation,
            &self.evidence.active_state,
        )?;
        if database_before != database_after
            || !record_plan_is_exact(&self.evidence.record, &self.evidence.database.route)
        {
            return Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::EvidenceChanged.into());
        }
        require_exact_record_binding(
            &self.evidence.installation,
            journal,
            &self.journal_record_binding,
            &self.evidence.record,
        )?;
        self.evidence.installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    pub(in crate::client) fn attempt_record_bound_delete(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        (
            Result<(), TransitionJournalRecordDeleteError>,
            ActiveReblitCompleteFinalizationAfterDeleteAuthority<'reservation>,
        ),
        ActiveReblitCompleteFinalizationAuthorityError,
    > {
        self.revalidate(journal)?;
        let Self {
            evidence,
            journal_binding,
            journal_record_binding,
        } = self;
        let delete = journal.delete_record_binding(
            evidence.installation.retained_mutable_cast_directory()?,
            journal_record_binding,
            &evidence.record,
        );
        Ok((
            delete,
            ActiveReblitCompleteFinalizationAfterDeleteAuthority {
                evidence,
                journal_binding,
            },
        ))
    }
}

impl ActiveReblitCompleteFinalizationAfterDeleteAuthority<'_> {
    pub(in crate::client) fn revalidate_after_journal_delete(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<(), ActiveReblitCompleteFinalizationAuthorityError> {
        if !journal.has_binding(&self.journal_binding) {
            return Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::JournalBindingChanged.into());
        }
        self.evidence.installation.revalidate_mutable_namespace()?;
        let database_before = require_exact_database(
            &self.evidence.database,
            inspect_current_database(
                &self.evidence.record,
                &self.evidence.database.route,
                &self.evidence.state_db,
            )?,
        )?;
        require_exact_active_state(
            &self.evidence.record,
            &self.evidence.installation,
            &self.evidence.active_state,
        )?;
        run_between_post_delete_database_captures();
        self.evidence
            .namespace
            .revalidate_completed_namespace_after_journal_delete(
                &self.evidence.installation,
                journal,
                &self.evidence.record,
            )?;
        let database_after = require_exact_database(
            &self.evidence.database,
            inspect_current_database(
                &self.evidence.record,
                &self.evidence.database.route,
                &self.evidence.state_db,
            )?,
        )?;
        require_exact_active_state(
            &self.evidence.record,
            &self.evidence.installation,
            &self.evidence.active_state,
        )?;
        if database_before != database_after
            || !record_plan_is_exact(&self.evidence.record, &self.evidence.database.route)
        {
            return Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::EvidenceChanged.into());
        }
        if !journal.has_binding(&self.journal_binding) {
            return Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::JournalBindingChanged.into());
        }
        self.evidence.installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn inspect_current_database(
    record: &TransitionRecord,
    route: &ActiveReblitCompleteFinalizationRouteEvidence,
    state_db: &db::state::Database,
) -> Result<
    ActiveReblitCompleteFinalizationDatabaseInspection,
    ActiveReblitCompleteFinalizationAuthorityError,
> {
    match route {
        ActiveReblitCompleteFinalizationRouteEvidence::ReceiptBacked { pair, .. } => {
            inspect_receipt_backed_database(record, *pair, state_db)
        }
        ActiveReblitCompleteFinalizationRouteEvidence::NoBoot { .. } => {
            inspect_no_boot_database(record, state_db)
        }
    }
}

fn inspect_current_database_for_plan(
    record: &TransitionRecord,
    route: &ActiveReblitCompleteFinalizationRoutePlan,
    state_db: &db::state::Database,
) -> Result<
    ActiveReblitCompleteFinalizationDatabaseInspection,
    ActiveReblitCompleteFinalizationAuthorityError,
> {
    match route {
        ActiveReblitCompleteFinalizationRoutePlan::ReceiptBacked(pair) => {
            inspect_receipt_backed_database(record, *pair, state_db)
        }
        ActiveReblitCompleteFinalizationRoutePlan::NoBoot => {
            inspect_no_boot_database(record, state_db)
        }
    }
}

fn inspect_receipt_backed_database(
    record: &TransitionRecord,
    pair: crate::boot_publication::BootPublicationReceiptPair,
    state_db: &db::state::Database,
) -> Result<
    ActiveReblitCompleteFinalizationDatabaseInspection,
    ActiveReblitCompleteFinalizationAuthorityError,
> {
    let receipt_before = match load_exact_promoted_receipt(state_db, record, pair)? {
        Some(receipt) => receipt,
        None => return Ok(ActiveReblitCompleteFinalizationDatabaseInspection::Incompatible),
    };
    let Some((context, state)) = inspect_context_and_state(record, state_db)? else {
        return Ok(ActiveReblitCompleteFinalizationDatabaseInspection::Incompatible);
    };
    let receipt_after = match load_exact_promoted_receipt(state_db, record, pair)? {
        Some(receipt) => receipt,
        None => {
            return Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::DatabaseChanged.into());
        }
    };
    if receipt_before != receipt_after {
        return Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::DatabaseChanged.into());
    }
    Ok(ActiveReblitCompleteFinalizationDatabaseInspection::Exact(
        ActiveReblitCompleteFinalizationDatabaseEvidence {
            route: ActiveReblitCompleteFinalizationRouteEvidence::ReceiptBacked {
                pair,
                receipt: receipt_after,
            },
            context,
            state,
        },
    ))
}

fn inspect_no_boot_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<
    ActiveReblitCompleteFinalizationDatabaseInspection,
    ActiveReblitCompleteFinalizationAuthorityError,
> {
    let receipt_chain_before = load_inert_no_boot_receipt_chain(record, state_db)?;
    let Some((context, state)) = inspect_context_and_state(record, state_db)? else {
        return Ok(ActiveReblitCompleteFinalizationDatabaseInspection::Incompatible);
    };
    let receipt_chain_after = load_inert_no_boot_receipt_chain(record, state_db)?;
    if receipt_chain_before != receipt_chain_after {
        return Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::DatabaseChanged.into());
    }
    Ok(ActiveReblitCompleteFinalizationDatabaseInspection::Exact(
        ActiveReblitCompleteFinalizationDatabaseEvidence {
            route: ActiveReblitCompleteFinalizationRouteEvidence::NoBoot {
                inert_receipt_chain: receipt_chain_after,
            },
            context,
            state,
        },
    ))
}

fn load_inert_no_boot_receipt_chain(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<
    db::state::CurrentExactPromotedBootPublicationReceiptChain,
    ActiveReblitCompleteFinalizationAuthorityError,
> {
    let chain = state_db
        .load_current_exact_promoted_boot_publication_receipt_chain()
        .map_err(ActiveReblitCompleteFinalizationAuthorityErrorKind::ReceiptChain)?;
    if matches!(
        &chain,
        db::state::CurrentExactPromotedBootPublicationReceiptChain::Installed(installed)
            if installed.installed_receipt().body().transition_id() == &record.transition_id
    ) {
        return Err(
            ActiveReblitCompleteFinalizationAuthorityErrorKind::ReceiptChainMatchesNoBootTransition
                .into(),
        );
    }
    Ok(chain)
}

fn inspect_context_and_state(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<Option<(DatabaseEvidence, State)>, ActiveReblitCompleteFinalizationAuthorityError> {
    let in_flight = state_db
        .audit_in_flight_transition()
        .map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    if !existing_state_context_is_exact(record, &context) {
        return Ok(None);
    }
    let state_id = state::Id::from(
        record
            .candidate
            .id
            .expect("checked exact ActiveReblit state"),
    );
    let state = state_db
        .get(state_id)
        .map_err(ActiveReblitCompleteFinalizationAuthorityErrorKind::StateDatabase)?;
    if state.id != state_id {
        return Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::DatabaseChanged.into());
    }
    Ok(Some((context, state)))
}

fn load_exact_promoted_receipt(
    state_db: &db::state::Database,
    record: &TransitionRecord,
    pair: crate::boot_publication::BootPublicationReceiptPair,
) -> Result<
    Option<db::state::BootPublicationReceiptState>,
    ActiveReblitCompleteFinalizationAuthorityError,
> {
    match state_db
        .load_exact_promoted_boot_publication_receipt_state(&record.transition_id, &pair)
    {
        Ok(receipt) => Ok(Some(receipt)),
        Err(db::state::ExactPromotedBootPublicationReceiptStateError::State(source)) => {
            Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::ReceiptState(source).into())
        }
        Err(source @ db::state::ExactPromotedBootPublicationReceiptStateError::PendingBodyPresent)
        | Err(source @ db::state::ExactPromotedBootPublicationReceiptStateError::MissingCommittedBody)
        | Err(
            source @ db::state::ExactPromotedBootPublicationReceiptStateError::CommittedBodyFingerprintMismatch {
                ..
            },
        ) => Err(
            ActiveReblitCompleteFinalizationAuthorityErrorKind::ReceiptCorrelation(source).into(),
        ),
        Err(
            db::state::ExactPromotedBootPublicationReceiptStateError::PendingHeadPresent { .. }
            | db::state::ExactPromotedBootPublicationReceiptStateError::CommittedHeadMismatch { .. }
            | db::state::ExactPromotedBootPublicationReceiptStateError::TransitionMismatch { .. }
            | db::state::ExactPromotedBootPublicationReceiptStateError::CommittedPredecessorMismatch { .. },
        ) => Ok(None),
    }
}

fn require_exact_database(
    expected: &ActiveReblitCompleteFinalizationDatabaseEvidence,
    actual: ActiveReblitCompleteFinalizationDatabaseInspection,
) -> Result<
    ActiveReblitCompleteFinalizationDatabaseEvidence,
    ActiveReblitCompleteFinalizationAuthorityError,
> {
    match actual {
        ActiveReblitCompleteFinalizationDatabaseInspection::Exact(actual)
            if actual == *expected => Ok(actual),
        _ => Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::DatabaseChanged.into()),
    }
}

fn existing_state_context_is_exact(record: &TransitionRecord, evidence: &DatabaseEvidence) -> bool {
    if !database_ownership_evidence_compatible(record, evidence)
        || !metadata_provenance_evidence_compatible(record, evidence)
    {
        return false;
    }
    let (Some(candidate), Some(previous)) = (
        record.candidate.id.map(state::Id::from),
        record.previous.id.map(state::Id::from),
    ) else {
        return false;
    };
    candidate == previous
        && matches!(
            evidence,
            DatabaseEvidence::ExistingCandidate {
                candidate: existing,
                provenance: Some(_),
                previous: None,
            } if existing.state == candidate
                && existing.ownership == db::state::TransitionOwnership::Cleared
        )
}

fn capture_exact_active_state(
    record: &TransitionRecord,
    installation: &Installation,
    reservation: &ActiveStateReservation,
) -> Result<Option<ActiveStateSnapshot>, ActiveReblitCompleteFinalizationAuthorityError> {
    let snapshot = reservation
        .capture_for_startup_recovery(installation)
        .map_err(ActiveReblitCompleteFinalizationAuthorityErrorKind::ActiveState)?;
    let expected = state::Id::from(record.candidate.id.expect("checked exact ActiveReblit state"));
    if snapshot.active() != Some(expected) {
        return Ok(None);
    }
    snapshot
        .revalidate(installation)
        .map_err(ActiveReblitCompleteFinalizationAuthorityErrorKind::ActiveState)?;
    Ok(Some(snapshot))
}

fn require_exact_active_state(
    record: &TransitionRecord,
    installation: &Installation,
    snapshot: &ActiveStateSnapshot,
) -> Result<(), ActiveReblitCompleteFinalizationAuthorityError> {
    let expected = state::Id::from(record.candidate.id.expect("retained exact ActiveReblit state"));
    if snapshot.active() != Some(expected) {
        return Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::ActiveSelectionChanged.into());
    }
    snapshot
        .revalidate(installation)
        .map_err(ActiveReblitCompleteFinalizationAuthorityErrorKind::ActiveState)?;
    Ok(())
}

fn same_nonempty_candidate_and_previous(record: &TransitionRecord) -> bool {
    record.candidate.id.is_some() && record.candidate.id == record.previous.id
}

fn exact_route_plan(
    record: &TransitionRecord,
    receipt_correlation: Option<crate::boot_publication::BootPublicationReceiptPair>,
) -> Option<ActiveReblitCompleteFinalizationRoutePlan> {
    if record.operation != Operation::ActiveReblit
        || record.phase != Phase::Complete
        || record.rollback.is_some()
        || !same_nonempty_candidate_and_previous(record)
    {
        return None;
    }
    match (
        record.generation,
        record.options.run_boot_sync,
        receipt_correlation,
    ) {
        (_, true, Some(pair)) => {
            Some(ActiveReblitCompleteFinalizationRoutePlan::ReceiptBacked(pair))
        }
        (13, false, None)
            if record.options.run_system_triggers && !record.options.archive_previous =>
        {
            Some(ActiveReblitCompleteFinalizationRoutePlan::NoBoot)
        }
        _ => None,
    }
}

fn record_plan_is_exact(
    record: &TransitionRecord,
    route: &ActiveReblitCompleteFinalizationRouteEvidence,
) -> bool {
    let common = record.operation == Operation::ActiveReblit
        && record.phase == Phase::Complete
        && record.rollback.is_none()
        && same_nonempty_candidate_and_previous(record);
    common
        && match route {
            ActiveReblitCompleteFinalizationRouteEvidence::ReceiptBacked { pair, .. } => {
                record.options.run_boot_sync
                    && record.boot_publication_receipts == Some(*pair)
            }
            ActiveReblitCompleteFinalizationRouteEvidence::NoBoot { .. } => {
                record.generation == 13
                    && record.options.run_system_triggers
                    && !record.options.run_boot_sync
                    && !record.options.archive_previous
                    && record.boot_publication_receipts.is_none()
            }
        }
}

fn require_exact_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), ActiveReblitCompleteFinalizationAuthorityError> {
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_store_binding(binding)
        && journal.has_record_binding(cast, binding, record)?
    {
        Ok(())
    } else {
        Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::JournalRecordBindingChanged.into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct ActiveReblitCompleteFinalizationAuthorityError(
    #[from] ActiveReblitCompleteFinalizationAuthorityErrorKind,
);

#[derive(Debug, thiserror::Error)]
enum ActiveReblitCompleteFinalizationAuthorityErrorKind {
    #[error("validate exact v3 ActiveReblit Complete record")]
    Record(#[source] CodecError),
    #[error("the exact Complete record binding changed")]
    JournalRecordBindingChanged,
    #[error("the lock-bearing Complete journal store changed")]
    JournalBindingChanged,
    #[error("load exact installed boot-publication receipt state")]
    ReceiptState(#[source] db::state::BootPublicationReceiptStateError),
    #[error("authenticate exact installed boot-publication receipt correlation")]
    ReceiptCorrelation(#[source] db::state::ExactPromotedBootPublicationReceiptStateError),
    #[error("load current exact promoted boot-publication receipt chain as inert evidence")]
    ReceiptChain(#[source] db::state::CurrentExactPromotedBootPublicationReceiptChainError),
    #[error("the no-boot transition unexpectedly owns the installed receipt chain")]
    ReceiptChainMatchesNoBootTransition,
    #[error("inspect exact cleared ActiveReblit database and provenance")]
    Inspection(#[source] InspectionError),
    #[error("load complete selected ActiveReblit state")]
    StateDatabase(#[source] db::Error),
    #[error("exact Complete database evidence changed")]
    DatabaseChanged,
    #[error("prove exact live active-state selection")]
    ActiveState(#[source] super::super::Error),
    #[error("the selected active state changed")]
    ActiveSelectionChanged,
    #[error("revalidate exact completed cleanup namespace")]
    Namespace(#[source] ActiveReblitCommitCleanupNamespaceError),
    #[error("exact forward Complete finalization evidence changed")]
    EvidenceChanged,
    #[error("revalidate retained mutable installation namespace")]
    Installation(#[source] crate::installation::Error),
    #[error("read or bind retained transition journal")]
    Journal(#[source] StorageError),
}

impl From<InspectionError> for ActiveReblitCompleteFinalizationAuthorityError {
    fn from(source: InspectionError) -> Self {
        ActiveReblitCompleteFinalizationAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<ActiveReblitCommitCleanupNamespaceError>
    for ActiveReblitCompleteFinalizationAuthorityError
{
    fn from(source: ActiveReblitCommitCleanupNamespaceError) -> Self {
        ActiveReblitCompleteFinalizationAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for ActiveReblitCompleteFinalizationAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        ActiveReblitCompleteFinalizationAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for ActiveReblitCompleteFinalizationAuthorityError {
    fn from(source: StorageError) -> Self {
        ActiveReblitCompleteFinalizationAuthorityErrorKind::Journal(source).into()
    }
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BETWEEN_POST_DELETE_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_active_reblit_complete_finalization_database_captures(
    hook: impl FnOnce() + 'static,
) {
    BETWEEN_DATABASE_CAPTURES.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_between_active_reblit_complete_finalization_post_delete_database_captures(
    hook: impl FnOnce() + 'static,
) {
    BETWEEN_POST_DELETE_DATABASE_CAPTURES.with(|slot| {
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
fn run_between_post_delete_database_captures() {
    BETWEEN_POST_DELETE_DATABASE_CAPTURES.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_between_post_delete_database_captures() {}
