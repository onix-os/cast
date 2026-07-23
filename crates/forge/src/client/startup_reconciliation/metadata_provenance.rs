use crate::{
    db,
    transition_journal::{ForwardPhase, Phase, RollbackAction, TransitionRecord},
};

use super::DatabaseEvidence;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FreshProvenanceExpectation {
    Absent,
    Optional,
    Present,
}

pub(super) fn metadata_provenance_evidence_compatible(record: &TransitionRecord, evidence: &DatabaseEvidence) -> bool {
    match evidence {
        DatabaseEvidence::AllocationNotObserved { .. } => true,
        DatabaseEvidence::AllocationCommittedBehindJournal { provenance, .. } => provenance.is_none(),
        DatabaseEvidence::CandidateOwnership {
            ownership, provenance, ..
        } => {
            if *ownership == db::state::TransitionOwnership::Missing {
                return provenance.is_none();
            }
            match fresh_provenance_expectation(record) {
                FreshProvenanceExpectation::Absent => provenance.is_none(),
                FreshProvenanceExpectation::Optional => true,
                FreshProvenanceExpectation::Present => provenance.is_some(),
            }
        }
        DatabaseEvidence::ExistingCandidate { provenance, .. } => provenance.is_some(),
        DatabaseEvidence::Conflict(_) => false,
    }
}

fn fresh_provenance_expectation(record: &TransitionRecord) -> FreshProvenanceExpectation {
    if let Some(rollback) = &record.rollback {
        return match rollback.fresh_db {
            RollbackAction::Applied | RollbackAction::AlreadySatisfied => FreshProvenanceExpectation::Absent,
            RollbackAction::Pending | RollbackAction::NotRequired => forward_provenance_expectation(rollback.source),
        };
    }

    match record.phase {
        Phase::Preparing | Phase::FreshStateAllocating | Phase::FreshStateAllocated => {
            FreshProvenanceExpectation::Absent
        }
        Phase::CandidatePrepareStarted => FreshProvenanceExpectation::Optional,
        _ => FreshProvenanceExpectation::Present,
    }
}

fn forward_provenance_expectation(phase: ForwardPhase) -> FreshProvenanceExpectation {
    match phase {
        ForwardPhase::Preparing | ForwardPhase::FreshStateAllocating | ForwardPhase::FreshStateAllocated => {
            FreshProvenanceExpectation::Absent
        }
        ForwardPhase::CandidatePrepareStarted => FreshProvenanceExpectation::Optional,
        _ => FreshProvenanceExpectation::Present,
    }
}
