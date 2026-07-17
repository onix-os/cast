use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCompleteRouteAdmission, UsrRollbackFreshDbInvalidationAdmission,
            fresh_db_invalidation_removal_call_count,
        },
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

use super::support::{CandidateResult, FreshDbOutcome, RouteFixture, Source, capture_invalidation_record};

#[test]
fn startup_usr_rollback_complete_route_source_fault_restart_retries_only_the_completion_route() {
    for origin in FreshDbOutcome::ALL {
        for source in Source::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateResult::ALL {
                    let case = (origin, source, usr_outcome, candidate_outcome);
                    let fixture = RouteFixture::new(origin, source, usr_outcome, candidate_outcome);
                    let database_before = fixture.database_snapshot();
                    let namespace_before = fixture.namespace_snapshot();
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = fixture.capture_ready(&journal, &reservation);
                    arm_next_temporary_sync_fault();

                    let error = persist_usr_rollback_complete_route_and_reopen(journal, authority).unwrap_err();

                    assert_temporary_sync_fault_consumed();
                    assert!(
                        matches!(
                            error,
                            UsrRollbackCompleteRoutePersistenceError::Advance {
                                durable: DurableUsrRollbackCompleteRouteRecord::FreshDbInvalidated,
                                ..
                            }
                        ),
                        "{case:?}: {error:?}"
                    );
                    assert_eq!(fixture.canonical_record(), fixture.source, "{case:?}");
                    fixture.assert_no_second_removal();

                    drop(reservation);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    assert!(matches!(
                        capture_invalidation_record(&fixture.fixture.fixture, &journal, &reservation, &fixture.source,)
                            .unwrap(),
                        UsrRollbackFreshDbInvalidationAdmission::NotApplicable
                    ));
                    let authority = fixture.capture_ready(&journal, &reservation);
                    let expected = fixture.expected_successor();

                    let (reopened, actual) =
                        persist_usr_rollback_complete_route_and_reopen(journal, authority).unwrap();

                    assert_eq!(actual, expected, "{case:?}");
                    assert_eq!(reopened.load().unwrap(), Some(expected), "{case:?}");
                    assert_eq!(fixture.database_snapshot(), database_before, "{case:?}");
                    assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case:?}");
                    assert_eq!(
                        fresh_db_invalidation_removal_call_count(),
                        origin.expected_removals(),
                        "{case:?}"
                    );
                    fixture.assert_no_second_removal();
                }
            }
        }
    }
}

#[test]
fn startup_usr_rollback_complete_route_rollback_complete_fault_restart_skips_route_and_invalidation() {
    for origin in FreshDbOutcome::ALL {
        for source in Source::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateResult::ALL {
                    let case = (origin, source, usr_outcome, candidate_outcome);
                    let fixture = RouteFixture::new(origin, source, usr_outcome, candidate_outcome);
                    let database_before = fixture.database_snapshot();
                    let namespace_before = fixture.namespace_snapshot();
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = fixture.capture_ready(&journal, &reservation);
                    let expected = fixture.expected_successor();
                    arm_next_update_first_directory_sync_fault();

                    let error = persist_usr_rollback_complete_route_and_reopen(journal, authority).unwrap_err();

                    assert_update_first_directory_sync_fault_consumed();
                    assert!(
                        matches!(
                            error,
                            UsrRollbackCompleteRoutePersistenceError::Advance {
                                durable: DurableUsrRollbackCompleteRouteRecord::RollbackComplete,
                                ..
                            }
                        ),
                        "{case:?}: {error:?}"
                    );
                    assert_eq!(fixture.canonical_record(), expected, "{case:?}");
                    fixture.assert_no_second_removal();

                    drop(reservation);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    assert!(matches!(
                        capture_invalidation_record(&fixture.fixture.fixture, &journal, &reservation, &expected,)
                            .unwrap(),
                        UsrRollbackFreshDbInvalidationAdmission::NotApplicable
                    ));
                    assert!(matches!(
                        super::support::capture_record(&fixture.fixture, &journal, &reservation, &expected,).unwrap(),
                        UsrRollbackCompleteRouteAdmission::NotApplicable
                    ));
                    assert_eq!(journal.load().unwrap(), Some(expected), "{case:?}");
                    assert_eq!(fixture.database_snapshot(), database_before, "{case:?}");
                    assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case:?}");
                    fixture.assert_no_second_removal();
                }
            }
        }
    }
}
