use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::fresh_db_invalidation_removal_call_count,
        startup_recovery::{
            DurableUsrRollbackFreshDbInvalidationRecord, UsrRollbackFreshDbInvalidationPersistenceError,
            persist_usr_rollback_fresh_db_invalidation_and_reopen,
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

use super::support::{
    CandidateResult, FreshDbInvalidationOrigin, Source, effect_authority, expected_fresh_db_invalidated,
    fixture_for_origin,
};

#[test]
fn startup_fresh_db_invalidation_persistence_faults_reopen_exact_intent_or_invalidated_record() {
    let cases: [(fn(), fn(), DurableUsrRollbackFreshDbInvalidationRecord); 5] = [
        (
            arm_next_temporary_sync_fault,
            assert_temporary_sync_fault_consumed,
            DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidationIntent,
        ),
        (
            arm_next_update_exchange_fault,
            assert_update_exchange_fault_consumed,
            DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidationIntent,
        ),
        (
            arm_next_update_first_directory_sync_fault,
            assert_update_first_directory_sync_fault_consumed,
            DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,
        ),
        (
            arm_next_displaced_unlink_fault,
            assert_displaced_unlink_fault_consumed,
            DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,
        ),
        (
            arm_next_update_final_directory_sync_fault,
            assert_update_final_directory_sync_fault_consumed,
            DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,
        ),
    ];

    let mut executions = 0;
    for historical in [false, true] {
        for origin in FreshDbInvalidationOrigin::ALL {
            for source in Source::THROUGH_FRESH_DB_INVALIDATED {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateResult::ALL {
                        for (arm, assert_consumed, expected_durable) in cases {
                            executions += 1;
                            let fixture =
                                fixture_for_origin(origin, historical, source, usr_outcome, candidate_outcome);
                            let journal = fixture.open_journal();
                            let reservation = ActiveStateReservation::acquire().unwrap();
                            let authority = effect_authority(&fixture, &journal, &reservation, origin);
                            let expected = expected_fresh_db_invalidated(&fixture, origin);
                            let expected_removals = if origin == FreshDbInvalidationOrigin::Applied {
                                1
                            } else {
                                0
                            };
                            arm();

                            let error = persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority)
                                .unwrap_err();

                            assert_consumed();
                            assert!(matches!(
                                error,
                                UsrRollbackFreshDbInvalidationPersistenceError::Advance { durable, .. }
                                    if durable == expected_durable
                            ));
                            match expected_durable {
                                DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidationIntent => {
                                    assert_eq!(fixture.canonical_record(), fixture.record)
                                }
                                DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated => {
                                    assert_eq!(fixture.canonical_record(), expected)
                                }
                            }
                            fixture.assert_exact_joint_absence();
                            assert_eq!(fresh_db_invalidation_removal_call_count(), expected_removals);
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
    assert_eq!(executions, 240);
}

#[test]
fn startup_fresh_db_invalidation_persistence_consumes_old_store_and_reopens_exact_success() {
    for origin in FreshDbInvalidationOrigin::ALL {
        let fixture = fixture_for_origin(
            origin,
            false,
            Source::Intent,
            RollbackActionOutcome::Applied,
            CandidateResult::AlreadySatisfied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = effect_authority(&fixture, &journal, &reservation, origin);
        let expected = expected_fresh_db_invalidated(&fixture, origin);
        let expected_removals = if origin == FreshDbInvalidationOrigin::Applied {
            1
        } else {
            0
        };

        let (reopened, actual) = persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap();

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
        assert_eq!(fresh_db_invalidation_removal_call_count(), expected_removals);
    }
}
