//! Shared non-effect evidence for the ActiveReblit boot-repair suffix.
//!
//! Required, Started, and Complete authorities remain phase-specific. This
//! module only centralizes their identical exact database, complete
//! target-state, active selection, and rollback-plan predicates so those
//! checks cannot drift.

use crate::{
    State, db, state,
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, TransitionRecord,
    },
};

use super::super::active_state_snapshot::{ActiveStateReservation, ActiveStateSnapshot};
use super::{
    DatabaseEvidence, InspectionError, database_ownership_evidence_compatible, inspect_database,
    metadata_provenance_evidence_compatible,
};

/// Exact source-database evidence plus the complete state consumed by boot
/// projection. The complete state detects changes to selections and descriptive
/// fields which ownership-only evidence cannot observe.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct ActiveReblitBootRepairDatabaseEvidence {
    context: DatabaseEvidence,
    target: State,
}

pub(super) enum ActiveReblitBootRepairDatabaseInspection {
    Exact(ActiveReblitBootRepairDatabaseEvidence),
    Incompatible(DatabaseEvidence),
}

pub(super) fn inspect_active_reblit_boot_repair_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<ActiveReblitBootRepairDatabaseInspection, ActiveReblitBootRepairEvidenceError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let context = inspect_database(record, state_db, in_flight)?;
    if !active_reblit_boot_repair_database_context_is_exact(record, &context) {
        return Ok(ActiveReblitBootRepairDatabaseInspection::Incompatible(context));
    }

    let target_id = state::Id::from(
        record
            .previous
            .id
            .expect("validated ActiveReblit boot-repair record has a previous state ID"),
    );
    let target = state_db
        .get(target_id)
        .map_err(ActiveReblitBootRepairEvidenceError::StateDatabase)?;
    if target.id != target_id {
        return Err(ActiveReblitBootRepairEvidenceError::TargetStateMismatch {
            expected: target_id,
            actual: target.id,
        });
    }
    Ok(ActiveReblitBootRepairDatabaseInspection::Exact(
        ActiveReblitBootRepairDatabaseEvidence { context, target },
    ))
}

pub(super) fn require_exact_active_reblit_boot_repair_database(
    expected: &ActiveReblitBootRepairDatabaseEvidence,
    actual: ActiveReblitBootRepairDatabaseInspection,
) -> Result<ActiveReblitBootRepairDatabaseEvidence, ActiveReblitBootRepairEvidenceError> {
    match actual {
        ActiveReblitBootRepairDatabaseInspection::Exact(actual) if actual == *expected => Ok(actual),
        ActiveReblitBootRepairDatabaseInspection::Exact(actual) => {
            Err(ActiveReblitBootRepairEvidenceError::DatabaseChanged {
                expected: Box::new(expected.context.clone()),
                actual: Box::new(actual.context),
                target_changed: expected.target != actual.target,
            })
        }
        ActiveReblitBootRepairDatabaseInspection::Incompatible(evidence) => {
            Err(ActiveReblitBootRepairEvidenceError::DatabaseIncompatible {
                evidence: Box::new(evidence),
            })
        }
    }
}

pub(super) fn capture_active_reblit_boot_repair_active_state(
    record: &TransitionRecord,
    installation: &crate::Installation,
    reservation: &ActiveStateReservation,
) -> Result<ActiveStateSnapshot, ActiveReblitBootRepairEvidenceError> {
    let active_state = reservation
        .capture_for_startup_recovery(installation)
        .map_err(|source| ActiveReblitBootRepairEvidenceError::ActiveState {
            source: Box::new(source),
        })?;
    require_exact_active_reblit_boot_repair_active_state(record, installation, &active_state)?;
    Ok(active_state)
}

pub(super) fn require_exact_active_reblit_boot_repair_active_state(
    record: &TransitionRecord,
    installation: &crate::Installation,
    active_state: &ActiveStateSnapshot,
) -> Result<(), ActiveReblitBootRepairEvidenceError> {
    let expected = state::Id::from(
        record
            .previous
            .id
            .expect("validated ActiveReblit boot-repair record has a previous state ID"),
    );
    let actual = active_state.active();
    if actual != Some(expected) {
        return Err(ActiveReblitBootRepairEvidenceError::ActiveSelectionMismatch { expected, actual });
    }
    active_state
        .revalidate(installation)
        .map_err(|source| ActiveReblitBootRepairEvidenceError::ActiveState {
            source: Box::new(source),
        })
}

pub(super) fn active_reblit_pending_boot_repair_plan_is_exact(record: &TransitionRecord, phase: Phase) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    matches!(phase, Phase::BootRepairRequired | Phase::BootRepairStarted)
        && record.operation == Operation::ActiveReblit
        && record.phase == phase
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

pub(super) fn active_reblit_completed_boot_repair_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    record.operation == Operation::ActiveReblit
        && record.phase == Phase::BootRepairComplete
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
        && matches!(rollback.boot, BootRollback::Applied | BootRollback::AlreadySatisfied)
        && rollback.external_effects_may_remain
}

fn active_reblit_boot_repair_database_context_is_exact(record: &TransitionRecord, evidence: &DatabaseEvidence) -> bool {
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

#[derive(Debug, thiserror::Error)]
pub(super) enum ActiveReblitBootRepairEvidenceError {
    #[error("inspect exact ActiveReblit boot-repair database ownership and provenance")]
    Inspection(#[from] InspectionError),
    #[error("load the complete ActiveReblit boot-repair target state")]
    StateDatabase(#[source] db::Error),
    #[error("loaded boot-repair target state changed ID from {expected} to {actual}")]
    TargetStateMismatch { expected: state::Id, actual: state::Id },
    #[error("ActiveReblit boot-repair database context is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error(
        "ActiveReblit boot-repair database evidence changed (expected={expected:?}, actual={actual:?}, target_changed={target_changed})"
    )]
    DatabaseChanged {
        expected: Box<DatabaseEvidence>,
        actual: Box<DatabaseEvidence>,
        target_changed: bool,
    },
    #[error("prove exact live active-state selection for ActiveReblit boot repair")]
    ActiveState {
        #[source]
        source: Box<super::super::Error>,
    },
    #[error("ActiveReblit boot repair requires active state {expected}, found {actual:?}")]
    ActiveSelectionMismatch {
        expected: state::Id,
        actual: Option<state::Id>,
    },
}
