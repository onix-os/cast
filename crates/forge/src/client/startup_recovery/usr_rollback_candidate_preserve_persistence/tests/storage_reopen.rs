use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count,
        },
        startup_recovery::{
            DurableUsrRollbackCandidatePreserveRecord, UsrRollbackCandidatePreservePersistenceError,
            persist_usr_rollback_candidate_preserve_and_reopen,
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
    CandidateOrigin, Source, durable_authority, expected_candidate_preserved, fixture_for_origin_at_epoch,
    non_journal_namespace_snapshot,
};

#[test]
fn startup_usr_rollback_candidate_preserve_persistence_faults_reopen_exact_source_or_successor() {
    let cases: [(fn(), fn(), DurableUsrRollbackCandidatePreserveRecord); 5] = [
        (
            arm_next_temporary_sync_fault,
            assert_temporary_sync_fault_consumed,
            DurableUsrRollbackCandidatePreserveRecord::Source,
        ),
        (
            arm_next_update_exchange_fault,
            assert_update_exchange_fault_consumed,
            DurableUsrRollbackCandidatePreserveRecord::Source,
        ),
        (
            arm_next_update_first_directory_sync_fault,
            assert_update_first_directory_sync_fault_consumed,
            DurableUsrRollbackCandidatePreserveRecord::CandidatePreserved,
        ),
        (
            arm_next_displaced_unlink_fault,
            assert_displaced_unlink_fault_consumed,
            DurableUsrRollbackCandidatePreserveRecord::CandidatePreserved,
        ),
        (
            arm_next_update_final_directory_sync_fault,
            assert_update_final_directory_sync_fault_consumed,
            DurableUsrRollbackCandidatePreserveRecord::CandidatePreserved,
        ),
    ];

    for historical in [false, true] {
        for origin in CandidateOrigin::ALL {
            for source in Source::THROUGH_CANDIDATE_PRESERVED {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for (arm, assert_consumed, expected_durable) in cases {
                        let fixture = fixture_for_origin_at_epoch(historical, origin, source, usr_outcome);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        reset_new_state_candidate_preserve_move_attempt_count();
                        let authority = durable_authority(&fixture, &journal, &reservation, origin);
                        let effect_count_before = new_state_candidate_preserve_move_attempt_count();
                        assert_eq!(effect_count_before, usize::from(origin == CandidateOrigin::Applied));
                        let database_before = fixture.fixture.database_snapshot();
                        let namespace_before = non_journal_namespace_snapshot(&fixture);
                        let successor = expected_candidate_preserved(&fixture, origin);
                        arm();

                        let error =
                            persist_usr_rollback_candidate_preserve_and_reopen(journal, authority).unwrap_err();

                        assert_consumed();
                        assert!(matches!(
                            error,
                            UsrRollbackCandidatePreservePersistenceError::Advance { durable, .. }
                                if durable == expected_durable
                        ));
                        match expected_durable {
                            DurableUsrRollbackCandidatePreserveRecord::Source => {
                                assert_eq!(fixture.fixture.canonical_record(), fixture.candidate_intent)
                            }
                            DurableUsrRollbackCandidatePreserveRecord::CandidatePreserved => {
                                assert_eq!(fixture.fixture.canonical_record(), successor)
                            }
                        }
                        let names = fs::read_dir(fixture.fixture.installation.root.join(".cast/journal"))
                            .unwrap()
                            .map(|entry| entry.unwrap().file_name())
                            .collect::<Vec<_>>();
                        assert_eq!(names.len(), 2, "stale journal residue remained after reopen: {names:?}");
                        assert_eq!(fixture.fixture.database_snapshot(), database_before);
                        assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
                        assert_eq!(new_state_candidate_preserve_move_attempt_count(), effect_count_before);
                    }
                }
            }
        }
    }
}

#[test]
fn startup_usr_rollback_candidate_preserve_persistence_consumes_old_store_and_reopens_exact_success() {
    for historical in [false, true] {
        for origin in CandidateOrigin::ALL {
            let fixture =
                fixture_for_origin_at_epoch(historical, origin, Source::Intent, RollbackActionOutcome::Applied);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_new_state_candidate_preserve_move_attempt_count();
            let authority = durable_authority(&fixture, &journal, &reservation, origin);
            let effect_count_before = new_state_candidate_preserve_move_attempt_count();
            assert_eq!(effect_count_before, usize::from(origin == CandidateOrigin::Applied));
            let database_before = fixture.fixture.database_snapshot();
            let namespace_before = non_journal_namespace_snapshot(&fixture);
            let expected = expected_candidate_preserved(&fixture, origin);

            let (reopened, actual) =
                persist_usr_rollback_candidate_preserve_and_reopen(journal, authority).unwrap();

            assert_eq!(actual, expected);
            assert_eq!(reopened.load().unwrap(), Some(expected.clone()));
            drop(reopened);
            let cast = fixture.fixture.installation.retained_mutable_cast_directory().unwrap();
            let independent =
                TransitionJournalStore::try_open_in_retained_cast(cast, &fixture.fixture.installation.root).unwrap();
            assert_eq!(independent.load().unwrap(), Some(expected));
            assert_eq!(fixture.fixture.database_snapshot(), database_before);
            assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
            assert_eq!(new_state_candidate_preserve_move_attempt_count(), effect_count_before);
        }
    }
}
