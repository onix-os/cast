//! Sealed read-only startup authority for an exact forward ActiveReblit
//! `BootSyncComplete` record.
//!
//! Admission retains the promoted canonical boot-publication receipt, the
//! cleared existing-state row and its metadata provenance, the complete state,
//! the live active-selection proof, the phase-specific namespace proof, and
//! the exact journal-record inode. It performs no boot, database, namespace,
//! cleanup, or trigger effect. The only mutation surface consumes the complete
//! authority through a caller-supplied legal bound journal successor and
//! returns all evidence needed to authenticate that successor both before and
//! after a canonical reopen.

use crate::{
    Installation, State, db, state,
    transition_journal::{
        CodecError, Operation, Phase, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::{ActiveStateReservation, ActiveStateSnapshot},
    startup_gate::ActiveReblitBootSyncCompleteSeal,
};
use super::{
    DatabaseEvidence, InspectionError, database_ownership_evidence_compatible, inspect_database,
    metadata_provenance_evidence_compatible,
};
use super::activation_namespace::{
    ActiveReblitBootSyncCompleteNamespaceError, ActiveReblitBootSyncCompleteNamespaceInspection,
    ActiveReblitBootSyncCompleteNamespaceProof,
    active_reblit_boot_sync_complete_namespace_error_is_mismatch,
};

/// Read-only startup admission for the exact receipt-backed completion point.
pub(in crate::client) enum ActiveReblitBootSyncCompleteAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Ready(ActiveReblitBootSyncCompleteAuthority<'reservation>),
}

/// Non-replayable authority for one exact bound `BootSyncComplete` record.
///
/// This type intentionally implements neither `Clone` nor `Copy`.
pub(in crate::client) struct ActiveReblitBootSyncCompleteAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
    database: ActiveReblitBootSyncCompleteDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: ActiveReblitBootSyncCompleteNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Evidence which survives the bound advance but grants no second advance.
///
/// A persistence caller must use the same-store validation, close the old
/// lock-bearing journal, reopen canonically, and then use reopened validation.
/// This type intentionally implements neither `Clone` nor `Copy`.
pub(in crate::client) struct ActiveReblitBootSyncCompletePostAdvanceAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    completed_record: TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
    database: ActiveReblitBootSyncCompleteDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: ActiveReblitBootSyncCompleteNamespaceProof,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Exact receipt, state-row, provenance, and complete-state observation from
/// one stable database sandwich. This evidence is intentionally not `Clone`.
#[derive(Debug, Eq, PartialEq)]
struct ActiveReblitBootSyncCompleteDatabaseEvidence {
    receipt: db::state::BootPublicationReceiptState,
    context: DatabaseEvidence,
    state: State,
}

enum ActiveReblitBootSyncCompleteDatabaseInspection {
    Exact(ActiveReblitBootSyncCompleteDatabaseEvidence),
    Incompatible,
}

impl<'reservation> ActiveReblitBootSyncCompleteAuthority<'reservation> {
    /// Capture one exact forward completion point without performing effects.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &ActiveReblitBootSyncCompleteSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<ActiveReblitBootSyncCompleteAdmission<'reservation>, ActiveReblitBootSyncCompleteAuthorityError> {
        if record.operation != Operation::ActiveReblit || record.phase != Phase::BootSyncComplete {
            return Ok(ActiveReblitBootSyncCompleteAdmission::NotApplicable);
        }
        let receipt_correlation = record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitBootSyncCompleteAuthorityErrorKind::Record)?;
        if record.rollback.is_some() || !same_nonempty_candidate_and_previous(record) {
            return Ok(ActiveReblitBootSyncCompleteAdmission::Deferred);
        }
        let Some(receipt_pair) = receipt_correlation else {
            return Ok(ActiveReblitBootSyncCompleteAdmission::Deferred);
        };

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding = journal.record_binding(
            installation.retained_mutable_cast_directory()?,
            record,
        )?;
        installation.revalidate_mutable_namespace()?;

        let database_before = match inspect_current_database(record, receipt_pair, state_db)? {
            ActiveReblitBootSyncCompleteDatabaseInspection::Exact(database) => database,
            ActiveReblitBootSyncCompleteDatabaseInspection::Incompatible => {
                return Ok(ActiveReblitBootSyncCompleteAdmission::Deferred);
            }
        };
        let active_state = match capture_exact_active_state(
            record,
            installation,
            active_state_reservation,
        )? {
            Some(active_state) => active_state,
            None => return Ok(ActiveReblitBootSyncCompleteAdmission::Deferred),
        };
        let namespace_inspection = match ActiveReblitBootSyncCompleteNamespaceInspection::begin(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(inspection) => inspection,
            Err(source) if active_reblit_boot_sync_complete_namespace_error_is_mismatch(&source) => {
                return Ok(ActiveReblitBootSyncCompleteAdmission::Deferred);
            }
            Err(source) => return Err(source.into()),
        };
        run_between_database_captures();
        let namespace = namespace_inspection.finish(
            installation,
            journal,
            &journal_record_binding,
            record,
        )?;
        let database_after = match inspect_current_database(record, receipt_pair, state_db)? {
            ActiveReblitBootSyncCompleteDatabaseInspection::Exact(database) => database,
            ActiveReblitBootSyncCompleteDatabaseInspection::Incompatible => {
                return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into());
            }
        };
        if database_before != database_after {
            return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into());
        }
        if !record_plan_is_exact(record, receipt_pair) {
            return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::RouteEvidenceChanged.into());
        }
        if !revalidate_active_state_for_admission(record, installation, &active_state)? {
            return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::ActiveSelectionChanged.into());
        }
        require_exact_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        Ok(ActiveReblitBootSyncCompleteAdmission::Ready(Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            receipt_pair,
            database: database_after,
            active_state,
            namespace,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        }))
    }

    /// Revalidate the binding-first DB -> active/namespace -> DB sandwich.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), ActiveReblitBootSyncCompleteAuthorityError> {
        require_exact_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        self.installation.revalidate_mutable_namespace()?;
        let database_before = require_exact_database(
            &self.database,
            inspect_current_database(&self.record, self.receipt_pair, &self.state_db)?,
        )?;
        require_exact_active_state(&self.record, &self.installation, &self.active_state)?;
        self.namespace.revalidate(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        let database_after = require_exact_database(
            &self.database,
            inspect_current_database(&self.record, self.receipt_pair, &self.state_db)?,
        )?;
        require_exact_active_state(&self.record, &self.installation, &self.active_state)?;
        if database_before != database_after || !record_plan_is_exact(&self.record, self.receipt_pair) {
            return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::RouteEvidenceChanged.into());
        }
        require_exact_record_binding(
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

    /// Consume the source authority through a caller-supplied exact successor.
    /// No successor is derived here and no retry authority is retained.
    pub(in crate::client) fn advance_record_binding(
        self,
        journal: &TransitionJournalStore,
        successor: &TransitionRecord,
    ) -> Result<
        (
            TransitionJournalRecordBinding,
            ActiveReblitBootSyncCompletePostAdvanceAuthority<'reservation>,
        ),
        ActiveReblitBootSyncCompleteRecordAdvanceError,
    > {
        self.revalidate(journal)?;
        if !exact_commit_decided_successor(&self.record, successor, self.receipt_pair)? {
            return Err(ActiveReblitBootSyncCompleteRecordAdvanceError::UnexpectedSuccessor);
        }

        let Self {
            installation,
            state_db,
            record,
            receipt_pair,
            database,
            active_state,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        } = self;
        let cast = installation.retained_mutable_cast_directory()?;
        let successor_binding = journal
            .advance_record_binding(cast, journal_record_binding, successor)
            .map_err(ActiveReblitBootSyncCompleteRecordAdvanceError::Storage)?;
        Ok((
            successor_binding,
            ActiveReblitBootSyncCompletePostAdvanceAuthority {
                installation,
                state_db,
                completed_record: record,
                receipt_pair,
                database,
                active_state,
                namespace,
                _active_state_reservation,
            },
        ))
    }
}

impl ActiveReblitBootSyncCompletePostAdvanceAuthority<'_> {
    /// Authenticate the exact successor while the advancing store remains open.
    pub(in crate::client) fn revalidate_successor_same_store(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
    ) -> Result<(), ActiveReblitBootSyncCompleteAuthorityError> {
        self.revalidate_successor(journal, successor_binding, successor, SuccessorBindingMode::SameStore)
    }

    /// Authenticate the same successor inode after canonical writer reopen.
    pub(in crate::client) fn revalidate_successor_reopened(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
    ) -> Result<(), ActiveReblitBootSyncCompleteAuthorityError> {
        self.revalidate_successor(journal, successor_binding, successor, SuccessorBindingMode::Reopened)
    }

    fn revalidate_successor(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
        binding_mode: SuccessorBindingMode,
    ) -> Result<(), ActiveReblitBootSyncCompleteAuthorityError> {
        require_exact_successor_binding(
            &self.installation,
            journal,
            successor_binding,
            successor,
            binding_mode,
        )?;
        if !exact_commit_decided_successor(&self.completed_record, successor, self.receipt_pair)
            .map_err(ActiveReblitBootSyncCompleteAuthorityErrorKind::Record)?
        {
            return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::UnexpectedSuccessor.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        let database_before = require_exact_database(
            &self.database,
            inspect_current_database(successor, self.receipt_pair, &self.state_db)?,
        )?;
        require_exact_active_state(successor, &self.installation, &self.active_state)?;
        match binding_mode {
            SuccessorBindingMode::SameStore => self.namespace.revalidate_successor_same_store(
                &self.installation,
                journal,
                successor_binding,
                &self.completed_record,
                successor,
            )?,
            SuccessorBindingMode::Reopened => self.namespace.revalidate_successor_reopened(
                &self.installation,
                journal,
                successor_binding,
                &self.completed_record,
                successor,
            )?,
        }
        let database_after = require_exact_database(
            &self.database,
            inspect_current_database(successor, self.receipt_pair, &self.state_db)?,
        )?;
        require_exact_active_state(successor, &self.installation, &self.active_state)?;
        if database_before != database_after {
            return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::RouteEvidenceChanged.into());
        }
        require_exact_successor_binding(
            &self.installation,
            journal,
            successor_binding,
            successor,
            binding_mode,
        )?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum SuccessorBindingMode {
    SameStore,
    Reopened,
}

fn inspect_current_database(
    record: &TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
    state_db: &db::state::Database,
) -> Result<ActiveReblitBootSyncCompleteDatabaseInspection, ActiveReblitBootSyncCompleteAuthorityError> {
    let receipt_before = match load_exact_promoted_receipt(state_db, record, receipt_pair)? {
        Some(receipt) => receipt,
        None => return Ok(ActiveReblitBootSyncCompleteDatabaseInspection::Incompatible),
    };
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    if !existing_state_context_is_exact(record, &context) {
        return Ok(ActiveReblitBootSyncCompleteDatabaseInspection::Incompatible);
    }
    let state_id = state::Id::from(record.candidate.id.expect("checked nonempty ActiveReblit state ID"));
    let state = match state_db.get(state_id) {
        Ok(state) => state,
        Err(db::Error::RowNotFound) => {
            return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into());
        }
        Err(source) => {
            return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::StateDatabase(source).into());
        }
    };
    if state.id != state_id {
        return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into());
    }
    let receipt_after = match load_exact_promoted_receipt(state_db, record, receipt_pair)? {
        Some(receipt) => receipt,
        None => {
            return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into());
        }
    };
    if receipt_before != receipt_after {
        return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into());
    }
    Ok(ActiveReblitBootSyncCompleteDatabaseInspection::Exact(
        ActiveReblitBootSyncCompleteDatabaseEvidence {
            receipt: receipt_after,
            context,
            state,
        },
    ))
}

fn load_exact_promoted_receipt(
    state_db: &db::state::Database,
    record: &TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
) -> Result<Option<db::state::BootPublicationReceiptState>, ActiveReblitBootSyncCompleteAuthorityError> {
    match state_db.load_exact_promoted_boot_publication_receipt_state(&record.transition_id, &receipt_pair) {
        Ok(receipt) => Ok(Some(receipt)),
        Err(db::state::ExactPromotedBootPublicationReceiptStateError::State(source)) => {
            Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::ReceiptState(source).into())
        }
        Err(source @ db::state::ExactPromotedBootPublicationReceiptStateError::PendingBodyPresent)
        | Err(source @ db::state::ExactPromotedBootPublicationReceiptStateError::MissingCommittedBody)
        | Err(
            source @ db::state::ExactPromotedBootPublicationReceiptStateError::CommittedBodyFingerprintMismatch {
                ..
            },
        ) => Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::ReceiptCorrelation(source).into()),
        Err(
            db::state::ExactPromotedBootPublicationReceiptStateError::PendingHeadPresent { .. }
            | db::state::ExactPromotedBootPublicationReceiptStateError::CommittedHeadMismatch { .. }
            | db::state::ExactPromotedBootPublicationReceiptStateError::TransitionMismatch { .. }
            | db::state::ExactPromotedBootPublicationReceiptStateError::CommittedPredecessorMismatch { .. },
        ) => Ok(None),
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

fn require_exact_database(
    expected: &ActiveReblitBootSyncCompleteDatabaseEvidence,
    actual: ActiveReblitBootSyncCompleteDatabaseInspection,
) -> Result<ActiveReblitBootSyncCompleteDatabaseEvidence, ActiveReblitBootSyncCompleteAuthorityError> {
    match actual {
        ActiveReblitBootSyncCompleteDatabaseInspection::Exact(actual) if actual == *expected => Ok(actual),
        ActiveReblitBootSyncCompleteDatabaseInspection::Exact(_)
        | ActiveReblitBootSyncCompleteDatabaseInspection::Incompatible => {
            Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into())
        }
    }
}

fn capture_exact_active_state(
    record: &TransitionRecord,
    installation: &Installation,
    reservation: &ActiveStateReservation,
) -> Result<Option<ActiveStateSnapshot>, ActiveReblitBootSyncCompleteAuthorityError> {
    let active_state = match reservation.capture_for_startup_recovery(installation) {
        Ok(active_state) => active_state,
        Err(source) => return Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::ActiveState(source).into()),
    };
    let expected = state::Id::from(record.candidate.id.expect("checked nonempty ActiveReblit state ID"));
    if active_state.active() != Some(expected) {
        return Ok(None);
    }
    if !revalidate_active_state_for_admission(record, installation, &active_state)? {
        return Ok(None);
    }
    Ok(Some(active_state))
}

fn revalidate_active_state_for_admission(
    record: &TransitionRecord,
    installation: &Installation,
    active_state: &ActiveStateSnapshot,
) -> Result<bool, ActiveReblitBootSyncCompleteAuthorityError> {
    let expected = state::Id::from(record.candidate.id.expect("checked nonempty ActiveReblit state ID"));
    if active_state.active() != Some(expected) {
        return Ok(false);
    }
    match active_state.revalidate(installation) {
        Ok(()) => Ok(true),
        Err(source) => Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::ActiveState(source).into()),
    }
}

fn require_exact_active_state(
    record: &TransitionRecord,
    installation: &Installation,
    active_state: &ActiveStateSnapshot,
) -> Result<(), ActiveReblitBootSyncCompleteAuthorityError> {
    let expected = state::Id::from(record.candidate.id.expect("validated ActiveReblit state ID"));
    let actual = active_state.active();
    if actual != Some(expected) {
        return Err(
            ActiveReblitBootSyncCompleteAuthorityErrorKind::ActiveSelectionMismatch { expected, actual }.into(),
        );
    }
    active_state
        .revalidate(installation)
        .map_err(ActiveReblitBootSyncCompleteAuthorityErrorKind::ActiveState)?;
    Ok(())
}

fn same_nonempty_candidate_and_previous(record: &TransitionRecord) -> bool {
    record.candidate.id.is_some() && record.candidate.id == record.previous.id
}

fn record_plan_is_exact(
    record: &TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
) -> bool {
    record.operation == Operation::ActiveReblit
        && record.phase == Phase::BootSyncComplete
        && record.rollback.is_none()
        && same_nonempty_candidate_and_previous(record)
        && record.boot_publication_receipts == Some(receipt_pair)
}

fn exact_commit_decided_successor(
    completed: &TransitionRecord,
    successor: &TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
) -> Result<bool, CodecError> {
    let completed_pair = completed.boot_publication_receipt_correlation()?;
    let successor_pair = successor.boot_publication_receipt_correlation()?;
    Ok(record_plan_is_exact(completed, receipt_pair)
        && successor.operation == Operation::ActiveReblit
        && successor.phase == Phase::CommitDecided
        && successor.rollback.is_none()
        && same_nonempty_candidate_and_previous(successor)
        && completed_pair == Some(receipt_pair)
        && successor_pair == Some(receipt_pair)
        && successor.generation == completed.generation.checked_add(1).unwrap_or(0)
        && successor.format == completed.format
        && successor.version == completed.version
        && successor.transition_id == completed.transition_id
        && successor.creation_epoch == completed.creation_epoch
        && successor.candidate == completed.candidate
        && successor.previous == completed.previous
        && successor.options == completed.options
        && successor.quarantine_name == completed.quarantine_name)
}

fn has_exact_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<bool, ActiveReblitBootSyncCompleteAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Ok(false);
    }
    let cast = installation.retained_mutable_cast_directory()?;
    journal.has_record_binding(cast, binding, record).map_err(Into::into)
}

fn require_exact_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), ActiveReblitBootSyncCompleteAuthorityError> {
    if has_exact_record_binding(installation, journal, binding, record)? {
        Ok(())
    } else {
        Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::JournalRecordBindingChanged.into())
    }
}

fn require_exact_successor_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
    mode: SuccessorBindingMode,
) -> Result<(), ActiveReblitBootSyncCompleteAuthorityError> {
    let cast = installation.retained_mutable_cast_directory()?;
    let exact = match mode {
        SuccessorBindingMode::SameStore => {
            journal.has_record_store_binding(binding) && journal.has_record_binding(cast, binding, successor)?
        }
        SuccessorBindingMode::Reopened => journal.has_reopened_record_binding(cast, binding, successor)?,
    };
    if exact {
        Ok(())
    } else {
        Err(ActiveReblitBootSyncCompleteAuthorityErrorKind::SuccessorRecordBindingChanged.into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct ActiveReblitBootSyncCompleteAuthorityError(
    #[from] ActiveReblitBootSyncCompleteAuthorityErrorKind,
);

impl From<InspectionError> for ActiveReblitBootSyncCompleteAuthorityError {
    fn from(source: InspectionError) -> Self {
        ActiveReblitBootSyncCompleteAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<ActiveReblitBootSyncCompleteNamespaceError> for ActiveReblitBootSyncCompleteAuthorityError {
    fn from(source: ActiveReblitBootSyncCompleteNamespaceError) -> Self {
        ActiveReblitBootSyncCompleteAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for ActiveReblitBootSyncCompleteAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        ActiveReblitBootSyncCompleteAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for ActiveReblitBootSyncCompleteAuthorityError {
    fn from(source: StorageError) -> Self {
        ActiveReblitBootSyncCompleteAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum ActiveReblitBootSyncCompleteRecordAdvanceError {
    #[error("revalidate exact ActiveReblit BootSyncComplete startup authority before the bound advance")]
    Authority(#[from] ActiveReblitBootSyncCompleteAuthorityError),
    #[error("validate the caller-supplied ActiveReblit CommitDecided successor")]
    Record(#[from] CodecError),
    #[error("the caller-supplied record is not the exact ActiveReblit CommitDecided successor")]
    UnexpectedSuccessor,
    #[error("revalidate retained installation before the bound ActiveReblit CommitDecided advance")]
    Installation(#[from] crate::installation::Error),
    #[error("advance the exact bound ActiveReblit BootSyncComplete record")]
    Storage(#[source] StorageError),
}

#[derive(Debug, thiserror::Error)]
enum ActiveReblitBootSyncCompleteAuthorityErrorKind {
    #[error("decode and validate the exact v3 ActiveReblit BootSyncComplete journal record")]
    Record(#[source] CodecError),
    #[error("the exact ActiveReblit BootSyncComplete journal-record binding changed")]
    JournalRecordBindingChanged,
    #[error("the exact ActiveReblit CommitDecided successor binding changed")]
    SuccessorRecordBindingChanged,
    #[error("read or revalidate the exact bound ActiveReblit transition journal")]
    Journal(#[source] StorageError),
    #[error("strictly load canonical promoted boot-publication receipt state")]
    ReceiptState(#[source] db::state::BootPublicationReceiptStateError),
    #[error("promoted boot-publication receipt state is internally inconsistent")]
    ReceiptCorrelation(#[source] db::state::ExactPromotedBootPublicationReceiptStateError),
    #[error("inspect exact cleared ActiveReblit state and metadata provenance")]
    Inspection(#[source] InspectionError),
    #[error("load the complete ActiveReblit target state")]
    StateDatabase(#[source] db::Error),
    #[error("exact ActiveReblit boot-completion database evidence changed")]
    DatabaseEvidenceChanged,
    #[error("prove exact live active-state selection for ActiveReblit boot completion")]
    ActiveState(#[source] super::super::Error),
    #[error("ActiveReblit boot completion requires active state {expected}, found {actual:?}")]
    ActiveSelectionMismatch {
        expected: state::Id,
        actual: Option<state::Id>,
    },
    #[error("revalidate exact ActiveReblit boot-completion namespace evidence")]
    Namespace(#[source] ActiveReblitBootSyncCompleteNamespaceError),
    #[error("exact ActiveReblit BootSyncComplete route evidence changed")]
    RouteEvidenceChanged,
    #[error("the exact ActiveReblit boot-completion active selection changed during admission")]
    ActiveSelectionChanged,
    #[error("the retained record is not the exact ActiveReblit CommitDecided successor")]
    UnexpectedSuccessor,
    #[error("revalidate retained mutable installation namespace")]
    Installation(#[source] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_active_reblit_boot_sync_complete_database_captures(
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
