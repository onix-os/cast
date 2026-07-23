//! Restart contracts for both durable journal-fault sides.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_post_exchange_durability_events,
            take_active_reblit_candidate_preserve_post_exchange_durability_events,
        },
        startup_recovery::{
            DurableUsrRollbackActiveReblitCandidatePreserveRecord,
            UsrRollbackActiveReblitCandidatePreservePersistenceError,
            persist_usr_rollback_active_reblit_candidate_preserve_and_reopen,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{
    CandidateOrigin, Epoch, Source, capture_record, durable_authority, expected_candidate_preserved,
    expected_post_events, fixture_for_origin, non_journal_namespace_snapshot,
};

#[test]
fn startup_active_reblit_candidate_preserve_persistence_source_fault_restart_finishes_without_second_exchange() {
    let mut exercised = 0;
    for epoch in Epoch::ALL {
        for first_origin in CandidateOrigin::ALL {
            for source in Source::ALL {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                let fixture = fixture_for_origin(epoch, first_origin, source, usr_outcome);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_active_reblit_candidate_preserve_exchange_attempt_count();
                reset_active_reblit_candidate_preserve_post_exchange_durability_events();
                let authority = durable_authority(&fixture, &journal, &reservation, first_origin);
                let first_exchange_count = usize::from(first_origin == CandidateOrigin::Applied);
                let database_before = fixture.fixture.database_snapshot();
                let namespace_before = non_journal_namespace_snapshot(&fixture);
                assert_eq!(
                    active_reblit_candidate_preserve_exchange_attempt_count(),
                    first_exchange_count
                );
                arm_next_temporary_sync_fault();

                let result = persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(journal, authority);
                drop(reservation);
                let error = result.unwrap_err();

                assert_temporary_sync_fault_consumed();
                assert!(matches!(
                    error,
                    UsrRollbackActiveReblitCandidatePreservePersistenceError::Advance {
                        durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source,
                        ..
                    }
                ));
                assert_eq!(fixture.fixture.canonical_record(), fixture.candidate_intent);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_active_reblit_candidate_preserve_post_exchange_durability_events();
                let authority = durable_authority(&fixture, &journal, &reservation, CandidateOrigin::AlreadySatisfied);
                assert_eq!(
                    take_active_reblit_candidate_preserve_post_exchange_durability_events(),
                    expected_post_events(&fixture)
                );
                assert_eq!(
                    active_reblit_candidate_preserve_exchange_attempt_count(),
                    first_exchange_count
                );
                let expected = expected_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);

                let result = persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(journal, authority);
                drop(reservation);
                let (reopened, actual) = result.unwrap();

                assert_eq!(actual, expected);
                assert_eq!(reopened.load().unwrap(), Some(expected));
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
                assert_eq!(
                    active_reblit_candidate_preserve_exchange_attempt_count(),
                    first_exchange_count
                );
                exercised += 1;
                }
            }
        }
    }
    assert_eq!(exercised, 32);
}

#[test]
fn startup_active_reblit_candidate_preserve_persistence_successor_fault_restart_skips_preservation() {
    let mut exercised = 0;
    for epoch in Epoch::ALL {
        for origin in CandidateOrigin::ALL {
            for source in Source::ALL {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                let fixture = fixture_for_origin(epoch, origin, source, usr_outcome);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_active_reblit_candidate_preserve_exchange_attempt_count();
                let authority = durable_authority(&fixture, &journal, &reservation, origin);
                let expected_exchange_count = usize::from(origin == CandidateOrigin::Applied);
                let expected = expected_candidate_preserved(&fixture, origin);
                let database_before = fixture.fixture.database_snapshot();
                let namespace_before = non_journal_namespace_snapshot(&fixture);
                arm_next_update_first_directory_sync_fault();

                let result = persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(journal, authority);
                drop(reservation);
                let error = result.unwrap_err();

                assert_update_first_directory_sync_fault_consumed();
                assert!(matches!(
                    error,
                    UsrRollbackActiveReblitCandidatePreservePersistenceError::Advance {
                        durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,
                        ..
                    }
                ));
                assert_eq!(fixture.fixture.canonical_record(), expected);
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_eq!(
                    active_reblit_candidate_preserve_exchange_attempt_count(),
                    expected_exchange_count
                );
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let not_applicable = matches!(
                    capture_record(&fixture.fixture, &journal, &reservation, &expected),
                    UsrRollbackCandidatePreserveAdmission::NotApplicable
                );
                drop(reservation);
                assert!(not_applicable);
                assert_eq!(journal.load().unwrap(), Some(expected));
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
                assert_eq!(
                    active_reblit_candidate_preserve_exchange_attempt_count(),
                    expected_exchange_count
                );
                exercised += 1;
                }
            }
        }
    }
    assert_eq!(exercised, 32);
}
