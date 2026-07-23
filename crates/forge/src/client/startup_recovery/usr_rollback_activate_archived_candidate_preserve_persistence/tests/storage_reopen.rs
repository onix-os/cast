//! Journal fault classification and canonical reopen contracts.

use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            archived_candidate_preserve_move_attempt_count, reset_archived_candidate_preserve_move_attempt_count,
        },
        startup_recovery::{
            DurableUsrRollbackArchivedCandidatePreserveRecord, UsrRollbackArchivedCandidatePreservePersistenceError,
            persist_usr_rollback_archived_candidate_preserve_and_reopen,
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

use super::super::candidate_test_support::CandidateSource;
use super::support::{
    CandidateOrigin, Epoch, assert_preserved, durable_authority, expected_candidate_preserved, fixture_for_origin,
    non_journal_namespace_snapshot,
};

#[test]
fn startup_archived_candidate_preserve_persistence_faults_reopen_exact_source_or_successor() {
    let cases: [(fn(), fn(), DurableUsrRollbackArchivedCandidatePreserveRecord); 5] = [
        (
            arm_next_temporary_sync_fault,
            assert_temporary_sync_fault_consumed,
            DurableUsrRollbackArchivedCandidatePreserveRecord::Source,
        ),
        (
            arm_next_update_exchange_fault,
            assert_update_exchange_fault_consumed,
            DurableUsrRollbackArchivedCandidatePreserveRecord::Source,
        ),
        (
            arm_next_update_first_directory_sync_fault,
            assert_update_first_directory_sync_fault_consumed,
            DurableUsrRollbackArchivedCandidatePreserveRecord::CandidatePreserved,
        ),
        (
            arm_next_displaced_unlink_fault,
            assert_displaced_unlink_fault_consumed,
            DurableUsrRollbackArchivedCandidatePreserveRecord::CandidatePreserved,
        ),
        (
            arm_next_update_final_directory_sync_fault,
            assert_update_final_directory_sync_fault_consumed,
            DurableUsrRollbackArchivedCandidatePreserveRecord::CandidatePreserved,
        ),
    ];

    for epoch in Epoch::ALL {
        for origin in CandidateOrigin::ALL {
            for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for (arm, assert_consumed, expected_durable) in cases {
                        let fixture = fixture_for_origin(epoch, origin, source, usr_outcome);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        reset_archived_candidate_preserve_move_attempt_count();
                        let authority = durable_authority(&fixture, &journal, &reservation, origin);
                        let effect_count_before = archived_candidate_preserve_move_attempt_count();
                        assert_eq!(effect_count_before, usize::from(origin == CandidateOrigin::Applied));
                        let database_before = fixture.fixture.database_snapshot();
                        let namespace_before = non_journal_namespace_snapshot(&fixture);
                        let successor = expected_candidate_preserved(&fixture, origin);
                        arm();

                        let result = persist_usr_rollback_archived_candidate_preserve_and_reopen(journal, authority);
                        drop(reservation);
                        let error = result.unwrap_err();

                        assert_consumed();
                        assert!(matches!(
                            error,
                            UsrRollbackArchivedCandidatePreservePersistenceError::Advance { durable, .. }
                                if durable == expected_durable
                        ));
                        match expected_durable {
                            DurableUsrRollbackArchivedCandidatePreserveRecord::Source => {
                                assert_eq!(fixture.fixture.canonical_record(), fixture.candidate_intent)
                            }
                            DurableUsrRollbackArchivedCandidatePreserveRecord::CandidatePreserved => {
                                assert_eq!(fixture.fixture.canonical_record(), successor)
                            }
                        }
                        assert_eq!(fixture.fixture.database_snapshot(), database_before);
                        assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
                        assert_eq!(archived_candidate_preserve_move_attempt_count(), effect_count_before);
                        assert_preserved(&fixture);
                        let names = fs::read_dir(fixture.fixture.installation.root.join(".cast/journal"))
                            .unwrap()
                            .map(|entry| entry.unwrap().file_name())
                            .collect::<Vec<_>>();
                        assert_eq!(names.len(), 2, "stale journal residue remained: {names:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn startup_archived_candidate_preserve_persistence_consumes_old_store_and_reopens_success() {
    for epoch in Epoch::ALL {
        for origin in CandidateOrigin::ALL {
            let fixture = fixture_for_origin(epoch, origin, CandidateSource::Intent, RollbackActionOutcome::Applied);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_archived_candidate_preserve_move_attempt_count();
            let authority = durable_authority(&fixture, &journal, &reservation, origin);
            let effect_count_before = archived_candidate_preserve_move_attempt_count();
            assert_eq!(effect_count_before, usize::from(origin == CandidateOrigin::Applied));
            let database_before = fixture.fixture.database_snapshot();
            let namespace_before = non_journal_namespace_snapshot(&fixture);
            let expected = expected_candidate_preserved(&fixture, origin);

            let result = persist_usr_rollback_archived_candidate_preserve_and_reopen(journal, authority);
            drop(reservation);
            let (reopened, actual) = result.unwrap();

            assert_eq!(actual, expected);
            assert_eq!(reopened.load().unwrap(), Some(expected.clone()));
            drop(reopened);
            let cast = fixture.fixture.installation.retained_mutable_cast_directory().unwrap();
            let independent =
                TransitionJournalStore::try_open_in_retained_cast(cast, &fixture.fixture.installation.root).unwrap();
            assert_eq!(independent.load().unwrap(), Some(expected));
            assert_eq!(fixture.fixture.database_snapshot(), database_before);
            assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
            assert_eq!(archived_candidate_preserve_move_attempt_count(), effect_count_before);
            assert_preserved(&fixture);
        }
    }
}
