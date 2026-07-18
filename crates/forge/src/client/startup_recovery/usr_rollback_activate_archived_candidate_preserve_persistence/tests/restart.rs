//! Restart contracts for both durable sides of journal persistence faults.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, archived_candidate_preserve_move_attempt_count,
            reset_archived_candidate_preserve_move_attempt_count,
            reset_archived_candidate_preserve_post_move_durability_events,
            take_archived_candidate_preserve_post_move_durability_events,
        },
        startup_recovery::{
            DurableUsrRollbackArchivedCandidatePreserveRecord, UsrRollbackArchivedCandidatePreservePersistenceError,
            persist_usr_rollback_archived_candidate_preserve_and_reopen,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::super::candidate_test_support::{CandidateSource, capture_record};
use super::support::{
    CandidateOrigin, Epoch, assert_preserved, durable_authority, expected_candidate_preserved, expected_post_events,
    fixture_for_origin,
};

#[test]
fn startup_archived_candidate_preserve_source_fault_restart_finishes_without_second_move() {
    for epoch in Epoch::ALL {
        for first_origin in CandidateOrigin::ALL {
            for source in CandidateSource::ALL {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    let fixture = fixture_for_origin(epoch, first_origin, source, usr_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_archived_candidate_preserve_move_attempt_count();
                    let authority = durable_authority(&fixture, &journal, &reservation, first_origin);
                    let first_move_count = usize::from(first_origin == CandidateOrigin::Applied);
                    arm_next_temporary_sync_fault();

                    let result = persist_usr_rollback_archived_candidate_preserve_and_reopen(journal, authority);
                    drop(reservation);

                    assert_temporary_sync_fault_consumed();
                    assert!(matches!(
                        result.unwrap_err(),
                        UsrRollbackArchivedCandidatePreservePersistenceError::Advance {
                            durable: DurableUsrRollbackArchivedCandidatePreserveRecord::Source,
                            ..
                        }
                    ));
                    assert_eq!(fixture.fixture.canonical_record(), fixture.candidate_intent);
                    assert_preserved(&fixture);

                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_archived_candidate_preserve_post_move_durability_events();
                    let authority =
                        durable_authority(&fixture, &journal, &reservation, CandidateOrigin::AlreadySatisfied);
                    assert_eq!(
                        take_archived_candidate_preserve_post_move_durability_events(),
                        expected_post_events(&fixture)
                    );
                    assert_eq!(archived_candidate_preserve_move_attempt_count(), first_move_count);
                    let expected = expected_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);

                    let result = persist_usr_rollback_archived_candidate_preserve_and_reopen(journal, authority);
                    drop(reservation);
                    let (reopened, actual) = result.unwrap();

                    assert_eq!(actual, expected);
                    assert_eq!(reopened.load().unwrap(), Some(expected));
                    assert_eq!(archived_candidate_preserve_move_attempt_count(), first_move_count);
                }
            }
        }
    }
}

#[test]
fn startup_archived_candidate_preserve_successor_fault_restart_skips_preservation() {
    for epoch in Epoch::ALL {
        for origin in CandidateOrigin::ALL {
            for source in CandidateSource::ALL {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    let fixture = fixture_for_origin(epoch, origin, source, usr_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_archived_candidate_preserve_move_attempt_count();
                    let authority = durable_authority(&fixture, &journal, &reservation, origin);
                    let expected_move_count = usize::from(origin == CandidateOrigin::Applied);
                    let expected = expected_candidate_preserved(&fixture, origin);
                    let database_before = fixture.fixture.database_snapshot();
                    arm_next_update_first_directory_sync_fault();

                    let result = persist_usr_rollback_archived_candidate_preserve_and_reopen(journal, authority);
                    drop(reservation);

                    assert_update_first_directory_sync_fault_consumed();
                    assert!(matches!(
                        result.unwrap_err(),
                        UsrRollbackArchivedCandidatePreservePersistenceError::Advance {
                            durable: DurableUsrRollbackArchivedCandidatePreserveRecord::CandidatePreserved,
                            ..
                        }
                    ));
                    assert_eq!(fixture.fixture.canonical_record(), expected);
                    assert_eq!(fixture.fixture.database_snapshot(), database_before);
                    assert_eq!(archived_candidate_preserve_move_attempt_count(), expected_move_count);
                    assert_preserved(&fixture);

                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let not_applicable = matches!(
                        capture_record(&fixture.fixture, &journal, &reservation, &expected),
                        UsrRollbackCandidatePreserveAdmission::NotApplicable
                    );
                    drop(reservation);
                    assert!(not_applicable);
                    assert_eq!(journal.load().unwrap(), Some(expected));
                    assert_eq!(archived_candidate_preserve_move_attempt_count(), expected_move_count);
                }
            }
        }
    }
}
