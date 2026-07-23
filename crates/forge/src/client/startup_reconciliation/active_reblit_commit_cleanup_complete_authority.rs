//! Exact read-only startup authority for advancing a completed ActiveReblit
//! cleanup record to `Complete` while retaining stable receipt-chain evidence,
//! which may be empty for the exact no-boot route.

mod retained_binding;

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
    Ready(ActiveReblitCommitCleanupCompleteAuthority<'reservation>),
}

/// Non-replayable authority for one exact bound `CommitCleanupComplete` record.
pub(in crate::client) struct ActiveReblitCommitCleanupCompleteAuthority<'reservation> {
    evidence: ActiveReblitCommitCleanupCompleteEvidence<'reservation>,
}

/// Evidence surviving the one bound advance, without second-advance authority.
pub(in crate::client) struct ActiveReblitCommitCleanupCompletePostAdvanceAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    completed_record: TransitionRecord,
    database: ActiveReblitCommitCleanupCompleteDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: ActiveReblitCommitCleanupFinishNamespaceProof,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

struct ActiveReblitCommitCleanupCompleteEvidence<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: ActiveReblitCommitCleanupCompleteDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: ActiveReblitCommitCleanupFinishNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[derive(Debug, Eq, PartialEq)]
struct ActiveReblitCommitCleanupCompleteDatabaseEvidence {
    route: ActiveReblitCommitCleanupCompleteRouteEvidence,
    context: DatabaseEvidence,
    state: State,
}

/// Disjoint route evidence retained across the sole bound advance. The no-boot
/// route treats the current installed receipt chain as inert equality evidence
/// only; it grants no receipt correlation or mutation authority.
#[derive(Debug, Eq, PartialEq)]
enum ActiveReblitCommitCleanupCompleteRouteEvidence {
    ReceiptBacked {
        pair: crate::boot_publication::BootPublicationReceiptPair,
        receipt: db::state::BootPublicationReceiptState,
    },
    NoBoot {
        inert_receipt_chain: db::state::CurrentExactPromotedBootPublicationReceiptChain,
    },
}

enum ActiveReblitCommitCleanupCompleteRoutePlan {
    ReceiptBacked(crate::boot_publication::BootPublicationReceiptPair),
    NoBoot,
}

enum ActiveReblitCommitCleanupCompleteDatabaseInspection {
    Exact(ActiveReblitCommitCleanupCompleteDatabaseEvidence),
    Incompatible,
}

enum ActiveReblitCommitCleanupCompleteCapture<'reservation> {
    NotApplicable,
    Deferred,
    Apply,
    Ready(ActiveReblitCommitCleanupCompleteAuthority<'reservation>),
}

impl ActiveReblitCommitCleanupCompleteAuthority<'_> {
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
        let captured = Self::capture_with_record_binding(
            installation,
            journal,
            state_db,
            active_state_reservation,
            record,
            || {
                installation.revalidate_mutable_namespace()?;
                let binding = journal.record_binding(
                    installation.retained_mutable_cast_directory()?,
                    record,
                )?;
                installation.revalidate_mutable_namespace()?;
                Ok(binding)
            },
        )?;
        Ok(match captured {
            ActiveReblitCommitCleanupCompleteCapture::NotApplicable => {
                ActiveReblitCommitCleanupCompleteAdmission::NotApplicable
            }
            ActiveReblitCommitCleanupCompleteCapture::Deferred
            | ActiveReblitCommitCleanupCompleteCapture::Apply => {
                ActiveReblitCommitCleanupCompleteAdmission::Deferred
            }
            ActiveReblitCommitCleanupCompleteCapture::Ready(authority) => {
                ActiveReblitCommitCleanupCompleteAdmission::Ready(authority)
            }
        })
    }

    fn capture_with_record_binding<'reservation>(
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        capture_binding: impl FnOnce() -> Result<
            TransitionJournalRecordBinding,
            ActiveReblitCommitCleanupCompleteAuthorityError,
        >,
    ) -> Result<
        ActiveReblitCommitCleanupCompleteCapture<'reservation>,
        ActiveReblitCommitCleanupCompleteAuthorityError,
    > {
        if record.operation != Operation::ActiveReblit
            || record.phase != Phase::CommitCleanupComplete
        {
            return Ok(ActiveReblitCommitCleanupCompleteCapture::NotApplicable);
        }

        // Bind the non-clone source inode before inspecting any mutable
        // database, active-selection, or namespace evidence.
        let journal_record_binding = capture_binding()?;
        require_exact_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;

        let receipt_correlation = record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::Record)?;
        let Some(route_plan) = exact_route_plan(record, receipt_correlation) else {
            return Ok(ActiveReblitCommitCleanupCompleteCapture::Deferred);
        };
        let database_before = match inspect_current_database_for_plan(record, &route_plan, state_db)? {
            ActiveReblitCommitCleanupCompleteDatabaseInspection::Exact(database) => database,
            ActiveReblitCommitCleanupCompleteDatabaseInspection::Incompatible => {
                return Ok(ActiveReblitCommitCleanupCompleteCapture::Deferred);
            }
        };
        let active_state = match capture_exact_active_state(
            record,
            installation,
            active_state_reservation,
        )? {
            Some(active_state) => active_state,
            None => return Ok(ActiveReblitCommitCleanupCompleteCapture::Deferred),
        };
        let inspection = match ActiveReblitCommitCleanupNamespaceInspection::begin(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(inspection) => inspection,
            Err(source) if active_reblit_commit_cleanup_namespace_error_is_mismatch(&source) => {
                return Ok(ActiveReblitCommitCleanupCompleteCapture::Deferred);
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
                return Ok(ActiveReblitCommitCleanupCompleteCapture::Apply);
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
            database: database_after,
            active_state,
            namespace,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        };
        Ok(ActiveReblitCommitCleanupCompleteCapture::Ready(
            ActiveReblitCommitCleanupCompleteAuthority { evidence },
        ))
    }
}

impl ActiveReblitCommitCleanupCompleteEvidence<'_> {
    fn revalidate(
        &self,
        journal: &TransitionJournalStore,
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
            inspect_current_database(&self.record, &self.database.route, &self.state_db)?,
        )?;
        require_exact_active_state(&self.record, &self.installation, &self.active_state)?;
        self.namespace.revalidate(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        let after = require_exact_database(
            &self.database,
            inspect_current_database(&self.record, &self.database.route, &self.state_db)?,
        )?;
        require_exact_active_state(&self.record, &self.installation, &self.active_state)?;
        if before != after || !record_plan_is_exact(&self.record, &self.database.route) {
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

impl<'reservation> ActiveReblitCommitCleanupCompleteAuthority<'reservation> {
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
        self.evidence.revalidate(journal)?;
        let successor = self.evidence.record.forward_successor(None)?;
        if !exact_complete_successor(
            &self.evidence.record,
            &successor,
            &self.evidence.database.route,
        )? {
            return Err(
                ActiveReblitCommitCleanupCompleteRecordAdvanceError::UnexpectedSuccessor,
            );
        }
        let ActiveReblitCommitCleanupCompleteEvidence {
            installation,
            state_db,
            record,
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
            &self.database.route,
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
            inspect_current_database(successor, &self.database.route, &self.state_db)?,
        )?;
        require_exact_active_state(successor, &self.installation, &self.active_state)?;
        self.namespace.revalidate_completed_namespace(
            &self.installation,
            &self.completed_record,
        )?;
        let after = require_exact_database(
            &self.database,
            inspect_current_database(successor, &self.database.route, &self.state_db)?,
        )?;
        require_exact_active_state(successor, &self.installation, &self.active_state)?;
        if before != after {
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
    route: &ActiveReblitCommitCleanupCompleteRouteEvidence,
    state_db: &db::state::Database,
) -> Result<
    ActiveReblitCommitCleanupCompleteDatabaseInspection,
    ActiveReblitCommitCleanupCompleteAuthorityError,
> {
    match route {
        ActiveReblitCommitCleanupCompleteRouteEvidence::ReceiptBacked { pair, .. } => {
            inspect_receipt_backed_database(record, *pair, state_db)
        }
        ActiveReblitCommitCleanupCompleteRouteEvidence::NoBoot { .. } => {
            inspect_no_boot_database(record, state_db)
        }
    }
}

fn inspect_current_database_for_plan(
    record: &TransitionRecord,
    route: &ActiveReblitCommitCleanupCompleteRoutePlan,
    state_db: &db::state::Database,
) -> Result<
    ActiveReblitCommitCleanupCompleteDatabaseInspection,
    ActiveReblitCommitCleanupCompleteAuthorityError,
> {
    match route {
        ActiveReblitCommitCleanupCompleteRoutePlan::ReceiptBacked(pair) => {
            inspect_receipt_backed_database(record, *pair, state_db)
        }
        ActiveReblitCommitCleanupCompleteRoutePlan::NoBoot => {
            inspect_no_boot_database(record, state_db)
        }
    }
}

fn inspect_receipt_backed_database(
    record: &TransitionRecord,
    pair: crate::boot_publication::BootPublicationReceiptPair,
    state_db: &db::state::Database,
) -> Result<
    ActiveReblitCommitCleanupCompleteDatabaseInspection,
    ActiveReblitCommitCleanupCompleteAuthorityError,
> {
    let receipt_before = match load_exact_promoted_receipt(state_db, record, pair)? {
        Some(receipt) => receipt,
        None => return Ok(ActiveReblitCommitCleanupCompleteDatabaseInspection::Incompatible),
    };
    let Some((context, state)) = inspect_context_and_state(record, state_db)? else {
        return Ok(ActiveReblitCommitCleanupCompleteDatabaseInspection::Incompatible);
    };
    let receipt_after = match load_exact_promoted_receipt(state_db, record, pair)? {
        Some(receipt) => receipt,
        None => {
            return Err(
                ActiveReblitCommitCleanupCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into(),
            );
        }
    };
    if receipt_before != receipt_after {
        return Err(
            ActiveReblitCommitCleanupCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into(),
        );
    }
    Ok(ActiveReblitCommitCleanupCompleteDatabaseInspection::Exact(
        ActiveReblitCommitCleanupCompleteDatabaseEvidence {
            route: ActiveReblitCommitCleanupCompleteRouteEvidence::ReceiptBacked {
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
    ActiveReblitCommitCleanupCompleteDatabaseInspection,
    ActiveReblitCommitCleanupCompleteAuthorityError,
> {
    let receipt_chain_before = load_inert_no_boot_receipt_chain(record, state_db)?;
    let Some((context, state)) = inspect_context_and_state(record, state_db)? else {
        return Ok(ActiveReblitCommitCleanupCompleteDatabaseInspection::Incompatible);
    };
    let receipt_chain_after = load_inert_no_boot_receipt_chain(record, state_db)?;
    if receipt_chain_before != receipt_chain_after {
        return Err(
            ActiveReblitCommitCleanupCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into(),
        );
    }
    Ok(ActiveReblitCommitCleanupCompleteDatabaseInspection::Exact(
        ActiveReblitCommitCleanupCompleteDatabaseEvidence {
            route: ActiveReblitCommitCleanupCompleteRouteEvidence::NoBoot {
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
    ActiveReblitCommitCleanupCompleteAuthorityError,
> {
    let chain = state_db
        .load_current_exact_promoted_boot_publication_receipt_chain()
        .map_err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::ReceiptChain)?;
    if matches!(
        &chain,
        db::state::CurrentExactPromotedBootPublicationReceiptChain::Installed(installed)
            if installed.installed_receipt().body().transition_id() == &record.transition_id
    ) {
        return Err(
            ActiveReblitCommitCleanupCompleteAuthorityErrorKind::ReceiptChainMatchesNoBootTransition
                .into(),
        );
    }
    Ok(chain)
}

fn inspect_context_and_state(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<Option<(DatabaseEvidence, State)>, ActiveReblitCommitCleanupCompleteAuthorityError> {
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
        .map_err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::StateDatabase)?;
    if state.id != state_id {
        return Err(
            ActiveReblitCommitCleanupCompleteAuthorityErrorKind::DatabaseEvidenceChanged.into(),
        );
    }
    Ok(Some((context, state)))
}

fn load_exact_promoted_receipt(
    state_db: &db::state::Database,
    record: &TransitionRecord,
    pair: crate::boot_publication::BootPublicationReceiptPair,
) -> Result<
    Option<db::state::BootPublicationReceiptState>,
    ActiveReblitCommitCleanupCompleteAuthorityError,
> {
    match state_db
        .load_exact_promoted_boot_publication_receipt_state(&record.transition_id, &pair)
    {
        Ok(receipt) => Ok(Some(receipt)),
        Err(db::state::ExactPromotedBootPublicationReceiptStateError::State(source)) => {
            Err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::ReceiptState(source).into())
        }
        Err(source @ db::state::ExactPromotedBootPublicationReceiptStateError::PendingBodyPresent)
        | Err(source @ db::state::ExactPromotedBootPublicationReceiptStateError::MissingCommittedBody)
        | Err(
            source @ db::state::ExactPromotedBootPublicationReceiptStateError::CommittedBodyFingerprintMismatch {
                ..
            },
        ) => Err(
            ActiveReblitCommitCleanupCompleteAuthorityErrorKind::ReceiptCorrelation(source).into(),
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

fn exact_route_plan(
    record: &TransitionRecord,
    receipt_correlation: Option<crate::boot_publication::BootPublicationReceiptPair>,
) -> Option<ActiveReblitCommitCleanupCompleteRoutePlan> {
    if record.operation != Operation::ActiveReblit
        || record.phase != Phase::CommitCleanupComplete
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
            Some(ActiveReblitCommitCleanupCompleteRoutePlan::ReceiptBacked(pair))
        }
        (12, false, None)
            if record.options.run_system_triggers && !record.options.archive_previous =>
        {
            Some(ActiveReblitCommitCleanupCompleteRoutePlan::NoBoot)
        }
        _ => None,
    }
}

fn record_plan_is_exact(
    record: &TransitionRecord,
    route: &ActiveReblitCommitCleanupCompleteRouteEvidence,
) -> bool {
    let common = record.operation == Operation::ActiveReblit
        && record.phase == Phase::CommitCleanupComplete
        && record.rollback.is_none()
        && same_nonempty_candidate_and_previous(record);
    common
        && match route {
            ActiveReblitCommitCleanupCompleteRouteEvidence::ReceiptBacked { pair, .. } => {
                record.options.run_boot_sync
                    && record.boot_publication_receipts == Some(*pair)
            }
            ActiveReblitCommitCleanupCompleteRouteEvidence::NoBoot { .. } => {
                record.generation == 12
                    && record.options.run_system_triggers
                    && !record.options.run_boot_sync
                    && !record.options.archive_previous
                    && record.boot_publication_receipts.is_none()
            }
        }
}

fn exact_complete_successor(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    route: &ActiveReblitCommitCleanupCompleteRouteEvidence,
) -> Result<bool, CodecError> {
    let source_pair = source.boot_publication_receipt_correlation()?;
    let successor_pair = successor.boot_publication_receipt_correlation()?;
    let successor_route_is_exact = match route {
        ActiveReblitCommitCleanupCompleteRouteEvidence::ReceiptBacked { pair, .. } => {
            successor.options.run_boot_sync
                && source_pair == Some(*pair)
                && successor_pair == Some(*pair)
        }
        ActiveReblitCommitCleanupCompleteRouteEvidence::NoBoot { .. } => {
            successor.generation == 13
                && successor.options.run_system_triggers
                && !successor.options.run_boot_sync
                && !successor.options.archive_previous
                && source_pair.is_none()
                && successor_pair.is_none()
        }
    };
    Ok(record_plan_is_exact(source, route)
        && successor.operation == Operation::ActiveReblit
        && successor.phase == Phase::Complete
        && successor.rollback.is_none()
        && same_nonempty_candidate_and_previous(successor)
        && successor_route_is_exact
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
    #[error("load exact promoted boot-publication receipt state")]
    ReceiptState(#[source] db::state::BootPublicationReceiptStateError),
    #[error("authenticate exact promoted boot-publication receipt correlation")]
    ReceiptCorrelation(#[source] db::state::ExactPromotedBootPublicationReceiptStateError),
    #[error("load current exact promoted boot-publication receipt chain as inert evidence")]
    ReceiptChain(#[source] db::state::CurrentExactPromotedBootPublicationReceiptChainError),
    #[error("the no-boot transition unexpectedly owns the installed receipt chain")]
    ReceiptChainMatchesNoBootTransition,
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
    #[error("the retained live CommitCleanupComplete binding is not the exact generation-14 Finish route")]
    RetainedCommitCleanupCompleteRejected,
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
pub(in crate::client) enum ActiveReblitCommitCleanupCompleteRecordAdvanceError {
    #[error("revalidate exact promoted CommitCleanupComplete authority")]
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
