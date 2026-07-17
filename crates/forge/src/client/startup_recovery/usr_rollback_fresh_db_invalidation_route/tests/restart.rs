use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationRouteAdmission, new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count,
        },
        startup_recovery::{
            DurableUsrRollbackFreshDbInvalidationRouteRecord, UsrRollbackFreshDbInvalidationRoutePersistenceError,
            persist_usr_rollback_fresh_db_invalidation_route_and_reopen,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{CandidateOutcome, CandidateSource, RouteFixture, capture_record};

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_source_fault_restart_retries_only_the_route() {
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for candidate_outcome in CandidateOutcome::ALL {
                let fixture = RouteFixture::new(source, usr_outcome, candidate_outcome);
                let database_before = fixture.database_snapshot();
                let namespace_before = fixture.namespace_snapshot();
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_new_state_candidate_preserve_move_attempt_count();
                let authority = fixture.capture_ready(&journal, &reservation);
                arm_next_temporary_sync_fault();

                let error =
                    persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority).unwrap_err();

                assert_temporary_sync_fault_consumed();
                assert!(matches!(
                    error,
                    UsrRollbackFreshDbInvalidationRoutePersistenceError::Advance {
                        durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved,
                        ..
                    }
                ));
                assert_eq!(fixture.canonical_record(), fixture.source);
                assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
                drop(reservation);

                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let authority = fixture.capture_ready(&journal, &reservation);
                let expected = fixture.expected_successor();
                let (reopened, actual) =
                    persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority).unwrap();

                assert_eq!(actual, expected);
                assert_eq!(reopened.load().unwrap(), Some(expected));
                assert_eq!(fixture.database_snapshot(), database_before);
                assert_eq!(fixture.namespace_snapshot(), namespace_before);
                assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
            }
        }
    }
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_successor_fault_restart_skips_the_route() {
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for candidate_outcome in CandidateOutcome::ALL {
                let fixture = RouteFixture::new(source, usr_outcome, candidate_outcome);
                let database_before = fixture.database_snapshot();
                let namespace_before = fixture.namespace_snapshot();
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_new_state_candidate_preserve_move_attempt_count();
                let authority = fixture.capture_ready(&journal, &reservation);
                let expected = fixture.expected_successor();
                arm_next_update_first_directory_sync_fault();

                let error =
                    persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority).unwrap_err();

                assert_update_first_directory_sync_fault_consumed();
                assert!(matches!(
                    error,
                    UsrRollbackFreshDbInvalidationRoutePersistenceError::Advance {
                        durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,
                        ..
                    }
                ));
                assert_eq!(fixture.canonical_record(), expected);
                drop(reservation);

                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                assert!(matches!(
                    capture_record(&fixture.fixture, &journal, &reservation, &expected).unwrap(),
                    UsrRollbackFreshDbInvalidationRouteAdmission::NotApplicable
                ));
                assert_eq!(journal.load().unwrap(), Some(expected));
                assert_eq!(fixture.database_snapshot(), database_before);
                assert_eq!(fixture.namespace_snapshot(), namespace_before);
                assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
            }
        }
    }
}
