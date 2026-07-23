//! Exact read-only authority for forward ActiveReblit commit cleanup.
//!
//! Admission is restricted to one of two disjoint v3 `CommitDecided` routes:
//! promoted-boot cleanup or the exact system-triggered no-boot cleanup. It
//! retains the complete state and provenance, active selection and
//! reservation, exact journal-record inode, and a descriptor-backed Apply or
//! Finish namespace proof. Admission remains read-only; the specialized child
//! owns the exact namespace effect and durability suffix, then the sole bound
//! `CommitCleanupComplete` persistence edge.

mod effect;

use crate::{
    Installation, State, db, state,
    transition_journal::{
        CodecError, Operation, Phase, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_reblit_boot_publication_preflight::ActiveReblitCommitCleanupSeal,
    active_state_snapshot::{ActiveStateReservation, ActiveStateSnapshot},
};
use super::{
    DatabaseEvidence, InspectionError, database_ownership_evidence_compatible, inspect_database,
    metadata_provenance_evidence_compatible,
};
use super::activation_namespace::{
    ActiveReblitCommitCleanupApplyNamespaceEffectEvidence,
    ActiveReblitCommitCleanupApplyNamespaceProof,
    ActiveReblitCommitCleanupFinishNamespaceEffectEvidence,
    ActiveReblitCommitCleanupFinishNamespaceProof, ActiveReblitCommitCleanupNamespaceError,
    ActiveReblitCommitCleanupNamespaceInspection, ActiveReblitCommitCleanupNamespaceProof,
    active_reblit_commit_cleanup_namespace_error_is_mismatch,
};

pub(in crate::client) use effect::{
    ActiveReblitCommitCleanupApplyReconciliation, ActiveReblitCommitCleanupDurableAuthority,
    ActiveReblitCommitCleanupEffectError, ActiveReblitCommitCleanupPendingDurabilityAuthority,
    ActiveReblitCommitCleanupPostAdvanceAuthority, ActiveReblitCommitCleanupRecordAdvanceError,
};

/// Layout-specific result of exact read-only cleanup admission.
pub(in crate::client) enum ActiveReblitCommitCleanupAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Apply(ActiveReblitCommitCleanupApplyAuthority<'reservation>),
    Finish(ActiveReblitCommitCleanupFinishAuthority<'reservation>),
}

/// Entry point for exact read-only cleanup admission.
pub(in crate::client) struct ActiveReblitCommitCleanupAuthority;

/// Exact Apply authority. It intentionally implements neither `Clone` nor
/// `Copy` and can be projected only by consuming it.
pub(in crate::client) struct ActiveReblitCommitCleanupApplyAuthority<'reservation> {
    evidence: ActiveReblitCommitCleanupCommonEvidence<'reservation>,
    namespace: ActiveReblitCommitCleanupApplyNamespaceProof,
}

/// Exact Finish authority. It intentionally implements neither `Clone` nor
/// `Copy` and can be projected only by consuming it.
pub(in crate::client) struct ActiveReblitCommitCleanupFinishAuthority<'reservation> {
    evidence: ActiveReblitCommitCleanupCommonEvidence<'reservation>,
    namespace: ActiveReblitCommitCleanupFinishNamespaceProof,
}

/// Narrow consuming projection accepted only by the specialized Apply effect
/// child.
pub(in crate::client) struct ActiveReblitCommitCleanupApplyEffectAuthority<'reservation> {
    _evidence: ActiveReblitCommitCleanupCommonEvidence<'reservation>,
    _namespace: ActiveReblitCommitCleanupApplyNamespaceEffectEvidence,
}

/// Narrow consuming projection accepted only by the specialized zero-exchange
/// Finish durability child.
pub(in crate::client) struct ActiveReblitCommitCleanupFinishEffectAuthority<'reservation> {
    _evidence: ActiveReblitCommitCleanupCommonEvidence<'reservation>,
    _namespace: ActiveReblitCommitCleanupFinishNamespaceEffectEvidence,
}

/// Common exact evidence retained behind the disjoint layout typestates.
struct ActiveReblitCommitCleanupCommonEvidence<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: ActiveReblitCommitCleanupDatabaseEvidence,
    active_state: ActiveStateSnapshot,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Disjoint cleanup route evidence. The no-boot route retains the current
/// installed chain only as inert evidence: it grants no receipt mutation or
/// correlation authority for this transition.
#[derive(Debug, Eq, PartialEq)]
enum ActiveReblitCommitCleanupRouteEvidence {
    PromotedBoot {
        pair: crate::boot_publication::BootPublicationReceiptPair,
        receipt: db::state::BootPublicationReceiptState,
    },
    NoBoot {
        inert_receipt_chain: db::state::CurrentExactPromotedBootPublicationReceiptChain,
    },
}

/// Route-specific evidence, existing-state context, and complete state from
/// one stable database sandwich. This evidence is intentionally not `Clone`.
#[derive(Debug, Eq, PartialEq)]
struct ActiveReblitCommitCleanupDatabaseEvidence {
    route: ActiveReblitCommitCleanupRouteEvidence,
    context: DatabaseEvidence,
    state: State,
}

enum ActiveReblitCommitCleanupRoutePlan {
    PromotedBoot(crate::boot_publication::BootPublicationReceiptPair),
    NoBoot,
}

enum ActiveReblitCommitCleanupDatabaseInspection {
    Exact(ActiveReblitCommitCleanupDatabaseEvidence),
    Incompatible,
}

impl ActiveReblitCommitCleanupAuthority {
    /// Capture exact Apply or Finish evidence without performing any effect.
    pub(in crate::client) fn capture<'reservation>(
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
    ) -> Result<
        ActiveReblitCommitCleanupAdmission<'reservation>,
        ActiveReblitCommitCleanupAuthorityError,
    > {
        Self::capture_with_record_binding(
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
        )
    }

    /// Admit only the live generation-13 promoted-boot Apply layout while
    /// consuming the journal binding retained by commit-decision coordination.
    /// Finish is deliberately restart-only and can enter only through
    /// [`Self::capture`].
    pub(in crate::client) fn capture_retained_binding<'reservation>(
        _cleanup_seal: ActiveReblitCommitCleanupSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        journal_record_binding: TransitionJournalRecordBinding,
    ) -> Result<
        ActiveReblitCommitCleanupApplyAuthority<'reservation>,
        ActiveReblitCommitCleanupAuthorityError,
    > {
        let receipt_pair = record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitCommitCleanupAuthorityErrorKind::Record)?;
        if record.operation != Operation::ActiveReblit
            || record.phase != Phase::CommitDecided
            || record.generation != 13
            || record.rollback.is_some()
            || record.options.archive_previous
            || !record.options.run_system_triggers
            || !record.options.run_boot_sync
            || receipt_pair.is_none()
            || !same_nonempty_candidate_and_previous(record)
        {
            return Err(
                ActiveReblitCommitCleanupAuthorityErrorKind::RetainedCommitDecisionRejected.into(),
            );
        }

        match Self::capture_with_record_binding(
            installation,
            journal,
            state_db,
            active_state_reservation,
            record,
            || Ok(journal_record_binding),
        )? {
            ActiveReblitCommitCleanupAdmission::Apply(authority) => Ok(authority),
            ActiveReblitCommitCleanupAdmission::NotApplicable
            | ActiveReblitCommitCleanupAdmission::Deferred
            | ActiveReblitCommitCleanupAdmission::Finish(_) => Err(
                ActiveReblitCommitCleanupAuthorityErrorKind::RetainedCommitDecisionRejected.into(),
            ),
        }
    }

    fn capture_with_record_binding<'reservation>(
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        capture_binding: impl FnOnce() -> Result<
            TransitionJournalRecordBinding,
            ActiveReblitCommitCleanupAuthorityError,
        >,
    ) -> Result<
        ActiveReblitCommitCleanupAdmission<'reservation>,
        ActiveReblitCommitCleanupAuthorityError,
    > {
        if record.operation != Operation::ActiveReblit || record.phase != Phase::CommitDecided {
            return Ok(ActiveReblitCommitCleanupAdmission::NotApplicable);
        }
        let receipt_correlation = record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitCommitCleanupAuthorityErrorKind::Record)?;
        let Some(route_plan) = exact_route_plan(record, receipt_correlation) else {
            return Ok(ActiveReblitCommitCleanupAdmission::Deferred);
        };

        let journal_record_binding = capture_binding()?;
        require_exact_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;

        let database_before = match inspect_current_database_for_plan(record, &route_plan, state_db)? {
            ActiveReblitCommitCleanupDatabaseInspection::Exact(database) => database,
            ActiveReblitCommitCleanupDatabaseInspection::Incompatible => {
                return Ok(ActiveReblitCommitCleanupAdmission::Deferred);
            }
        };
        let active_state = match capture_exact_active_state(
            record,
            installation,
            active_state_reservation,
        )? {
            Some(active_state) => active_state,
            None => return Ok(ActiveReblitCommitCleanupAdmission::Deferred),
        };
        let namespace_inspection = match ActiveReblitCommitCleanupNamespaceInspection::begin(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(inspection) => inspection,
            Err(source) if active_reblit_commit_cleanup_namespace_error_is_mismatch(&source) => {
                return Ok(ActiveReblitCommitCleanupAdmission::Deferred);
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
            inspect_current_database(record, &database_before.route, state_db)?,
        )?;
        if database_before != database_after {
            return Err(ActiveReblitCommitCleanupAuthorityErrorKind::DatabaseEvidenceChanged.into());
        }
        if !record_plan_is_exact(record, &database_after.route) {
            return Err(ActiveReblitCommitCleanupAuthorityErrorKind::RouteEvidenceChanged.into());
        }
        if !revalidate_active_state_for_admission(record, installation, &active_state)? {
            return Err(ActiveReblitCommitCleanupAuthorityErrorKind::ActiveSelectionChanged.into());
        }
        require_exact_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        let evidence = ActiveReblitCommitCleanupCommonEvidence {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database: database_after,
            active_state,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        };
        Ok(match namespace {
            ActiveReblitCommitCleanupNamespaceProof::Apply(namespace) => {
                ActiveReblitCommitCleanupAdmission::Apply(
                    ActiveReblitCommitCleanupApplyAuthority { evidence, namespace },
                )
            }
            ActiveReblitCommitCleanupNamespaceProof::Finish(namespace) => {
                ActiveReblitCommitCleanupAdmission::Finish(
                    ActiveReblitCommitCleanupFinishAuthority { evidence, namespace },
                )
            }
        })
    }
}

impl<'reservation> ActiveReblitCommitCleanupApplyAuthority<'reservation> {
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), ActiveReblitCommitCleanupAuthorityError> {
        revalidate_apply(&self.evidence, &self.namespace, journal)
    }

    /// Consume exact Apply admission into the only projection accepted by the
    /// specialized effect child.
    pub(in crate::client) fn into_effect_authority(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        ActiveReblitCommitCleanupApplyEffectAuthority<'reservation>,
        ActiveReblitCommitCleanupAuthorityError,
    > {
        self.revalidate(journal)?;
        Ok(ActiveReblitCommitCleanupApplyEffectAuthority {
            _evidence: self.evidence,
            _namespace: self.namespace.into_effect_evidence(),
        })
    }
}

impl<'reservation> ActiveReblitCommitCleanupFinishAuthority<'reservation> {
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), ActiveReblitCommitCleanupAuthorityError> {
        revalidate_finish(&self.evidence, &self.namespace, journal)
    }

    /// Consume exact Finish admission into its zero-exchange durability
    /// projection.
    pub(in crate::client) fn into_effect_authority(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        ActiveReblitCommitCleanupFinishEffectAuthority<'reservation>,
        ActiveReblitCommitCleanupAuthorityError,
    > {
        self.revalidate(journal)?;
        Ok(ActiveReblitCommitCleanupFinishEffectAuthority {
            _evidence: self.evidence,
            _namespace: self.namespace.into_effect_evidence(),
        })
    }
}

fn revalidate_apply(
    evidence: &ActiveReblitCommitCleanupCommonEvidence<'_>,
    namespace: &ActiveReblitCommitCleanupApplyNamespaceProof,
    journal: &TransitionJournalStore,
) -> Result<(), ActiveReblitCommitCleanupAuthorityError> {
    require_exact_record_binding(
        &evidence.installation,
        journal,
        &evidence.journal_record_binding,
        &evidence.record,
    )?;
    evidence.installation.revalidate_mutable_namespace()?;
    let database_before = require_exact_database(
        &evidence.database,
        inspect_current_database(&evidence.record, &evidence.database.route, &evidence.state_db)?,
    )?;
    require_exact_active_state(&evidence.record, &evidence.installation, &evidence.active_state)?;
    namespace.revalidate(
        &evidence.installation,
        journal,
        &evidence.journal_record_binding,
        &evidence.record,
    )?;
    finish_common_revalidation(evidence, journal, database_before)
}

fn revalidate_finish(
    evidence: &ActiveReblitCommitCleanupCommonEvidence<'_>,
    namespace: &ActiveReblitCommitCleanupFinishNamespaceProof,
    journal: &TransitionJournalStore,
) -> Result<(), ActiveReblitCommitCleanupAuthorityError> {
    require_exact_record_binding(
        &evidence.installation,
        journal,
        &evidence.journal_record_binding,
        &evidence.record,
    )?;
    evidence.installation.revalidate_mutable_namespace()?;
    let database_before = require_exact_database(
        &evidence.database,
        inspect_current_database(&evidence.record, &evidence.database.route, &evidence.state_db)?,
    )?;
    require_exact_active_state(&evidence.record, &evidence.installation, &evidence.active_state)?;
    namespace.revalidate(
        &evidence.installation,
        journal,
        &evidence.journal_record_binding,
        &evidence.record,
    )?;
    finish_common_revalidation(evidence, journal, database_before)
}

fn finish_common_revalidation(
    evidence: &ActiveReblitCommitCleanupCommonEvidence<'_>,
    journal: &TransitionJournalStore,
    database_before: ActiveReblitCommitCleanupDatabaseEvidence,
) -> Result<(), ActiveReblitCommitCleanupAuthorityError> {
    let database_after = require_exact_database(
        &evidence.database,
        inspect_current_database(&evidence.record, &evidence.database.route, &evidence.state_db)?,
    )?;
    require_exact_active_state(&evidence.record, &evidence.installation, &evidence.active_state)?;
    if database_before != database_after
        || !record_plan_is_exact(&evidence.record, &evidence.database.route)
    {
        return Err(ActiveReblitCommitCleanupAuthorityErrorKind::RouteEvidenceChanged.into());
    }
    require_exact_record_binding(
        &evidence.installation,
        journal,
        &evidence.journal_record_binding,
        &evidence.record,
    )?;
    evidence.installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn inspect_current_database(
    record: &TransitionRecord,
    route: &ActiveReblitCommitCleanupRouteEvidence,
    state_db: &db::state::Database,
) -> Result<
    ActiveReblitCommitCleanupDatabaseInspection,
    ActiveReblitCommitCleanupAuthorityError,
> {
    match route {
        ActiveReblitCommitCleanupRouteEvidence::PromotedBoot { pair, .. } => {
            inspect_promoted_boot_database(record, *pair, state_db)
        }
        ActiveReblitCommitCleanupRouteEvidence::NoBoot { .. } => {
            inspect_no_boot_database(record, state_db)
        }
    }
}

fn inspect_current_database_for_plan(
    record: &TransitionRecord,
    route: &ActiveReblitCommitCleanupRoutePlan,
    state_db: &db::state::Database,
) -> Result<
    ActiveReblitCommitCleanupDatabaseInspection,
    ActiveReblitCommitCleanupAuthorityError,
> {
    match route {
        ActiveReblitCommitCleanupRoutePlan::PromotedBoot(pair) => {
            inspect_promoted_boot_database(record, *pair, state_db)
        }
        ActiveReblitCommitCleanupRoutePlan::NoBoot => inspect_no_boot_database(record, state_db),
    }
}

fn inspect_promoted_boot_database(
    record: &TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
    state_db: &db::state::Database,
) -> Result<
    ActiveReblitCommitCleanupDatabaseInspection,
    ActiveReblitCommitCleanupAuthorityError,
> {
    let receipt_before = match load_exact_promoted_receipt(state_db, record, receipt_pair)? {
        Some(receipt) => receipt,
        None => return Ok(ActiveReblitCommitCleanupDatabaseInspection::Incompatible),
    };
    let Some((context, state)) = inspect_context_and_state(record, state_db)? else {
        return Ok(ActiveReblitCommitCleanupDatabaseInspection::Incompatible);
    };
    let receipt_after = match load_exact_promoted_receipt(state_db, record, receipt_pair)? {
        Some(receipt) => receipt,
        None => {
            return Err(ActiveReblitCommitCleanupAuthorityErrorKind::DatabaseEvidenceChanged.into());
        }
    };
    if receipt_before != receipt_after {
        return Err(ActiveReblitCommitCleanupAuthorityErrorKind::DatabaseEvidenceChanged.into());
    }
    Ok(ActiveReblitCommitCleanupDatabaseInspection::Exact(
        ActiveReblitCommitCleanupDatabaseEvidence {
            route: ActiveReblitCommitCleanupRouteEvidence::PromotedBoot {
                pair: receipt_pair,
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
    ActiveReblitCommitCleanupDatabaseInspection,
    ActiveReblitCommitCleanupAuthorityError,
> {
    let receipt_chain_before = load_inert_no_boot_receipt_chain(record, state_db)?;
    let Some((context, state)) = inspect_context_and_state(record, state_db)? else {
        return Ok(ActiveReblitCommitCleanupDatabaseInspection::Incompatible);
    };
    let receipt_chain_after = load_inert_no_boot_receipt_chain(record, state_db)?;
    if receipt_chain_before != receipt_chain_after {
        return Err(ActiveReblitCommitCleanupAuthorityErrorKind::DatabaseEvidenceChanged.into());
    }
    Ok(ActiveReblitCommitCleanupDatabaseInspection::Exact(
        ActiveReblitCommitCleanupDatabaseEvidence {
            route: ActiveReblitCommitCleanupRouteEvidence::NoBoot {
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
    ActiveReblitCommitCleanupAuthorityError,
> {
    let chain = state_db
        .load_current_exact_promoted_boot_publication_receipt_chain()
        .map_err(ActiveReblitCommitCleanupAuthorityErrorKind::ReceiptChain)?;
    if matches!(
        &chain,
        db::state::CurrentExactPromotedBootPublicationReceiptChain::Installed(installed)
            if installed.installed_receipt().body().transition_id() == &record.transition_id
    ) {
        return Err(ActiveReblitCommitCleanupAuthorityErrorKind::ReceiptChainMatchesNoBootTransition.into());
    }
    Ok(chain)
}

fn inspect_context_and_state(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<Option<(DatabaseEvidence, State)>, ActiveReblitCommitCleanupAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    if !existing_state_context_is_exact(record, &context) {
        return Ok(None);
    }
    let state_id = state::Id::from(record.candidate.id.expect("checked exact ActiveReblit state ID"));
    let state = match state_db.get(state_id) {
        Ok(state) => state,
        Err(db::Error::RowNotFound) => {
            return Err(ActiveReblitCommitCleanupAuthorityErrorKind::DatabaseEvidenceChanged.into());
        }
        Err(source) => {
            return Err(ActiveReblitCommitCleanupAuthorityErrorKind::StateDatabase(source).into());
        }
    };
    if state.id != state_id {
        return Err(ActiveReblitCommitCleanupAuthorityErrorKind::DatabaseEvidenceChanged.into());
    }
    Ok(Some((context, state)))
}

fn load_exact_promoted_receipt(
    state_db: &db::state::Database,
    record: &TransitionRecord,
    receipt_pair: crate::boot_publication::BootPublicationReceiptPair,
) -> Result<Option<db::state::BootPublicationReceiptState>, ActiveReblitCommitCleanupAuthorityError> {
    match state_db.load_exact_promoted_boot_publication_receipt_state(&record.transition_id, &receipt_pair) {
        Ok(receipt) => Ok(Some(receipt)),
        Err(db::state::ExactPromotedBootPublicationReceiptStateError::State(source)) => {
            Err(ActiveReblitCommitCleanupAuthorityErrorKind::ReceiptState(source).into())
        }
        Err(source @ db::state::ExactPromotedBootPublicationReceiptStateError::PendingBodyPresent)
        | Err(source @ db::state::ExactPromotedBootPublicationReceiptStateError::MissingCommittedBody)
        | Err(
            source @ db::state::ExactPromotedBootPublicationReceiptStateError::CommittedBodyFingerprintMismatch {
                ..
            },
        ) => Err(ActiveReblitCommitCleanupAuthorityErrorKind::ReceiptCorrelation(source).into()),
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
    expected: &ActiveReblitCommitCleanupDatabaseEvidence,
    actual: ActiveReblitCommitCleanupDatabaseInspection,
) -> Result<ActiveReblitCommitCleanupDatabaseEvidence, ActiveReblitCommitCleanupAuthorityError> {
    match actual {
        ActiveReblitCommitCleanupDatabaseInspection::Exact(actual) if actual == *expected => Ok(actual),
        ActiveReblitCommitCleanupDatabaseInspection::Exact(_)
        | ActiveReblitCommitCleanupDatabaseInspection::Incompatible => {
            Err(ActiveReblitCommitCleanupAuthorityErrorKind::DatabaseEvidenceChanged.into())
        }
    }
}

fn capture_exact_active_state(
    record: &TransitionRecord,
    installation: &Installation,
    reservation: &ActiveStateReservation,
) -> Result<Option<ActiveStateSnapshot>, ActiveReblitCommitCleanupAuthorityError> {
    let active_state = reservation
        .capture_for_startup_recovery(installation)
        .map_err(ActiveReblitCommitCleanupAuthorityErrorKind::ActiveState)?;
    let expected = state::Id::from(record.candidate.id.expect("checked exact ActiveReblit state ID"));
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
) -> Result<bool, ActiveReblitCommitCleanupAuthorityError> {
    let expected = state::Id::from(record.candidate.id.expect("checked exact ActiveReblit state ID"));
    if active_state.active() != Some(expected) {
        return Ok(false);
    }
    active_state
        .revalidate(installation)
        .map_err(ActiveReblitCommitCleanupAuthorityErrorKind::ActiveState)?;
    Ok(true)
}

fn require_exact_active_state(
    record: &TransitionRecord,
    installation: &Installation,
    active_state: &ActiveStateSnapshot,
) -> Result<(), ActiveReblitCommitCleanupAuthorityError> {
    let expected = state::Id::from(record.candidate.id.expect("retained exact ActiveReblit state ID"));
    let actual = active_state.active();
    if actual != Some(expected) {
        return Err(
            ActiveReblitCommitCleanupAuthorityErrorKind::ActiveSelectionMismatch { expected, actual }.into(),
        );
    }
    active_state
        .revalidate(installation)
        .map_err(ActiveReblitCommitCleanupAuthorityErrorKind::ActiveState)?;
    Ok(())
}

fn same_nonempty_candidate_and_previous(record: &TransitionRecord) -> bool {
    record.candidate.id.is_some() && record.candidate.id == record.previous.id
}

fn exact_route_plan(
    record: &TransitionRecord,
    receipt_correlation: Option<crate::boot_publication::BootPublicationReceiptPair>,
) -> Option<ActiveReblitCommitCleanupRoutePlan> {
    if record.operation != Operation::ActiveReblit
        || record.phase != Phase::CommitDecided
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
        (_, true, Some(pair)) => Some(ActiveReblitCommitCleanupRoutePlan::PromotedBoot(pair)),
        (11, false, None)
            if !record.options.archive_previous && record.options.run_system_triggers =>
        {
            Some(ActiveReblitCommitCleanupRoutePlan::NoBoot)
        }
        _ => None,
    }
}

fn record_plan_is_exact(
    record: &TransitionRecord,
    route: &ActiveReblitCommitCleanupRouteEvidence,
) -> bool {
    let common = record.operation == Operation::ActiveReblit
        && record.phase == Phase::CommitDecided
        && record.rollback.is_none()
        && same_nonempty_candidate_and_previous(record);
    common
        && match route {
            ActiveReblitCommitCleanupRouteEvidence::PromotedBoot { pair, .. } => {
                record.options.run_boot_sync && record.boot_publication_receipts == Some(*pair)
            }
            ActiveReblitCommitCleanupRouteEvidence::NoBoot { .. } => {
                record.generation == 11
                    && !record.options.archive_previous
                    && record.options.run_system_triggers
                    && !record.options.run_boot_sync
                    && record.boot_publication_receipts.is_none()
            }
        }
}

fn require_exact_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), ActiveReblitCommitCleanupAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(ActiveReblitCommitCleanupAuthorityErrorKind::JournalRecordBindingChanged.into());
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, binding, record)? {
        Ok(())
    } else {
        Err(ActiveReblitCommitCleanupAuthorityErrorKind::JournalRecordBindingChanged.into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct ActiveReblitCommitCleanupAuthorityError(
    #[from] ActiveReblitCommitCleanupAuthorityErrorKind,
);

impl From<InspectionError> for ActiveReblitCommitCleanupAuthorityError {
    fn from(source: InspectionError) -> Self {
        ActiveReblitCommitCleanupAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<ActiveReblitCommitCleanupNamespaceError> for ActiveReblitCommitCleanupAuthorityError {
    fn from(source: ActiveReblitCommitCleanupNamespaceError) -> Self {
        ActiveReblitCommitCleanupAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for ActiveReblitCommitCleanupAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        ActiveReblitCommitCleanupAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for ActiveReblitCommitCleanupAuthorityError {
    fn from(source: StorageError) -> Self {
        ActiveReblitCommitCleanupAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum ActiveReblitCommitCleanupAuthorityErrorKind {
    #[error("decode and validate the exact v3 ActiveReblit CommitDecided journal record")]
    Record(#[source] CodecError),
    #[error("the exact ActiveReblit CommitDecided journal-record binding changed")]
    JournalRecordBindingChanged,
    #[error("the retained live CommitDecided handoff is not the exact generation-13 promoted-boot Apply route")]
    RetainedCommitDecisionRejected,
    #[error("the exact ActiveReblit CommitCleanupComplete successor binding changed")]
    SuccessorRecordBindingChanged,
    #[error("read or revalidate the exact bound ActiveReblit CommitDecided journal")]
    Journal(#[source] StorageError),
    #[error("strictly load canonical promoted boot-publication receipt state")]
    ReceiptState(#[source] db::state::BootPublicationReceiptStateError),
    #[error("promoted boot-publication receipt state is internally inconsistent")]
    ReceiptCorrelation(#[source] db::state::ExactPromotedBootPublicationReceiptStateError),
    #[error("strictly load inert installed boot-publication receipt state for no-boot cleanup")]
    ReceiptChain(#[source] db::state::CurrentExactPromotedBootPublicationReceiptChainError),
    #[error("the installed boot-publication receipt belongs to the no-boot transition")]
    ReceiptChainMatchesNoBootTransition,
    #[error("inspect exact cleared ActiveReblit state and metadata provenance")]
    Inspection(#[source] InspectionError),
    #[error("load the complete ActiveReblit selected state")]
    StateDatabase(#[source] db::Error),
    #[error("exact ActiveReblit cleanup database evidence changed")]
    DatabaseEvidenceChanged,
    #[error("prove exact live active-state selection for ActiveReblit cleanup")]
    ActiveState(#[source] super::super::Error),
    #[error("ActiveReblit cleanup requires active state {expected}, found {actual:?}")]
    ActiveSelectionMismatch {
        expected: state::Id,
        actual: Option<state::Id>,
    },
    #[error("revalidate exact descriptor-backed ActiveReblit cleanup namespace evidence")]
    Namespace(#[source] ActiveReblitCommitCleanupNamespaceError),
    #[error("exact ActiveReblit CommitDecided route evidence changed")]
    RouteEvidenceChanged,
    #[error("the ActiveReblit cleanup active selection changed during admission")]
    ActiveSelectionChanged,
    #[error("the retained record is not the exact ActiveReblit CommitCleanupComplete successor")]
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
pub(in crate::client) fn arm_between_active_reblit_commit_cleanup_database_captures(
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
