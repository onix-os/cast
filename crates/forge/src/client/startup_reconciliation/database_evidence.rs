//! Exact state-database evidence for startup transition reconciliation.

use crate::{
    db,
    state::{self, TransitionId},
    transition_journal::{Operation, Phase, TransitionRecord},
};

use super::{InspectionError, metadata_provenance::metadata_provenance_evidence_compatible};

/// Exact state-database evidence correlated with the decoded journal record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum DatabaseEvidence {
    AllocationNotObserved {
        previous: Option<ExistingStateEvidence>,
    },
    AllocationCommittedBehindJournal {
        state: state::Id,
        provenance: Option<db::state::MetadataProvenance>,
        previous: Option<ExistingStateEvidence>,
    },
    CandidateOwnership {
        state: state::Id,
        ownership: db::state::TransitionOwnership,
        provenance: Option<db::state::MetadataProvenance>,
        previous: Option<ExistingStateEvidence>,
    },
    ExistingCandidate {
        candidate: ExistingStateEvidence,
        provenance: Option<db::state::MetadataProvenance>,
        previous: Option<ExistingStateEvidence>,
    },
    Conflict(DatabaseConflict),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExistingStateEvidence {
    pub(super) state: state::Id,
    pub(super) ownership: db::state::TransitionOwnership,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum DatabaseInspectionStability {
    Stable,
    Changed { after: DatabaseEvidence },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum DatabaseConflict {
    UnexpectedForExistingCandidate {
        state: state::Id,
        transition: TransitionId,
    },
    UnexpectedBeforeAllocationIntent {
        state: state::Id,
    },
    ForeignTransition {
        state: state::Id,
        transition: TransitionId,
    },
    CandidateStateMismatch {
        expected: state::Id,
        actual: state::Id,
    },
    InconsistentAuditOwnership {
        state: state::Id,
        audit_present: bool,
        ownership: db::state::TransitionOwnership,
    },
}

pub(super) fn inspect_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
    in_flight: Option<db::state::InFlightTransition>,
) -> Result<DatabaseEvidence, InspectionError> {
    if record.operation != Operation::NewState {
        if let Some(row) = in_flight {
            return Ok(DatabaseEvidence::Conflict(
                DatabaseConflict::UnexpectedForExistingCandidate {
                    state: row.state_id,
                    transition: row.transition_id,
                },
            ));
        }
        let candidate = state::Id::from(
            record
                .candidate
                .id
                .expect("validated existing-candidate record has a state ID"),
        );
        let candidate = ExistingStateEvidence {
            state: candidate,
            ownership: state_db.transition_ownership(candidate, &record.transition_id)?,
        };
        let provenance = state_db.metadata_provenance(candidate.state)?;
        let previous = inspect_previous_state(record, state_db, Some(candidate.state))?;
        return Ok(DatabaseEvidence::ExistingCandidate {
            candidate,
            provenance,
            previous,
        });
    }

    let Some(candidate) = record.candidate.id.map(state::Id::from) else {
        let previous = inspect_previous_state(record, state_db, None)?;
        return Ok(match in_flight {
            None => DatabaseEvidence::AllocationNotObserved { previous },
            Some(row) if row.transition_id != record.transition_id => {
                DatabaseEvidence::Conflict(DatabaseConflict::ForeignTransition {
                    state: row.state_id,
                    transition: row.transition_id,
                })
            }
            Some(row) if record.phase != Phase::FreshStateAllocating => {
                DatabaseEvidence::Conflict(DatabaseConflict::UnexpectedBeforeAllocationIntent { state: row.state_id })
            }
            Some(row) => DatabaseEvidence::AllocationCommittedBehindJournal {
                state: row.state_id,
                provenance: state_db.metadata_provenance(row.state_id)?,
                previous,
            },
        });
    };

    if let Some(row) = in_flight.as_ref() {
        if row.transition_id != record.transition_id {
            return Ok(DatabaseEvidence::Conflict(DatabaseConflict::ForeignTransition {
                state: row.state_id,
                transition: row.transition_id.clone(),
            }));
        }
        if row.state_id != candidate {
            return Ok(DatabaseEvidence::Conflict(DatabaseConflict::CandidateStateMismatch {
                expected: candidate,
                actual: row.state_id,
            }));
        }
    }

    let audit_present = in_flight.is_some();
    let ownership = state_db.transition_ownership(candidate, &record.transition_id)?;
    let ownership_consistent = if audit_present {
        ownership == db::state::TransitionOwnership::Matching
    } else {
        matches!(
            ownership,
            db::state::TransitionOwnership::Cleared | db::state::TransitionOwnership::Missing
        )
    };
    if !ownership_consistent {
        return Ok(DatabaseEvidence::Conflict(
            DatabaseConflict::InconsistentAuditOwnership {
                state: candidate,
                audit_present,
                ownership,
            },
        ));
    }
    let previous = inspect_previous_state(record, state_db, Some(candidate))?;
    let provenance = state_db.metadata_provenance(candidate)?;
    Ok(DatabaseEvidence::CandidateOwnership {
        state: candidate,
        ownership,
        provenance,
        previous,
    })
}

fn inspect_previous_state(
    record: &TransitionRecord,
    state_db: &db::state::Database,
    candidate: Option<state::Id>,
) -> Result<Option<ExistingStateEvidence>, InspectionError> {
    record
        .previous
        .id
        .map(state::Id::from)
        .filter(|previous| Some(*previous) != candidate)
        .map(|previous| {
            state_db
                .transition_ownership(previous, &record.transition_id)
                .map(|ownership| ExistingStateEvidence {
                    state: previous,
                    ownership,
                })
                .map_err(InspectionError::from)
        })
        .transpose()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FreshDatabaseExpectation {
    Matching,
    MatchingOrCleared,
    Cleared,
    MatchingOrMissing,
    Missing,
}

#[cfg(test)]
pub(super) fn database_evidence_compatible(record: &TransitionRecord, evidence: &DatabaseEvidence) -> bool {
    database_ownership_evidence_compatible(record, evidence)
        && metadata_provenance_evidence_compatible(record, evidence)
}

pub(super) fn database_ownership_evidence_compatible(record: &TransitionRecord, evidence: &DatabaseEvidence) -> bool {
    match evidence {
        DatabaseEvidence::AllocationNotObserved { previous } => {
            previous_state_compatible(previous)
                && record.candidate.id.is_none()
                && (matches!(record.phase, Phase::Preparing | Phase::FreshStateAllocating)
                    || record.rollback.as_ref().is_some_and(|rollback| {
                        matches!(
                            rollback.fresh_db,
                            crate::transition_journal::RollbackAction::NotRequired
                                | crate::transition_journal::RollbackAction::AlreadySatisfied
                        )
                    }))
        }
        DatabaseEvidence::AllocationCommittedBehindJournal { previous, .. } => {
            previous_state_compatible(previous) && record.phase == Phase::FreshStateAllocating
        }
        DatabaseEvidence::CandidateOwnership {
            ownership, previous, ..
        } => {
            previous_state_compatible(previous)
                && match fresh_database_expectation(record) {
                    FreshDatabaseExpectation::Matching => *ownership == db::state::TransitionOwnership::Matching,
                    FreshDatabaseExpectation::MatchingOrCleared => matches!(
                        ownership,
                        db::state::TransitionOwnership::Matching | db::state::TransitionOwnership::Cleared
                    ),
                    FreshDatabaseExpectation::Cleared => *ownership == db::state::TransitionOwnership::Cleared,
                    FreshDatabaseExpectation::MatchingOrMissing => matches!(
                        ownership,
                        db::state::TransitionOwnership::Matching | db::state::TransitionOwnership::Missing
                    ),
                    FreshDatabaseExpectation::Missing => *ownership == db::state::TransitionOwnership::Missing,
                }
        }
        DatabaseEvidence::ExistingCandidate {
            candidate, previous, ..
        } => {
            candidate.ownership == db::state::TransitionOwnership::Cleared
                && previous
                    .as_ref()
                    .is_none_or(|previous| previous.ownership == db::state::TransitionOwnership::Cleared)
        }
        DatabaseEvidence::Conflict(_) => false,
    }
}

fn previous_state_compatible(previous: &Option<ExistingStateEvidence>) -> bool {
    previous
        .as_ref()
        .is_none_or(|previous| previous.ownership == db::state::TransitionOwnership::Cleared)
}

pub(super) fn fresh_database_expectation(record: &TransitionRecord) -> FreshDatabaseExpectation {
    if record.rollback.is_none() {
        return match record.phase {
            Phase::CommitDecided => FreshDatabaseExpectation::MatchingOrCleared,
            Phase::CommitCleanupComplete | Phase::Complete => FreshDatabaseExpectation::Cleared,
            _ => FreshDatabaseExpectation::Matching,
        };
    }

    let rollback = record.rollback.as_ref().expect("checked rollback record");
    match rollback.fresh_db {
        crate::transition_journal::RollbackAction::Pending if record.phase == Phase::FreshDbInvalidationIntent => {
            FreshDatabaseExpectation::MatchingOrMissing
        }
        crate::transition_journal::RollbackAction::Pending => FreshDatabaseExpectation::Matching,
        crate::transition_journal::RollbackAction::Applied
        | crate::transition_journal::RollbackAction::AlreadySatisfied => FreshDatabaseExpectation::Missing,
        crate::transition_journal::RollbackAction::NotRequired => FreshDatabaseExpectation::Matching,
    }
}
