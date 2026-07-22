//! Exact startup authority for retiring the completed ActiveReblit receipt
//! head and advancing the bound record to `Complete`.

use crate::{
    Installation, State, db, state,
    transition_journal::{
        CodecError, Operation, Phase, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::{ActiveStateReservation, ActiveStateSnapshot},
    startup_gate::ActiveReblitCommitCleanupCompleteSeal,
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

/// Read-only result for the exact startup checkpoint.
pub(in crate::client) enum ActiveReblitCommitCleanupCompleteAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Apply(ActiveReblitCommitCleanupCompleteApplyAuthority<'reservation>),
    Finish(ActiveReblitCommitCleanupCompleteFinishAuthority<'reservation>),
}

pub(in crate::client) struct ActiveReblitCommitCleanupCompleteAuthority;

/// Consuming authority for the sole receipt-head retirement attempt.
pub(in crate::client) struct ActiveReblitCommitCleanupCompleteApplyAuthority<'reservation> {
    evidence: ActiveReblitCommitCleanupCompleteEvidence<'reservation>,
}

/// Consuming authority proving the exact receipt head is already retired.
pub(in crate::client) struct ActiveReblitCommitCleanupCompleteFinishAuthority<'reservation> {
    evidence: ActiveReblitCommitCleanupCompleteEvidence<'reservation>,
}

/// Exact retired evidence accepted by the sole `Complete` persistence edge.
pub(in crate::client) struct ActiveReblitCommitCleanupCompleteRetiredAuthority<'reservation> {
    evidence: ActiveReblitCommitCleanupCompleteEvidence<'reservation>,
}

/// Evidence surviving the one bound advance, without second-advance authority.
pub(in crate::client) struct ActiveReblitCommitCleanupCompletePostAdvanceAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    completed_record: TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
    database: ActiveReblitCommitCleanupCompleteDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: ActiveReblitCommitCleanupFinishNamespaceProof,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

struct ActiveReblitCommitCleanupCompleteEvidence<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
    database: ActiveReblitCommitCleanupCompleteDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: ActiveReblitCommitCleanupFinishNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[derive(Debug, Eq, PartialEq)]
struct ActiveReblitCommitCleanupCompleteDatabaseEvidence {
    receipt: db::state::BootPublicationReceiptRetirementDurableState,
    context: DatabaseEvidence,
    state: State,
}

enum ActiveReblitCommitCleanupCompleteDatabaseInspection {
    Exact(ActiveReblitCommitCleanupCompleteDatabaseEvidence),
    Incompatible,
}

impl ActiveReblitCommitCleanupCompleteAuthority {
    pub(in crate::client) fn capture<'reservation>(
        _seal: &ActiveReblitCommitCleanupCompleteSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<
        ActiveReblitCommitCleanupCompleteAdmission<'reservation>,
        ActiveReblitCommitCleanupCompleteAuthorityError,
    > {
        if record.operation != Operation::ActiveReblit
            || record.phase != Phase::CommitCleanupComplete
        {
            return Ok(ActiveReblitCommitCleanupCompleteAdmission::NotApplicable);
        }

        // Bind the non-clone source inode before inspecting any mutable
        // database, active-selection, or namespace evidence.
        installation.revalidate_mutable_namespace()?;
        let journal_record_binding = journal.record_binding(
            installation.retained_mutable_cast_directory()?,
            record,
        )?;
        installation.revalidate_mutable_namespace()?;

        let receipt_pair = match record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::Record)?
        {
            Some(pair)
                if record.rollback.is_none()
                    && record.options.run_boot_sync
                    && same_nonempty_candidate_and_previous(record) => pair,
            _ => return Ok(ActiveReblitCommitCleanupCompleteAdmission::Deferred),
        };
        let database_before = match inspect_current_database(record, receipt_pair, state_db)? {
            ActiveReblitCommitCleanupCompleteDatabaseInspection::Exact(database) => database,
            ActiveReblitCommitCleanupCompleteDatabaseInspection::Incompatible => {
                return Ok(ActiveReblitCommitCleanupCompleteAdmission::Deferred);
            }
        };
        let active_state = match capture_exact_active_state(
            record,
            installation,
            active_state_reservation,
        )? {
            Some(active_state) => active_state,
            None => return Ok(ActiveReblitCommitCleanupCompleteAdmission::Deferred),
        };
        let inspection = match ActiveReblitCommitCleanupNamespaceInspection::begin(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(inspection) => inspection,
            Err(source) if active_reblit_commit_cleanup_namespace_error_is_mismatch(&source) => {
                return Ok(ActiveReblitCommitCleanupCompleteAdmission::Deferred);
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
                return Ok(ActiveReblitCommitCleanupCompleteAdmission::Deferred);
            }
        };
        let database_after = require_exact_database(
            &database_before,
            inspect_current_database(record, receipt_pair, state_db)?,
        )?;
        require_exact_active_state(record, installation, &active_state)?;
        if database_before != database_after || !record_plan_is_exact(record, receipt_pair) {
            return Err(
                ActiveReblitCommitCleanupCompleteAuthorityErrorKind::RouteEvidenceChanged.into(),
            );
        }
        require_exact_record_binding(
            installation,
            journal,
            &journal_record_binding,
            record,
        )?;
        installation.revalidate_mutable_namespace()?;

        let evidence = ActiveReblitCommitCleanupCompleteEvidence {
            installation: installation.clone(),
            state_db: state_db.clone(),
            record: record.clone(),
            receipt_pair,
            database: database_after,
            active_state,
            namespace,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        };
        Ok(match evidence.database.receipt {
            db::state::BootPublicationReceiptRetirementDurableState::Promoted => {
                ActiveReblitCommitCleanupCompleteAdmission::Apply(
                    ActiveReblitCommitCleanupCompleteApplyAuthority { evidence },
                )
            }
            db::state::BootPublicationReceiptRetirementDurableState::Retired => {
                ActiveReblitCommitCleanupCompleteAdmission::Finish(
                    ActiveReblitCommitCleanupCompleteFinishAuthority { evidence },
                )
            }
        })
    }
}

impl<'reservation> ActiveReblitCommitCleanupCompleteApplyAuthority<'reservation> {
    /// Consume Apply authority through at most one retirement invocation.
    pub(in crate::client) fn retire(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        ActiveReblitCommitCleanupCompleteRetiredAuthority<'reservation>,
        ActiveReblitCommitCleanupCompleteApplyError,
    > {
        let mut evidence = self.evidence;
        evidence.revalidate(
            journal,
            db::state::BootPublicationReceiptRetirementDurableState::Promoted,
        )?;
        evidence
            .state_db
            .retire_promoted_boot_publication_receipt_head(
                &evidence.record.transition_id,
                &evidence.receipt_pair,
            )?;
        evidence.database.receipt =
            db::state::BootPublicationReceiptRetirementDurableState::Retired;
        evidence.revalidate(
            journal,
            db::state::BootPublicationReceiptRetirementDurableState::Retired,
        )?;
        Ok(ActiveReblitCommitCleanupCompleteRetiredAuthority { evidence })
    }
}

impl<'reservation> ActiveReblitCommitCleanupCompleteFinishAuthority<'reservation> {
    /// Consume Finish authority without invoking receipt retirement.
    pub(in crate::client) fn into_retired(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        ActiveReblitCommitCleanupCompleteRetiredAuthority<'reservation>,
        ActiveReblitCommitCleanupCompleteAuthorityError,
    > {
        self.evidence.revalidate(
            journal,
            db::state::BootPublicationReceiptRetirementDurableState::Retired,
        )?;
        Ok(ActiveReblitCommitCleanupCompleteRetiredAuthority {
            evidence: self.evidence,
        })
    }
}

impl ActiveReblitCommitCleanupCompleteEvidence<'_> {
    fn revalidate(
        &self,
        journal: &TransitionJournalStore,
        receipt: db::state::BootPublicationReceiptRetirementDurableState,
    ) -> Result<(), ActiveReblitCommitCleanupCompleteAuthorityError> {
        require_exact_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        self.installation.revalidate_mutable_namespace()?;
        let before = require_exact_database(
            &self.database,
            inspect_current_database(&self.record, self.receipt_pair, &self.state_db)?,
        )?;
        if before.receipt != receipt {
            return Err(
                ActiveReblitCommitCleanupCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into(),
            );
        }
        require_exact_active_state(&self.record, &self.installation, &self.active_state)?;
        self.namespace.revalidate(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        let after = require_exact_database(
            &self.database,
            inspect_current_database(&self.record, self.receipt_pair, &self.state_db)?,
        )?;
        require_exact_active_state(&self.record, &self.installation, &self.active_state)?;
        if before != after || !record_plan_is_exact(&self.record, self.receipt_pair) {
            return Err(
                ActiveReblitCommitCleanupCompleteAuthorityErrorKind::RouteEvidenceChanged.into(),
            );
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
}

impl<'reservation> ActiveReblitCommitCleanupCompleteRetiredAuthority<'reservation> {
    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.evidence.installation
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.evidence.record
    }

    /// Derive and publish the sole legal `Complete` successor internally.
    pub(in crate::client) fn advance_to_complete(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        (
            TransitionRecord,
            TransitionJournalRecordBinding,
            ActiveReblitCommitCleanupCompletePostAdvanceAuthority<'reservation>,
        ),
        ActiveReblitCommitCleanupCompleteRecordAdvanceError,
    > {
        self.evidence.revalidate(
            journal,
            db::state::BootPublicationReceiptRetirementDurableState::Retired,
        )?;
        let successor = self.evidence.record.forward_successor(None)?;
        if !exact_complete_successor(
            &self.evidence.record,
            &successor,
            self.evidence.receipt_pair,
        )? {
            return Err(
                ActiveReblitCommitCleanupCompleteRecordAdvanceError::UnexpectedSuccessor,
            );
        }
        let ActiveReblitCommitCleanupCompleteEvidence {
            installation,
            state_db,
            record,
            receipt_pair,
            database,
            active_state,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        } = self.evidence;
        let binding = journal
            .advance_record_binding(
                installation.retained_mutable_cast_directory()?,
                journal_record_binding,
                &successor,
            )
            .map_err(|source| ActiveReblitCommitCleanupCompleteRecordAdvanceError::Storage {
                source,
                successor: Box::new(successor.clone()),
            })?;
        Ok((
            successor,
            binding,
            ActiveReblitCommitCleanupCompletePostAdvanceAuthority {
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

impl ActiveReblitCommitCleanupCompletePostAdvanceAuthority<'_> {
    pub(in crate::client) fn revalidate_successor_same_store(
        &self,
        journal: &TransitionJournalStore,
        binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
    ) -> Result<(), ActiveReblitCommitCleanupCompleteAuthorityError> {
        self.revalidate_successor(journal, binding, successor, BindingMode::SameStore)
    }

    pub(in crate::client) fn revalidate_successor_reopened(
        &self,
        journal: &TransitionJournalStore,
        binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
    ) -> Result<(), ActiveReblitCommitCleanupCompleteAuthorityError> {
        self.revalidate_successor(journal, binding, successor, BindingMode::Reopened)
    }

    fn revalidate_successor(
        &self,
        journal: &TransitionJournalStore,
        binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
        mode: BindingMode,
    ) -> Result<(), ActiveReblitCommitCleanupCompleteAuthorityError> {
        require_successor_binding(&self.installation, journal, binding, successor, mode)?;
        if !exact_complete_successor(
            &self.completed_record,
            successor,
            self.receipt_pair,
        )
        .map_err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::Record)?
        {
            return Err(
                ActiveReblitCommitCleanupCompleteAuthorityErrorKind::UnexpectedSuccessor.into(),
            );
        }
        self.installation.revalidate_mutable_namespace()?;
        let before = require_exact_database(
            &self.database,
            inspect_current_database(successor, self.receipt_pair, &self.state_db)?,
        )?;
        require_exact_active_state(successor, &self.installation, &self.active_state)?;
        self.namespace.revalidate_completed_namespace(
            &self.installation,
            &self.completed_record,
        )?;
        let after = require_exact_database(
            &self.database,
            inspect_current_database(successor, self.receipt_pair, &self.state_db)?,
        )?;
        require_exact_active_state(successor, &self.installation, &self.active_state)?;
        if before != after
            || after.receipt
                != db::state::BootPublicationReceiptRetirementDurableState::Retired
        {
            return Err(
                ActiveReblitCommitCleanupCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into(),
            );
        }
        require_successor_binding(&self.installation, journal, binding, successor, mode)?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum BindingMode {
    SameStore,
    Reopened,
}

fn inspect_current_database(
    record: &TransitionRecord,
    pair: crate::boot_publication::BootPublicationReceiptPair,
    state_db: &db::state::Database,
) -> Result<
    ActiveReblitCommitCleanupCompleteDatabaseInspection,
    ActiveReblitCommitCleanupCompleteAuthorityError,
> {
    let receipt_before = match state_db
        .inspect_exact_boot_publication_receipt_retirement_state(&record.transition_id, &pair)
    {
        Ok(receipt) => receipt,
        Err(db::state::BootPublicationReceiptRetirementError::StateMismatch { .. }) => {
            return Ok(ActiveReblitCommitCleanupCompleteDatabaseInspection::Incompatible);
        }
        Err(source) => {
            return Err(
                ActiveReblitCommitCleanupCompleteAuthorityErrorKind::Receipt(source).into(),
            );
        }
    };
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    if !existing_state_context_is_exact(record, &context) {
        return Ok(ActiveReblitCommitCleanupCompleteDatabaseInspection::Incompatible);
    }
    let state_id = state::Id::from(record.candidate.id.expect("checked exact ActiveReblit state"));
    let state = state_db
        .get(state_id)
        .map_err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::StateDatabase)?;
    if state.id != state_id {
        return Err(
            ActiveReblitCommitCleanupCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into(),
        );
    }
    let receipt_after = state_db
        .inspect_exact_boot_publication_receipt_retirement_state(&record.transition_id, &pair)
        .map_err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::Receipt)?;
    if receipt_before != receipt_after {
        return Err(
            ActiveReblitCommitCleanupCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into(),
        );
    }
    Ok(ActiveReblitCommitCleanupCompleteDatabaseInspection::Exact(
        ActiveReblitCommitCleanupCompleteDatabaseEvidence {
            receipt: receipt_after,
            context,
            state,
        },
    ))
}

fn require_exact_database(
    expected: &ActiveReblitCommitCleanupCompleteDatabaseEvidence,
    actual: ActiveReblitCommitCleanupCompleteDatabaseInspection,
) -> Result<
    ActiveReblitCommitCleanupCompleteDatabaseEvidence,
    ActiveReblitCommitCleanupCompleteAuthorityError,
> {
    match actual {
        ActiveReblitCommitCleanupCompleteDatabaseInspection::Exact(actual)
            if actual == *expected => Ok(actual),
        _ => Err(
            ActiveReblitCommitCleanupCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into(),
        ),
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
) -> Result<Option<ActiveStateSnapshot>, ActiveReblitCommitCleanupCompleteAuthorityError> {
    let snapshot = reservation
        .capture_for_startup_recovery(installation)
        .map_err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::ActiveState)?;
    let expected = state::Id::from(record.candidate.id.expect("checked exact ActiveReblit state"));
    if snapshot.active() != Some(expected) {
        return Ok(None);
    }
    snapshot
        .revalidate(installation)
        .map_err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::ActiveState)?;
    Ok(Some(snapshot))
}

fn require_exact_active_state(
    record: &TransitionRecord,
    installation: &Installation,
    snapshot: &ActiveStateSnapshot,
) -> Result<(), ActiveReblitCommitCleanupCompleteAuthorityError> {
    let expected = state::Id::from(record.candidate.id.expect("retained exact ActiveReblit state"));
    if snapshot.active() != Some(expected) {
        return Err(
            ActiveReblitCommitCleanupCompleteAuthorityErrorKind::ActiveSelectionChanged.into(),
        );
    }
    snapshot
        .revalidate(installation)
        .map_err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::ActiveState)?;
    Ok(())
}

fn same_nonempty_candidate_and_previous(record: &TransitionRecord) -> bool {
    record.candidate.id.is_some() && record.candidate.id == record.previous.id
}

fn record_plan_is_exact(
    record: &TransitionRecord,
    pair: crate::boot_publication::BootPublicationReceiptPair,
) -> bool {
    record.operation == Operation::ActiveReblit
        && record.phase == Phase::CommitCleanupComplete
        && record.rollback.is_none()
        && record.options.run_boot_sync
        && same_nonempty_candidate_and_previous(record)
        && record.boot_publication_receipts == Some(pair)
}

fn exact_complete_successor(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    pair: crate::boot_publication::BootPublicationReceiptPair,
) -> Result<bool, CodecError> {
    let source_pair = source.boot_publication_receipt_correlation()?;
    let successor_pair = successor.boot_publication_receipt_correlation()?;
    Ok(record_plan_is_exact(source, pair)
        && successor.operation == Operation::ActiveReblit
        && successor.phase == Phase::Complete
        && successor.rollback.is_none()
        && successor.options.run_boot_sync
        && same_nonempty_candidate_and_previous(successor)
        && source_pair == Some(pair)
        && successor_pair == Some(pair)
        && successor.generation == source.generation.checked_add(1).unwrap_or(0)
        && successor.format == source.format
        && successor.version == source.version
        && successor.transition_id == source.transition_id
        && successor.creation_epoch == source.creation_epoch
        && successor.candidate == source.candidate
        && successor.previous == source.previous
        && successor.options == source.options
        && successor.quarantine_name == source.quarantine_name)
}

fn require_exact_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), ActiveReblitCommitCleanupCompleteAuthorityError> {
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_store_binding(binding)
        && journal.has_record_binding(cast, binding, record)?
    {
        Ok(())
    } else {
        Err(
            ActiveReblitCommitCleanupCompleteAuthorityErrorKind::JournalRecordBindingChanged.into(),
        )
    }
}

fn require_successor_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
    mode: BindingMode,
) -> Result<(), ActiveReblitCommitCleanupCompleteAuthorityError> {
    let cast = installation.retained_mutable_cast_directory()?;
    let exact = match mode {
        BindingMode::SameStore => {
            journal.has_record_store_binding(binding)
                && journal.has_record_binding(cast, binding, successor)?
        }
        BindingMode::Reopened => {
            journal.has_reopened_record_binding(cast, binding, successor)?
        }
    };
    if exact {
        Ok(())
    } else {
        Err(
            ActiveReblitCommitCleanupCompleteAuthorityErrorKind::SuccessorRecordBindingChanged.into(),
        )
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct ActiveReblitCommitCleanupCompleteAuthorityError(
    #[from] ActiveReblitCommitCleanupCompleteAuthorityErrorKind,
);

#[derive(Debug, thiserror::Error)]
enum ActiveReblitCommitCleanupCompleteAuthorityErrorKind {
    #[error("validate exact v3 ActiveReblit CommitCleanupComplete record")]
    Record(#[source] CodecError),
    #[error("the exact CommitCleanupComplete source binding changed")]
    JournalRecordBindingChanged,
    #[error("the exact Complete successor binding changed")]
    SuccessorRecordBindingChanged,
    #[error("inspect exact promoted or retired boot-publication receipt chain")]
    Receipt(#[source] db::state::BootPublicationReceiptRetirementError),
    #[error("inspect exact cleared ActiveReblit database and provenance")]
    Inspection(#[source] InspectionError),
    #[error("load complete selected ActiveReblit state")]
    StateDatabase(#[source] db::Error),
    #[error("exact CommitCleanupComplete database evidence changed")]
    DatabaseEvidenceChanged,
    #[error("prove exact live active-state selection")]
    ActiveState(#[source] super::super::Error),
    #[error("the selected active state changed")]
    ActiveSelectionChanged,
    #[error("revalidate exact completed cleanup namespace")]
    Namespace(#[source] ActiveReblitCommitCleanupNamespaceError),
    #[error("the exact CommitCleanupComplete route evidence changed")]
    RouteEvidenceChanged,
    #[error("the retained successor is not exact ActiveReblit Complete")]
    UnexpectedSuccessor,
    #[error("revalidate retained mutable installation namespace")]
    Installation(#[source] crate::installation::Error),
    #[error("read or bind retained transition journal")]
    Journal(#[source] StorageError),
}

impl From<InspectionError> for ActiveReblitCommitCleanupCompleteAuthorityError {
    fn from(source: InspectionError) -> Self {
        ActiveReblitCommitCleanupCompleteAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<ActiveReblitCommitCleanupNamespaceError>
    for ActiveReblitCommitCleanupCompleteAuthorityError
{
    fn from(source: ActiveReblitCommitCleanupNamespaceError) -> Self {
        ActiveReblitCommitCleanupCompleteAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for ActiveReblitCommitCleanupCompleteAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        ActiveReblitCommitCleanupCompleteAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for ActiveReblitCommitCleanupCompleteAuthorityError {
    fn from(source: StorageError) -> Self {
        ActiveReblitCommitCleanupCompleteAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum ActiveReblitCommitCleanupCompleteApplyError {
    #[error("revalidate exact promoted CommitCleanupComplete authority")]
    Authority(#[from] ActiveReblitCommitCleanupCompleteAuthorityError),
    #[error("retire exact promoted boot-publication receipt head")]
    Retirement(#[from] db::state::BootPublicationReceiptRetirementError),
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum ActiveReblitCommitCleanupCompleteRecordAdvanceError {
    #[error("revalidate exact retired CommitCleanupComplete authority")]
    Authority(#[from] ActiveReblitCommitCleanupCompleteAuthorityError),
    #[error("derive or validate sole ActiveReblit Complete successor")]
    Record(#[from] CodecError),
    #[error("derived record is not exact ActiveReblit Complete")]
    UnexpectedSuccessor,
    #[error("revalidate retained installation before bound Complete advance")]
    Installation(#[from] crate::installation::Error),
    #[error("advance exact bound CommitCleanupComplete record")]
    Storage {
        #[source]
        source: StorageError,
        successor: Box<TransitionRecord>,
    },
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_active_reblit_commit_cleanup_complete_database_captures(
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
