use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationAdmission, fresh_db_invalidation_removal_call_count,
        },
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
    CandidateResult, FreshDbInvalidationOrigin, FreshInvalidationHandles, Source, effect_authority,
    expected_fresh_db_invalidated, fixture_for_origin, install_persistent_database, release_handles,
};

#[test]
fn startup_fresh_db_invalidation_source_durable_fresh_handle_reopen_uses_zero_removal_finish() {
    let mut executions = 0;
    for historical in [false, true] {
        for first_origin in FreshDbInvalidationOrigin::ALL {
            for source in Source::THROUGH_FRESH_DB_INVALIDATED {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateResult::ALL {
                        executions += 1;
                        let mut fixture =
                            fixture_for_origin(first_origin, historical, source, usr_outcome, candidate_outcome);
                        install_persistent_database(&mut fixture, first_origin);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = effect_authority(&fixture, &journal, &reservation, first_origin);
                        let expected =
                            expected_fresh_db_invalidated(&fixture, FreshDbInvalidationOrigin::AlreadySatisfied);
                        arm_next_temporary_sync_fault();

                        let error =
                            persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap_err();

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
                            usize::from(first_origin == FreshDbInvalidationOrigin::Applied)
                        );
                        drop(reservation);

                        let candidate = fixture.fixture.fixture.candidate_state;
                        let retained = release_handles(fixture);
                        let fresh = FreshInvalidationHandles::open(retained.path());
                        assert_eq!(fresh.record.phase, crate::transition_journal::Phase::FreshDbInvalidationIntent);
                        let all_before = fresh.database.all().unwrap();
                        let in_flight_before = fresh.database.audit_in_flight_transition().unwrap();
                        let provenance_before = fresh.database.metadata_provenance(candidate).unwrap();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let UsrRollbackFreshDbInvalidationAdmission::Finish(finish) =
                            fresh.capture(&reservation).unwrap()
                        else {
                            panic!("fresh exact source plus joint absence did not admit Finish");
                        };
                        let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
                        let authority = finish.reconcile(&seal, &fresh.journal).unwrap();
                        assert_eq!(authority.origin_for_test(), RollbackActionOutcome::AlreadySatisfied);
                        assert_eq!(fresh_db_invalidation_removal_call_count(), 0);
                        let database = fresh.database.clone();
                        let journal = fresh.journal;

                        let (reopened, actual) =
                            persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap();

                        assert_eq!(actual, expected);
                        assert_eq!(reopened.load().unwrap(), Some(expected));
                        assert_eq!(database.all().unwrap(), all_before);
                        assert_eq!(database.audit_in_flight_transition().unwrap(), in_flight_before);
                        assert_eq!(database.metadata_provenance(candidate).unwrap(), provenance_before);
                        assert_eq!(fresh_db_invalidation_removal_call_count(), 0);
                    }
                }
            }
        }
    }
    assert_eq!(executions, 48);
}

#[test]
fn startup_fresh_db_invalidation_successor_durable_fresh_handle_reopen_skips_invalidation() {
    let mut executions = 0;
    for historical in [false, true] {
        for origin in FreshDbInvalidationOrigin::ALL {
            for source in Source::THROUGH_FRESH_DB_INVALIDATED {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateResult::ALL {
                        executions += 1;
                        let mut fixture = fixture_for_origin(origin, historical, source, usr_outcome, candidate_outcome);
                        install_persistent_database(&mut fixture, origin);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = effect_authority(&fixture, &journal, &reservation, origin);
                        let expected = expected_fresh_db_invalidated(&fixture, origin);
                        arm_next_update_first_directory_sync_fault();

                        let error =
                            persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap_err();

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
                        let removals_before_reopen = fresh_db_invalidation_removal_call_count();
                        drop(reservation);

                        let candidate = fixture.fixture.fixture.candidate_state;
                        let retained = release_handles(fixture);
                        let fresh = FreshInvalidationHandles::open(retained.path());
                        assert_eq!(fresh.record, expected);
                        let all_before = fresh.database.all().unwrap();
                        let in_flight_before = fresh.database.audit_in_flight_transition().unwrap();
                        let provenance_before = fresh.database.metadata_provenance(candidate).unwrap();
                        let reservation = ActiveStateReservation::acquire().unwrap();

                        assert!(matches!(
                            fresh.capture(&reservation).unwrap(),
                            UsrRollbackFreshDbInvalidationAdmission::NotApplicable
                        ));
                        assert_eq!(fresh.journal.load().unwrap(), Some(expected));
                        assert_eq!(fresh.database.all().unwrap(), all_before);
                        assert_eq!(fresh.database.audit_in_flight_transition().unwrap(), in_flight_before);
                        assert_eq!(fresh.database.metadata_provenance(candidate).unwrap(), provenance_before);
                        assert_eq!(fresh_db_invalidation_removal_call_count(), removals_before_reopen);
                    }
                }
            }
        }
    }
    assert_eq!(executions, 48);
}
