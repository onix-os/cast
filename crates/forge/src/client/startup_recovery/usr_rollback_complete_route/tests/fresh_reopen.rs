//! Same-process fresh-handle reopen contracts for both durability sides.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{UsrRollbackCompleteRouteAdmission, fresh_db_invalidation_removal_call_count},
        startup_recovery::{
            DurableUsrRollbackCompleteRouteRecord, UsrRollbackCompleteRoutePersistenceError,
            persist_usr_rollback_complete_route_and_reopen,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{
    CandidateResult, FreshCompleteRouteHandles, FreshDbOutcome, RouteFixture, Source,
    install_persistent_joint_absence_database, release_route_handles,
};

#[test]
fn startup_usr_rollback_complete_route_source_durable_fresh_handle_reopen_retries_only_the_route() {
    let mut executions = 0;
    for historical in [false, true] {
        for origin in FreshDbOutcome::ALL {
            for source in Source::THROUGH_ROLLBACK_COMPLETE {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateResult::ALL {
                        executions += 1;
                        let mut fixture = if historical {
                            RouteFixture::historical(origin, source, usr_outcome, candidate_outcome)
                        } else {
                            RouteFixture::new(origin, source, usr_outcome, candidate_outcome)
                        };
                        install_persistent_joint_absence_database(&mut fixture);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = fixture.capture_ready(&journal, &reservation);
                        let expected_source = fixture.source.clone();
                        let expected = fixture.expected_successor();
                        let expected_removals = origin.expected_removals();
                        arm_next_temporary_sync_fault();

                        let error =
                            persist_usr_rollback_complete_route_and_reopen(journal, authority).unwrap_err();

                        assert_temporary_sync_fault_consumed();
                        assert!(matches!(
                            error,
                            UsrRollbackCompleteRoutePersistenceError::Advance {
                                durable: DurableUsrRollbackCompleteRouteRecord::FreshDbInvalidated,
                                ..
                            }
                        ));
                        assert_eq!(fixture.canonical_record(), expected_source);
                        fixture.fixture.assert_exact_joint_absence();
                        assert_eq!(fresh_db_invalidation_removal_call_count(), expected_removals);
                        drop(reservation);

                        let retained = release_route_handles(fixture);
                        let fresh = FreshCompleteRouteHandles::open(retained.path());
                        assert_eq!(fresh.record, expected_source);
                        let all_before = fresh.database.all().unwrap();
                        let in_flight_before = fresh.database.audit_in_flight_transition().unwrap();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = fresh.capture_ready(&reservation);
                        let database = fresh.database.clone();
                        let journal = fresh.journal;

                        let (reopened, actual) =
                            persist_usr_rollback_complete_route_and_reopen(journal, authority).unwrap();

                        assert_eq!(actual, expected);
                        assert_eq!(reopened.load().unwrap(), Some(expected));
                        assert_eq!(database.all().unwrap(), all_before);
                        assert_eq!(database.audit_in_flight_transition().unwrap(), in_flight_before);
                        assert_eq!(fresh_db_invalidation_removal_call_count(), expected_removals);
                    }
                }
            }
        }
    }
    assert_eq!(executions, 48);
}

#[test]
fn startup_usr_rollback_complete_route_successor_durable_fresh_handle_reopen_skips_the_route() {
    let mut executions = 0;
    for historical in [false, true] {
        for origin in FreshDbOutcome::ALL {
            for source in Source::THROUGH_ROLLBACK_COMPLETE {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateResult::ALL {
                        executions += 1;
                        let mut fixture = if historical {
                            RouteFixture::historical(origin, source, usr_outcome, candidate_outcome)
                        } else {
                            RouteFixture::new(origin, source, usr_outcome, candidate_outcome)
                        };
                        install_persistent_joint_absence_database(&mut fixture);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = fixture.capture_ready(&journal, &reservation);
                        let expected = fixture.expected_successor();
                        let expected_removals = origin.expected_removals();
                        arm_next_update_first_directory_sync_fault();

                        let error =
                            persist_usr_rollback_complete_route_and_reopen(journal, authority).unwrap_err();

                        assert_update_first_directory_sync_fault_consumed();
                        assert!(matches!(
                            error,
                            UsrRollbackCompleteRoutePersistenceError::Advance {
                                durable: DurableUsrRollbackCompleteRouteRecord::RollbackComplete,
                                ..
                            }
                        ));
                        assert_eq!(fixture.canonical_record(), expected);
                        fixture.fixture.assert_exact_joint_absence();
                        assert_eq!(fresh_db_invalidation_removal_call_count(), expected_removals);
                        drop(reservation);

                        let retained = release_route_handles(fixture);
                        let fresh = FreshCompleteRouteHandles::open(retained.path());
                        assert_eq!(fresh.record, expected);
                        let all_before = fresh.database.all().unwrap();
                        let in_flight_before = fresh.database.audit_in_flight_transition().unwrap();
                        let reservation = ActiveStateReservation::acquire().unwrap();

                        assert!(matches!(
                            fresh.capture(&reservation).unwrap(),
                            UsrRollbackCompleteRouteAdmission::NotApplicable
                        ));
                        assert_eq!(fresh.journal.load().unwrap(), Some(expected));
                        assert_eq!(fresh.database.all().unwrap(), all_before);
                        assert_eq!(fresh.database.audit_in_flight_transition().unwrap(), in_flight_before);
                        assert_eq!(fresh_db_invalidation_removal_call_count(), expected_removals);
                    }
                }
            }
        }
    }
    assert_eq!(executions, 48);
}
