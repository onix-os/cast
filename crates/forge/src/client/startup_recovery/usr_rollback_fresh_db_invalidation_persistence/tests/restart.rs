use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{UsrRollbackFreshDbInvalidationAdmission, fresh_db_invalidation_removal_call_count},
        startup_recovery::{
            DurableUsrRollbackFreshDbInvalidationRecord, UsrRollbackFreshDbInvalidationEffectSeal,
            UsrRollbackFreshDbInvalidationPersistenceError, persist_usr_rollback_fresh_db_invalidation_and_reopen,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{
    CandidateResult, FreshDbInvalidationOrigin, Source, capture_record, expected_fresh_db_invalidated,
    fixture_for_origin,
};

#[test]
fn startup_fresh_db_invalidation_persistence_source_fault_restart_uses_zero_removal_finish() {
    for first_origin in FreshDbInvalidationOrigin::ALL {
        for source in Source::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateResult::ALL {
                    let fixture = fixture_for_origin(first_origin, false, source, usr_outcome, candidate_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = super::support::effect_authority(&fixture, &journal, &reservation, first_origin);
                    arm_next_temporary_sync_fault();

                    let error = persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap_err();

                    assert_temporary_sync_fault_consumed();
                    assert!(matches!(
                        error,
                        UsrRollbackFreshDbInvalidationPersistenceError::Advance {
                            durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidationIntent,
                            ..
                        }
                    ));
                    assert_eq!(fixture.canonical_record(), fixture.record);
                    fixture.assert_exact_joint_absence();
                    assert_eq!(
                        fresh_db_invalidation_removal_call_count(),
                        if first_origin == FreshDbInvalidationOrigin::Applied {
                            1
                        } else {
                            0
                        }
                    );

                    drop(reservation);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let finish = fixture.capture_finish(&journal, &reservation);
                    let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
                    let authority = finish.reconcile(&seal, &journal).unwrap();
                    assert_eq!(authority.origin_for_test(), RollbackActionOutcome::AlreadySatisfied);
                    assert_eq!(fresh_db_invalidation_removal_call_count(), 0);
                    let expected = expected_fresh_db_invalidated(&fixture, FreshDbInvalidationOrigin::AlreadySatisfied);

                    let (reopened, actual) =
                        persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap();

                    assert_eq!(actual, expected);
                    assert_eq!(reopened.load().unwrap(), Some(expected));
                    assert_eq!(fresh_db_invalidation_removal_call_count(), 0);
                    fixture.assert_exact_joint_absence();
                }
            }
        }
    }
}

#[test]
fn startup_fresh_db_invalidation_persistence_successor_fault_restart_is_not_applicable() {
    for origin in FreshDbInvalidationOrigin::ALL {
        for source in Source::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateResult::ALL {
                    let fixture = fixture_for_origin(origin, false, source, usr_outcome, candidate_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = super::support::effect_authority(&fixture, &journal, &reservation, origin);
                    let expected = expected_fresh_db_invalidated(&fixture, origin);
                    arm_next_update_first_directory_sync_fault();

                    let error = persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap_err();

                    assert_update_first_directory_sync_fault_consumed();
                    assert!(matches!(
                        error,
                        UsrRollbackFreshDbInvalidationPersistenceError::Advance {
                            durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,
                            ..
                        }
                    ));
                    assert_eq!(fixture.canonical_record(), expected);
                    fixture.assert_exact_joint_absence();
                    let removals_before_restart = fresh_db_invalidation_removal_call_count();

                    drop(reservation);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    assert!(matches!(
                        capture_record(&fixture.fixture, &journal, &reservation, &expected).unwrap(),
                        UsrRollbackFreshDbInvalidationAdmission::NotApplicable
                    ));
                    assert_eq!(journal.load().unwrap(), Some(expected));
                    assert_eq!(fresh_db_invalidation_removal_call_count(), removals_before_restart);
                    fixture.assert_exact_joint_absence();
                }
            }
        }
    }
}
