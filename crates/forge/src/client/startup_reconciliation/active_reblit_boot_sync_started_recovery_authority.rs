//! Sealed read-only recovery authority for an exact promoted ActiveReblit
//! `BootSyncStarted` checkpoint.
//!
//! Exact pending and legacy records remain rollback-eligible. Once the exact
//! receipt has become the committed head, admission retains the source inode,
//! canonical receipt chain, validated cleanup classification, complete state
//! and provenance, live active selection, and descriptor-rooted forward
//! namespace. Capture and revalidation perform no journal, database, boot,
//! namespace, cleanup, or trigger effect.

use crate::{
    Installation, State, db, state,
    transition_journal::{
        CodecError, Operation, Phase, StorageError,
        TransitionJournalRecordBinding, TransitionJournalStore,
        TransitionRecord,
    },
};

use super::super::{
    active_reblit_promoted_boot_cleanup_plan::{
        ActiveReblitPromotedBootCleanupPlan,
        ActiveReblitPromotedBootCleanupPlanError,
    },
    active_state_snapshot::{ActiveStateReservation, ActiveStateSnapshot},
    startup_gate::ActiveReblitBootSyncStartedCleanupSeal,
};
use super::{
    DatabaseEvidence, InspectionError,
    database_ownership_evidence_compatible, inspect_database,
    metadata_provenance_evidence_compatible,
};
use super::activation_namespace::{
    ActiveReblitBootSyncStartedNamespaceError,
    ActiveReblitBootSyncStartedNamespaceInspection,
    ActiveReblitBootSyncStartedNamespaceProof,
    active_reblit_boot_sync_started_namespace_error_is_mismatch,
};

/// Read-only result at the receipt-promotion boundary.
pub(in crate::client) enum ActiveReblitBootSyncStartedRecoveryAdmission<'reservation> {
    NotApplicable,
    RollbackEligible,
    Deferred,
    Ready(ActiveReblitBootSyncStartedRecoveryAuthority<'reservation>),
}

/// Non-replayable exact recovery evidence retained under the startup lock.
///
/// This type intentionally implements neither `Clone` nor `Copy`.
pub(in crate::client) struct ActiveReblitBootSyncStartedRecoveryAuthority<'reservation> {
    cleanup_seal: ActiveReblitBootSyncStartedCleanupSeal,
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
    database: ActiveReblitBootSyncStartedDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    namespace: ActiveReblitBootSyncStartedNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Exact receipt chain, state row, and provenance from one stable database
/// sandwich. This value is deliberately not cloneable.
#[derive(Debug, Eq, PartialEq)]
struct ActiveReblitBootSyncStartedDatabaseEvidence {
    receipt_chain: db::state::ExactPromotedBootPublicationReceiptChain,
    context: DatabaseEvidence,
    state: State,
}

enum ActiveReblitBootSyncStartedDatabaseInspection {
    Exact(ActiveReblitBootSyncStartedDatabaseEvidence),
    Incompatible,
}

enum ReceiptBoundary {
    Pending,
    Promoted,
}

impl<'reservation> ActiveReblitBootSyncStartedRecoveryAuthority<'reservation> {
    /// Capture exact promoted recovery evidence without performing effects.
    pub(in crate::client) fn capture(
        cleanup_seal: ActiveReblitBootSyncStartedCleanupSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<ActiveReblitBootSyncStartedRecoveryAdmission<'reservation>, ActiveReblitBootSyncStartedRecoveryAuthorityError> {
        if record.operation != Operation::ActiveReblit
            || record.phase != Phase::BootSyncStarted
        {
            return Ok(ActiveReblitBootSyncStartedRecoveryAdmission::NotApplicable);
        }

        let Some(receipt_pair) = record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::Record)?
        else {
            return Ok(ActiveReblitBootSyncStartedRecoveryAdmission::RollbackEligible);
        };
        if cleanup_seal.promoted_receipt() != receipt_pair.pending {
            return Err(
                ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::CleanupSealReceiptMismatch
                    .into(),
            );
        }

        // Bind the exact source inode before mutable receipt or state reads.
        installation.revalidate_mutable_namespace()?;
        let journal_record_binding = journal.record_binding(
            installation.retained_mutable_cast_directory()?,
            record,
        )?;
        installation.revalidate_mutable_namespace()?;

        match inspect_receipt_boundary(state_db, record, receipt_pair)? {
            ReceiptBoundary::Pending => {
                return Ok(ActiveReblitBootSyncStartedRecoveryAdmission::RollbackEligible);
            }
            ReceiptBoundary::Promoted => {}
        }
        if !record_plan_is_exact(record, receipt_pair, &cleanup_seal) {
            return Ok(ActiveReblitBootSyncStartedRecoveryAdmission::Deferred);
        }

        let database_before = match inspect_current_database(
            record,
            receipt_pair,
            state_db,
        )? {
            ActiveReblitBootSyncStartedDatabaseInspection::Exact(database) => database,
            ActiveReblitBootSyncStartedDatabaseInspection::Incompatible => {
                return Ok(ActiveReblitBootSyncStartedRecoveryAdmission::Deferred);
            }
        };
        let active_state = match capture_exact_active_state(
            record,
            installation,
            active_state_reservation,
        )? {
            Some(active_state) => active_state,
            None => return Ok(ActiveReblitBootSyncStartedRecoveryAdmission::Deferred),
        };
        let namespace_inspection = match ActiveReblitBootSyncStartedNamespaceInspection::begin(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(inspection) => inspection,
            Err(source)
                if active_reblit_boot_sync_started_namespace_error_is_mismatch(&source) =>
            {
                return Ok(ActiveReblitBootSyncStartedRecoveryAdmission::Deferred);
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
        let database_after = require_exact_database(
            &database_before,
            inspect_current_database(record, receipt_pair, state_db)?,
        )?;
        require_exact_active_state(record, installation, &active_state)?;
        if database_before != database_after
            || !record_plan_is_exact(record, receipt_pair, &cleanup_seal)
        {
            return Err(
                ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::RouteEvidenceChanged
                    .into(),
            );
        }
        require_exact_record_binding(
            installation,
            journal,
            &journal_record_binding,
            record,
        )?;
        installation.revalidate_mutable_namespace()?;

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        Ok(ActiveReblitBootSyncStartedRecoveryAdmission::Ready(Self {
            cleanup_seal,
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

    /// Revalidate binding-first DB -> active/namespace -> DB evidence.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), ActiveReblitBootSyncStartedRecoveryAuthorityError> {
        require_exact_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        self.installation.revalidate_mutable_namespace()?;
        let database_before = require_exact_database(
            &self.database,
            inspect_current_database(
                &self.record,
                self.receipt_pair,
                &self.state_db,
            )?,
        )?;
        require_exact_active_state(
            &self.record,
            &self.installation,
            &self.active_state,
        )?;
        self.namespace.revalidate(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        let database_after = require_exact_database(
            &self.database,
            inspect_current_database(
                &self.record,
                self.receipt_pair,
                &self.state_db,
            )?,
        )?;
        require_exact_active_state(
            &self.record,
            &self.installation,
            &self.active_state,
        )?;
        if database_before != database_after
            || !record_plan_is_exact(
                &self.record,
                self.receipt_pair,
                &self.cleanup_seal,
            )
        {
            return Err(
                ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::RouteEvidenceChanged
                    .into(),
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

    /// Revalidate all retained authority and then rederive the inert cleanup
    /// plan borrowing the exact authenticated receipt chain.
    pub(in crate::client) fn cleanup_plan<'authority>(
        &'authority self,
        journal: &TransitionJournalStore,
    ) -> Result<ActiveReblitPromotedBootCleanupPlan<'authority>, ActiveReblitBootSyncStartedRecoveryAuthorityError> {
        self.revalidate(journal)?;
        let plan = self
            .database
            .receipt_chain
            .prepare_active_reblit_promoted_boot_cleanup_plan()?;
        if plan.promoted_receipt() != self.cleanup_seal.promoted_receipt() {
            return Err(
                ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::CleanupPlanReceiptMismatch
                    .into(),
            );
        }
        Ok(plan)
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.installation
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.record
    }
}

fn inspect_receipt_boundary(
    state_db: &db::state::Database,
    record: &TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
) -> Result<ReceiptBoundary, ActiveReblitBootSyncStartedRecoveryAuthorityError> {
    match state_db.load_exact_promoted_boot_publication_receipt_chain(
        &record.transition_id,
        &receipt_pair,
    ) {
        Ok(chain) => {
            require_exact_cleanup_plan(&chain, receipt_pair)?;
            Ok(ReceiptBoundary::Promoted)
        }
        Err(
            db::state::ExactPromotedBootPublicationReceiptStateError::PendingHeadPresent {
                ..
            },
        ) => {
            let pending = state_db
                .boot_publication_receipt_state()
                .map_err(
                    ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::PendingReceiptState,
                )?;
            if pending.receipt_pair_for(&record.transition_id) == Some(receipt_pair) {
                Ok(ReceiptBoundary::Pending)
            } else {
                Err(
                    ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::PendingReceiptCorrelationMismatch
                        .into(),
                )
            }
        }
        Err(source) => Err(
            ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::ReceiptCorrelation(source)
                .into(),
        ),
    }
}

fn inspect_current_database(
    record: &TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
    state_db: &db::state::Database,
) -> Result<ActiveReblitBootSyncStartedDatabaseInspection, ActiveReblitBootSyncStartedRecoveryAuthorityError> {
    let receipt_before = load_exact_promoted_chain(state_db, record, receipt_pair)?;
    let plan_before = require_exact_cleanup_plan(&receipt_before, receipt_pair)?;
    let in_flight = state_db
        .audit_in_flight_transition()
        .map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    if !existing_state_context_is_exact(record, &context) {
        return Ok(ActiveReblitBootSyncStartedDatabaseInspection::Incompatible);
    }
    let state_id = state::Id::from(
        record
            .candidate
            .id
            .expect("checked exact ActiveReblit state ID"),
    );
    let state = match state_db.get(state_id) {
        Ok(state) => state,
        Err(db::Error::RowNotFound) => {
            return Err(
                ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::DatabaseEvidenceChanged
                    .into(),
            );
        }
        Err(source) => {
            return Err(
                ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::StateDatabase(source)
                    .into(),
            );
        }
    };
    if state.id != state_id {
        return Err(
            ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::DatabaseEvidenceChanged
                .into(),
        );
    }
    let receipt_after = load_exact_promoted_chain(state_db, record, receipt_pair)?;
    let plan_after = require_exact_cleanup_plan(&receipt_after, receipt_pair)?;
    let receipt_and_plan_match = receipt_before == receipt_after
        && plan_before == plan_after;
    drop(plan_before);
    drop(plan_after);
    if !receipt_and_plan_match {
        return Err(
            ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::DatabaseEvidenceChanged
                .into(),
        );
    }
    Ok(ActiveReblitBootSyncStartedDatabaseInspection::Exact(
        ActiveReblitBootSyncStartedDatabaseEvidence {
            receipt_chain: receipt_after,
            context,
            state,
        },
    ))
}

fn load_exact_promoted_chain(
    state_db: &db::state::Database,
    record: &TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
) -> Result<db::state::ExactPromotedBootPublicationReceiptChain, ActiveReblitBootSyncStartedRecoveryAuthorityError> {
    state_db
        .load_exact_promoted_boot_publication_receipt_chain(
            &record.transition_id,
            &receipt_pair,
        )
        .map_err(|source| {
            ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::ReceiptCorrelation(source)
                .into()
        })
}

fn require_exact_cleanup_plan<'chain>(
    chain: &'chain db::state::ExactPromotedBootPublicationReceiptChain,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
) -> Result<ActiveReblitPromotedBootCleanupPlan<'chain>, ActiveReblitBootSyncStartedRecoveryAuthorityError> {
    let plan = chain.prepare_active_reblit_promoted_boot_cleanup_plan()?;
    if plan.promoted_receipt() == receipt_pair.pending {
        Ok(plan)
    } else {
        Err(
            ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::CleanupPlanReceiptMismatch
                .into(),
        )
    }
}

fn existing_state_context_is_exact(
    record: &TransitionRecord,
    evidence: &DatabaseEvidence,
) -> bool {
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
    expected: &ActiveReblitBootSyncStartedDatabaseEvidence,
    actual: ActiveReblitBootSyncStartedDatabaseInspection,
) -> Result<ActiveReblitBootSyncStartedDatabaseEvidence, ActiveReblitBootSyncStartedRecoveryAuthorityError> {
    match actual {
        ActiveReblitBootSyncStartedDatabaseInspection::Exact(actual)
            if actual == *expected =>
        {
            Ok(actual)
        }
        ActiveReblitBootSyncStartedDatabaseInspection::Exact(_)
        | ActiveReblitBootSyncStartedDatabaseInspection::Incompatible => Err(
            ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::DatabaseEvidenceChanged
                .into(),
        ),
    }
}

fn capture_exact_active_state(
    record: &TransitionRecord,
    installation: &Installation,
    reservation: &ActiveStateReservation,
) -> Result<Option<ActiveStateSnapshot>, ActiveReblitBootSyncStartedRecoveryAuthorityError> {
    let active_state = reservation
        .capture_for_startup_recovery(installation)
        .map_err(ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::ActiveState)?;
    let expected = state::Id::from(
        record
            .candidate
            .id
            .expect("checked exact ActiveReblit state ID"),
    );
    if active_state.active() != Some(expected) {
        return Ok(None);
    }
    active_state
        .revalidate(installation)
        .map_err(ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::ActiveState)?;
    Ok(Some(active_state))
}

fn require_exact_active_state(
    record: &TransitionRecord,
    installation: &Installation,
    active_state: &ActiveStateSnapshot,
) -> Result<(), ActiveReblitBootSyncStartedRecoveryAuthorityError> {
    let expected = state::Id::from(
        record
            .candidate
            .id
            .expect("validated ActiveReblit state ID"),
    );
    let actual = active_state.active();
    if actual != Some(expected) {
        return Err(
            ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::ActiveSelectionMismatch {
                expected,
                actual,
            }
            .into(),
        );
    }
    active_state
        .revalidate(installation)
        .map_err(ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::ActiveState)?;
    Ok(())
}

fn record_plan_is_exact(
    record: &TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
    cleanup_seal: &ActiveReblitBootSyncStartedCleanupSeal,
) -> bool {
    record.operation == Operation::ActiveReblit
        && record.phase == Phase::BootSyncStarted
        && record.rollback.is_none()
        && record.options.run_boot_sync
        && record.candidate.id.is_some()
        && record.candidate.id == record.previous.id
        && record.boot_publication_receipts == Some(receipt_pair)
        && cleanup_seal.promoted_receipt() == receipt_pair.pending
}

fn require_exact_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), ActiveReblitBootSyncStartedRecoveryAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(
            ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::JournalRecordBindingChanged
                .into(),
        );
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, binding, record)? {
        Ok(())
    } else {
        Err(
            ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::JournalRecordBindingChanged
                .into(),
        )
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct ActiveReblitBootSyncStartedRecoveryAuthorityError(
    #[from] ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind,
);

impl From<InspectionError> for ActiveReblitBootSyncStartedRecoveryAuthorityError {
    fn from(source: InspectionError) -> Self {
        ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<ActiveReblitBootSyncStartedNamespaceError>
    for ActiveReblitBootSyncStartedRecoveryAuthorityError
{
    fn from(source: ActiveReblitBootSyncStartedNamespaceError) -> Self {
        ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<ActiveReblitPromotedBootCleanupPlanError>
    for ActiveReblitBootSyncStartedRecoveryAuthorityError
{
    fn from(source: ActiveReblitPromotedBootCleanupPlanError) -> Self {
        ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::CleanupPlan(source).into()
    }
}

impl From<crate::installation::Error>
    for ActiveReblitBootSyncStartedRecoveryAuthorityError
{
    fn from(source: crate::installation::Error) -> Self {
        ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for ActiveReblitBootSyncStartedRecoveryAuthorityError {
    fn from(source: StorageError) -> Self {
        ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum ActiveReblitBootSyncStartedRecoveryAuthorityErrorKind {
    #[error("decode and validate the exact ActiveReblit BootSyncStarted journal record")]
    Record(#[source] CodecError),
    #[error("the startup cleanup seal does not name the record's promoted receipt")]
    CleanupSealReceiptMismatch,
    #[error("the exact ActiveReblit BootSyncStarted journal-record binding changed")]
    JournalRecordBindingChanged,
    #[error("read or revalidate the exact bound ActiveReblit transition journal")]
    Journal(#[source] StorageError),
    #[error("strictly load the pending boot-publication receipt state")]
    PendingReceiptState(#[source] db::state::BootPublicationReceiptStateError),
    #[error("the pending boot-publication receipt does not match the exact journal pair")]
    PendingReceiptCorrelationMismatch,
    #[error("the boot-publication receipt chain conflicts with the exact journal pair")]
    ReceiptCorrelation(
        #[source] db::state::ExactPromotedBootPublicationReceiptStateError,
    ),
    #[error("derive the exact promoted boot-publication cleanup plan")]
    CleanupPlan(#[source] ActiveReblitPromotedBootCleanupPlanError),
    #[error("the cleanup plan does not name the exact promoted receipt")]
    CleanupPlanReceiptMismatch,
    #[error("inspect exact cleared ActiveReblit state and metadata provenance")]
    Inspection(#[source] InspectionError),
    #[error("load the complete ActiveReblit target state")]
    StateDatabase(#[source] db::Error),
    #[error("exact ActiveReblit BootSyncStarted database evidence changed")]
    DatabaseEvidenceChanged,
    #[error("prove exact live active-state selection for ActiveReblit restart cleanup")]
    ActiveState(#[source] super::super::Error),
    #[error("ActiveReblit restart cleanup requires active state {expected}, found {actual:?}")]
    ActiveSelectionMismatch {
        expected: state::Id,
        actual: Option<state::Id>,
    },
    #[error("revalidate exact ActiveReblit BootSyncStarted namespace evidence")]
    Namespace(#[source] ActiveReblitBootSyncStartedNamespaceError),
    #[error("exact ActiveReblit BootSyncStarted recovery evidence changed")]
    RouteEvidenceChanged,
    #[error("revalidate retained mutable installation namespace")]
    Installation(#[source] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_active_reblit_boot_sync_started_database_captures(
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
