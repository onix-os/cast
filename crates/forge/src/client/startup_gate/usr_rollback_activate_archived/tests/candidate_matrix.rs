//! Full production-startup matrix for the bounded candidate checkpoint.

use crate::transition_journal::{Phase, RollbackActionOutcome};

use super::support::{
    CandidateOrigin, CandidateSource, Epoch, assert_candidate_pending_audit, assert_candidate_preserved_topology,
    build_candidate, candidate_move_count, enter_candidate, expected_candidate_preserved, reset_candidate_observers,
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

#[test]
fn startup_activate_archived_candidate_dispatch_applied_matrix_moves_once_and_returns_pending() {
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in USR_OUTCOMES {
                let fixture = build_candidate(epoch, source, usr_outcome, CandidateOrigin::Applied);
                let expected = expected_candidate_preserved(&fixture, CandidateOrigin::Applied);
                let database_before = fixture.fixture.database_snapshot();
                reset_candidate_observers();

                let error = enter_candidate(&fixture);

                assert_candidate_pending_audit(&error, &fixture, &expected);
                assert_eq!(fixture.fixture.canonical_record(), expected);
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_candidate_preserved_topology(&fixture, &expected);
                assert_eq!(candidate_move_count(), 1);
            }
        }
    }
}

#[test]
fn startup_activate_archived_candidate_dispatch_finish_matrix_never_moves_and_returns_pending() {
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in USR_OUTCOMES {
                let fixture = build_candidate(epoch, source, usr_outcome, CandidateOrigin::AlreadySatisfied);
                let expected = expected_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);
                let database_before = fixture.fixture.database_snapshot();
                let namespace_before = fixture.fixture.namespace_snapshot();
                reset_candidate_observers();

                let error = enter_candidate(&fixture);

                assert_candidate_pending_audit(&error, &fixture, &expected);
                assert_eq!(fixture.fixture.canonical_record(), expected);
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
                assert_candidate_preserved_topology(&fixture, &expected);
                assert_eq!(candidate_move_count(), 0);
            }
        }
    }
}

#[test]
fn startup_activate_archived_candidate_dispatch_never_falls_through_to_completion_in_same_entry() {
    let fixture = build_candidate(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::Applied,
    );
    let preserved = expected_candidate_preserved(&fixture, CandidateOrigin::Applied);
    let completion = preserved.rollback_successor(None).unwrap();
    assert_eq!(completion.phase, Phase::RollbackComplete);
    reset_candidate_observers();

    let error = enter_candidate(&fixture);

    assert_candidate_pending_audit(&error, &fixture, &preserved);
    assert_eq!(fixture.fixture.canonical_record(), preserved);
    assert_ne!(fixture.fixture.canonical_record(), completion);
    assert_eq!(candidate_move_count(), 1);
}
