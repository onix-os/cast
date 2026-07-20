use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            DurableUsrRollbackFreshDbInvalidationRouteRecord, UsrRollbackFreshDbInvalidationRoutePersistenceError,
            persist_usr_rollback_fresh_db_invalidation_route_and_reopen,
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

use super::support::{CandidateOutcome, CandidateSource, RouteFixture};

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_storage_faults_reopen_exact_source_or_successor() {
    let cases: [(fn(), fn(), DurableUsrRollbackFreshDbInvalidationRouteRecord); 5] = [
        (
            arm_next_temporary_sync_fault,
            assert_temporary_sync_fault_consumed,
            DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved,
        ),
        (
            arm_next_update_exchange_fault,
            assert_update_exchange_fault_consumed,
            DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved,
        ),
        (
            arm_next_update_first_directory_sync_fault,
            assert_update_first_directory_sync_fault_consumed,
            DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,
        ),
        (
            arm_next_displaced_unlink_fault,
            assert_displaced_unlink_fault_consumed,
            DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,
        ),
        (
            arm_next_update_final_directory_sync_fault,
            assert_update_final_directory_sync_fault_consumed,
            DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,
        ),
    ];

    for historical in [false, true] {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    for (arm, assert_consumed, expected_durable) in cases {
                        let fixture = RouteFixture::at_epoch(historical, source, usr_outcome, candidate_outcome);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = fixture.capture_ready(&journal, &reservation);
                        let successor = fixture.expected_successor();
                        arm();

                        let error = persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority)
                            .unwrap_err();

                        assert_consumed();
                        assert!(matches!(
                            error,
                            UsrRollbackFreshDbInvalidationRoutePersistenceError::Advance { durable, .. }
                                if durable == expected_durable
                        ));
                        match expected_durable {
                            DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved => {
                                assert_eq!(fixture.canonical_record(), fixture.source)
                            }
                            DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent => {
                                assert_eq!(fixture.canonical_record(), successor)
                            }
                        }
                        let names = fs::read_dir(fixture.fixture.fixture.installation.root.join(".cast/journal"))
                            .unwrap()
                            .map(|entry| entry.unwrap().file_name())
                            .collect::<Vec<_>>();
                        assert_eq!(names.len(), 2, "stale journal residue remained after reopen: {names:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_consumes_old_store_and_returns_canonical_reopen() {
    for candidate_outcome in CandidateOutcome::ALL {
        let fixture = RouteFixture::new(
            CandidateSource::Intent,
            RollbackActionOutcome::Applied,
            candidate_outcome,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        let expected = fixture.expected_successor();

        let (reopened, actual) =
            persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority).unwrap();

        assert_eq!(actual, expected);
        assert_eq!(reopened.load().unwrap(), Some(expected.clone()));
        drop(reopened);
        let cast = fixture
            .fixture
            .fixture
            .installation
            .retained_mutable_cast_directory()
            .unwrap();
        let independent =
            TransitionJournalStore::try_open_in_retained_cast(cast, &fixture.fixture.fixture.installation.root)
                .unwrap();
        assert_eq!(independent.load().unwrap(), Some(expected));
    }
}
