//! Fresh-handle restart contracts for both journal durability sides.

use crate::{
    client::startup_recovery::DurableUsrRollbackArchivedCandidatePreserveRecord,
    transition_journal::{
        Phase, RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{
    CandidateOrigin, CandidateSource, Epoch, assert_pending_phase, assert_persistence_advance, build_candidate,
    candidate_move_count, canonical_record_from_root, enter_candidate, enter_candidate_with_fresh_handles,
    expected_candidate_preserved, install_persistent_candidate_database, release_candidate_handles,
    reset_candidate_observers,
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

#[test]
fn startup_activate_archived_candidate_source_fault_fresh_entry_finishes_without_second_move() {
    for epoch in Epoch::ALL {
        for origin in CandidateOrigin::ALL {
            for source in CandidateSource::ALL {
                for usr_outcome in USR_OUTCOMES {
                    let mut fixture = build_candidate(epoch, source, usr_outcome, origin);
                    install_persistent_candidate_database(&mut fixture);
                    let source_record = fixture.candidate_intent.clone();
                    let expected = expected_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);
                    let expected_moves = usize::from(origin == CandidateOrigin::Applied);
                    reset_candidate_observers();
                    arm_next_temporary_sync_fault();

                    let first = enter_candidate(&fixture);

                    assert_temporary_sync_fault_consumed();
                    assert_persistence_advance(&first, DurableUsrRollbackArchivedCandidatePreserveRecord::Source);
                    assert_eq!(fixture.fixture.canonical_record(), source_record);
                    assert_eq!(candidate_move_count(), expected_moves);

                    let retained = release_candidate_handles(fixture);
                    let second = enter_candidate_with_fresh_handles(retained.path());

                    assert_pending_phase(&second, Phase::CandidatePreserved);
                    assert_eq!(canonical_record_from_root(retained.path()), expected);
                    assert_eq!(candidate_move_count(), expected_moves);
                }
            }
        }
    }
}

#[test]
fn startup_activate_archived_candidate_successor_fault_fresh_entry_completes_without_second_move() {
    for epoch in Epoch::ALL {
        for origin in CandidateOrigin::ALL {
            for source in CandidateSource::ALL {
                for usr_outcome in USR_OUTCOMES {
                    let mut fixture = build_candidate(epoch, source, usr_outcome, origin);
                    install_persistent_candidate_database(&mut fixture);
                    let preserved = expected_candidate_preserved(&fixture, origin);
                    let completion = preserved.rollback_successor(None).unwrap();
                    assert_eq!(completion.phase, Phase::RollbackComplete);
                    let expected_moves = usize::from(origin == CandidateOrigin::Applied);
                    reset_candidate_observers();
                    arm_next_update_first_directory_sync_fault();

                    let first = enter_candidate(&fixture);

                    assert_update_first_directory_sync_fault_consumed();
                    assert_persistence_advance(
                        &first,
                        DurableUsrRollbackArchivedCandidatePreserveRecord::CandidatePreserved,
                    );
                    assert_eq!(fixture.fixture.canonical_record(), preserved);
                    assert_eq!(candidate_move_count(), expected_moves);

                    let retained = release_candidate_handles(fixture);
                    let second = enter_candidate_with_fresh_handles(retained.path());

                    assert_pending_phase(&second, Phase::RollbackComplete);
                    assert_eq!(canonical_record_from_root(retained.path()), completion);
                    assert_eq!(candidate_move_count(), expected_moves);
                }
            }
        }
    }
}
