use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            DurableUsrRollbackReverseRecord, UsrRollbackReversePersistenceError,
            persist_usr_rollback_reverse_and_reopen,
        },
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{
        RollbackActionOutcome, TransitionJournalStore, arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{OperationKind, durable_authority, expected_usr_restored, fixture_for_outcome};

#[test]
fn startup_usr_rollback_reverse_persistence_storage_faults_reopen_to_exact_source_or_usr_restored() {
    let cases: [(fn(), fn(), DurableUsrRollbackReverseRecord); 5] = [
        (
            arm_next_temporary_sync_fault,
            assert_temporary_sync_fault_consumed,
            DurableUsrRollbackReverseRecord::Source,
        ),
        (
            arm_next_update_exchange_fault,
            assert_update_exchange_fault_consumed,
            DurableUsrRollbackReverseRecord::Source,
        ),
        (
            arm_next_update_first_directory_sync_fault,
            assert_update_first_directory_sync_fault_consumed,
            DurableUsrRollbackReverseRecord::UsrRestored,
        ),
        (
            arm_next_displaced_unlink_fault,
            assert_displaced_unlink_fault_consumed,
            DurableUsrRollbackReverseRecord::UsrRestored,
        ),
        (
            arm_next_update_final_directory_sync_fault,
            assert_update_final_directory_sync_fault_consumed,
            DurableUsrRollbackReverseRecord::UsrRestored,
        ),
    ];

    for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
        for (arm, assert_consumed, expected_durable) in cases {
            let fixture = fixture_for_outcome(OperationKind::NewState, outcome);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_retained_exchange_syscall_count();
            let authority = durable_authority(&fixture, &journal, &reservation, outcome);
            let expected_exchange_count = usize::from(outcome == RollbackActionOutcome::Applied);
            let expected_usr_restored = expected_usr_restored(&fixture, outcome);
            arm();

            let error = persist_usr_rollback_reverse_and_reopen(journal, authority).unwrap_err();

            assert_consumed();
            assert!(matches!(
                error,
                UsrRollbackReversePersistenceError::Advance { durable, .. }
                    if durable == expected_durable
            ));
            match expected_durable {
                DurableUsrRollbackReverseRecord::Source => {
                    assert_eq!(fixture.fixture.canonical_record(), fixture.record)
                }
                DurableUsrRollbackReverseRecord::UsrRestored => {
                    assert_eq!(fixture.fixture.canonical_record(), expected_usr_restored)
                }
            }
            assert_eq!(retained_exchange_syscall_count(), expected_exchange_count);
            let names = fs::read_dir(fixture.fixture.installation.root.join(".cast/journal"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            assert_eq!(names.len(), 2, "stale journal residue remained after reopen: {names:?}");
        }
    }
}

#[test]
fn startup_usr_rollback_reverse_persistence_consumes_old_journal_and_reopens_exact_success() {
    for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
        let fixture = fixture_for_outcome(OperationKind::Archived, outcome);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = durable_authority(&fixture, &journal, &reservation, outcome);
        let expected = expected_usr_restored(&fixture, outcome);

        let (reopened, actual) = persist_usr_rollback_reverse_and_reopen(journal, authority).unwrap();

        assert_eq!(actual, expected);
        assert_eq!(reopened.load().unwrap(), Some(expected.clone()));
        drop(reopened);
        let cast = fixture.fixture.installation.retained_mutable_cast_directory().unwrap();
        let independent =
            TransitionJournalStore::try_open_in_retained_cast(cast, &fixture.fixture.installation.root).unwrap();
        assert_eq!(independent.load().unwrap(), Some(expected));
    }
}
