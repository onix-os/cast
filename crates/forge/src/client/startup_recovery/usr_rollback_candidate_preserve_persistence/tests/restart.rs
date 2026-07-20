//! Restart contracts for each durable journal fault side.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_post_move_durability_events,
            take_new_state_candidate_preserve_post_move_durability_events,
        },
        startup_recovery::{
            DurableUsrRollbackCandidatePreserveRecord, UsrRollbackCandidatePreservePersistenceError,
            persist_usr_rollback_candidate_preserve_and_reopen,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{
    CandidateOrigin, Source, capture_record, durable_authority, expected_candidate_preserved, expected_post_events,
    fixture_for_origin_at_epoch,
};

#[test]
fn startup_usr_rollback_candidate_preserve_source_fault_restart_finishes_without_second_move() {
    for historical in [false, true] {
        for first_origin in CandidateOrigin::ALL {
            for source in Source::ALL {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    let fixture = fixture_for_origin_at_epoch(historical, first_origin, source, usr_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_new_state_candidate_preserve_move_attempt_count();
                    reset_new_state_candidate_preserve_post_move_durability_events();
                    let authority = durable_authority(&fixture, &journal, &reservation, first_origin);
                    let first_move_count = usize::from(first_origin == CandidateOrigin::Applied);
                    assert_eq!(new_state_candidate_preserve_move_attempt_count(), first_move_count);
                    arm_next_temporary_sync_fault();

                    let error = persist_usr_rollback_candidate_preserve_and_reopen(journal, authority).unwrap_err();

                    assert_temporary_sync_fault_consumed();
                    assert!(matches!(
                        error,
                        UsrRollbackCandidatePreservePersistenceError::Advance {
                            durable: DurableUsrRollbackCandidatePreserveRecord::Source,
                            ..
                        }
                    ));
                    assert_eq!(fixture.fixture.canonical_record(), fixture.candidate_intent);
                    drop(reservation);

                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_new_state_candidate_preserve_post_move_durability_events();
                    let authority =
                        durable_authority(&fixture, &journal, &reservation, CandidateOrigin::AlreadySatisfied);
                    assert_eq!(
                        take_new_state_candidate_preserve_post_move_durability_events(),
                        expected_post_events(&fixture)
                    );
                    assert_eq!(new_state_candidate_preserve_move_attempt_count(), first_move_count);
                    let expected = expected_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);

                    let (reopened, actual) =
                        persist_usr_rollback_candidate_preserve_and_reopen(journal, authority).unwrap();

                    assert_eq!(actual, expected);
                    assert_eq!(reopened.load().unwrap(), Some(expected));
                    assert_eq!(new_state_candidate_preserve_move_attempt_count(), first_move_count);
                }
            }
        }
    }
}

#[test]
fn startup_usr_rollback_candidate_preserve_successor_fault_restart_skips_preservation() {
    for historical in [false, true] {
        for origin in CandidateOrigin::ALL {
            for source in Source::ALL {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    let fixture = fixture_for_origin_at_epoch(historical, origin, source, usr_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_new_state_candidate_preserve_move_attempt_count();
                    let authority = durable_authority(&fixture, &journal, &reservation, origin);
                    let expected_move_count = usize::from(origin == CandidateOrigin::Applied);
                    let expected = expected_candidate_preserved(&fixture, origin);
                    let database_before = fixture.fixture.database_snapshot();
                    arm_next_update_first_directory_sync_fault();

                    let error = persist_usr_rollback_candidate_preserve_and_reopen(journal, authority).unwrap_err();

                    assert_update_first_directory_sync_fault_consumed();
                    assert!(matches!(
                        error,
                        UsrRollbackCandidatePreservePersistenceError::Advance {
                            durable: DurableUsrRollbackCandidatePreserveRecord::CandidatePreserved,
                            ..
                        }
                    ));
                    assert_eq!(fixture.fixture.canonical_record(), expected);
                    assert_eq!(fixture.fixture.database_snapshot(), database_before);
                    assert_eq!(new_state_candidate_preserve_move_attempt_count(), expected_move_count);
                    drop(reservation);

                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    assert!(matches!(
                        capture_record(&fixture.fixture, &journal, &reservation, &expected),
                        UsrRollbackCandidatePreserveAdmission::NotApplicable
                    ));
                    assert_eq!(journal.load().unwrap(), Some(expected));
                    assert_eq!(fixture.fixture.database_snapshot(), database_before);
                    assert_eq!(new_state_candidate_preserve_move_attempt_count(), expected_move_count);
                }
            }
        }
    }
}
