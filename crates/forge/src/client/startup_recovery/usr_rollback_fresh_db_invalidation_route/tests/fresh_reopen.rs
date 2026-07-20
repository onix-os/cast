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

use super::support::{CandidateOutcome, CandidateSource, FreshRouteHandles, RouteFixture};

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_source_durable_fresh_handle_reopen_retries_only_the_route() {
    for historical in [false, true] {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    let mut fixture = RouteFixture::at_epoch(historical, source, usr_outcome, candidate_outcome);
                    fixture.install_persistent_database();
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

                    let expected_source = fixture.source.clone();
                    let expected = fixture.expected_successor();
                    let candidate_state = fixture.fixture.fixture.candidate_state;
                    let retained = fixture.release_handles();
                    let fresh = FreshRouteHandles::open(retained.path());
                    assert_eq!(fresh.record, expected_source);
                    let all_before = fresh.database.all().unwrap();
                    let in_flight_before = fresh.database.audit_in_flight_transition().unwrap();
                    let provenance_before = fresh.database.metadata_provenance(candidate_state).unwrap();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = fresh.capture_ready(&reservation);
                    let database = fresh.database.clone();
                    let journal = fresh.journal;
                    let (reopened, actual) =
                        persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority).unwrap();

                    assert_eq!(actual, expected);
                    assert_eq!(reopened.load().unwrap(), Some(expected));
                    assert_eq!(database.all().unwrap(), all_before);
                    assert_eq!(database.audit_in_flight_transition().unwrap(), in_flight_before);
                    assert_eq!(database.metadata_provenance(candidate_state).unwrap(), provenance_before);
                    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
                }
            }
        }
    }
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_successor_durable_fresh_handle_reopen_skips_the_route() {
    for historical in [false, true] {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    let mut fixture = RouteFixture::at_epoch(historical, source, usr_outcome, candidate_outcome);
                    fixture.install_persistent_database();
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

                    let candidate_state = fixture.fixture.fixture.candidate_state;
                    let retained = fixture.release_handles();
                    let fresh = FreshRouteHandles::open(retained.path());
                    assert_eq!(fresh.record, expected);
                    let all_before = fresh.database.all().unwrap();
                    let in_flight_before = fresh.database.audit_in_flight_transition().unwrap();
                    let provenance_before = fresh.database.metadata_provenance(candidate_state).unwrap();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    assert!(matches!(
                        fresh.capture(&reservation).unwrap(),
                        UsrRollbackFreshDbInvalidationRouteAdmission::NotApplicable
                    ));
                    assert_eq!(fresh.journal.load().unwrap(), Some(expected));
                    assert_eq!(fresh.database.all().unwrap(), all_before);
                    assert_eq!(fresh.database.audit_in_flight_transition().unwrap(), in_flight_before);
                    assert_eq!(fresh.database.metadata_provenance(candidate_state).unwrap(), provenance_before);
                    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
                }
            }
        }
    }
}
