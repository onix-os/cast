use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            DurableUsrRollbackCompleteRouteRecord, UsrRollbackCompleteRoutePersistenceError,
            persist_usr_rollback_complete_route_and_reopen,
        },
    },
    transition_journal::{
        RollbackActionOutcome, TransitionJournalStore, arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{CandidateResult, FreshDbOutcome, RouteFixture, Source};

#[test]
fn startup_usr_rollback_complete_route_storage_faults_reopen_exact_fresh_db_invalidated_or_rollback_complete() {
    let cases: [(fn(), fn(), DurableUsrRollbackCompleteRouteRecord); 5] = [
        (
            arm_next_temporary_sync_fault,
            assert_temporary_sync_fault_consumed,
            DurableUsrRollbackCompleteRouteRecord::FreshDbInvalidated,
        ),
        (
            arm_next_update_exchange_fault,
            assert_update_exchange_fault_consumed,
            DurableUsrRollbackCompleteRouteRecord::FreshDbInvalidated,
        ),
        (
            arm_next_update_first_directory_sync_fault,
            assert_update_first_directory_sync_fault_consumed,
            DurableUsrRollbackCompleteRouteRecord::RollbackComplete,
        ),
        (
            arm_next_displaced_unlink_fault,
            assert_displaced_unlink_fault_consumed,
            DurableUsrRollbackCompleteRouteRecord::RollbackComplete,
        ),
        (
            arm_next_update_final_directory_sync_fault,
            assert_update_final_directory_sync_fault_consumed,
            DurableUsrRollbackCompleteRouteRecord::RollbackComplete,
        ),
    ];

    let mut executions = 0;
    for historical in [false, true] {
        for origin in FreshDbOutcome::ALL {
            for source in Source::THROUGH_ROLLBACK_COMPLETE {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateResult::ALL {
                        for (arm, assert_consumed, expected_durable) in cases {
                            executions += 1;
                            let case = (origin, source, usr_outcome, candidate_outcome, expected_durable);
                            let fixture = if historical {
                                RouteFixture::historical(origin, source, usr_outcome, candidate_outcome)
                            } else {
                                RouteFixture::new(origin, source, usr_outcome, candidate_outcome)
                            };
                            let journal = fixture.open_journal();
                            let reservation = ActiveStateReservation::acquire().unwrap();
                            let authority = fixture.capture_ready(&journal, &reservation);
                            let expected = fixture.expected_successor();
                            let database_before = fixture.database_snapshot();
                            let namespace_before = fixture.namespace_snapshot();
                            arm();

                            let error =
                                persist_usr_rollback_complete_route_and_reopen(journal, authority).unwrap_err();

                            assert_consumed();
                            assert!(
                                matches!(
                                    error,
                                    UsrRollbackCompleteRoutePersistenceError::Advance { durable, .. }
                                        if durable == expected_durable
                                ),
                                "{case:?}: {error:?}"
                            );
                            match expected_durable {
                                DurableUsrRollbackCompleteRouteRecord::FreshDbInvalidated => {
                                    assert_eq!(fixture.canonical_record(), fixture.source, "{case:?}")
                                }
                                DurableUsrRollbackCompleteRouteRecord::RollbackComplete => {
                                    assert_eq!(fixture.canonical_record(), expected, "{case:?}")
                                }
                            }
                            assert_eq!(fixture.database_snapshot(), database_before, "{case:?}");
                            assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case:?}");
                            fixture.assert_no_second_removal();
                            let names = fs::read_dir(
                                fixture.fixture.fixture.fixture.installation.root.join(".cast/journal"),
                            )
                            .unwrap()
                            .map(|entry| entry.unwrap().file_name())
                            .collect::<Vec<_>>();
                            assert_eq!(names.len(), 2, "{case:?}: stale journal residue: {names:?}");
                        }
                    }
                }
            }
        }
    }
    assert_eq!(executions, 240);
}

#[test]
fn startup_usr_rollback_complete_route_consumes_old_store_and_returns_canonical_reopen() {
    for origin in FreshDbOutcome::ALL {
        let fixture = RouteFixture::new(
            origin,
            Source::Intent,
            RollbackActionOutcome::Applied,
            CandidateResult::AlreadySatisfied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        let expected = fixture.expected_successor();

        let (reopened, actual) = persist_usr_rollback_complete_route_and_reopen(journal, authority).unwrap();

        assert_eq!(actual, expected);
        assert_eq!(reopened.load().unwrap(), Some(expected.clone()));
        drop(reopened);
        let cast = fixture
            .fixture
            .fixture
            .fixture
            .installation
            .retained_mutable_cast_directory()
            .unwrap();
        let independent =
            TransitionJournalStore::try_open_in_retained_cast(cast, &fixture.fixture.fixture.fixture.installation.root)
                .unwrap();
        assert_eq!(independent.load().unwrap(), Some(expected));
        fixture.assert_no_second_removal();
    }
}
