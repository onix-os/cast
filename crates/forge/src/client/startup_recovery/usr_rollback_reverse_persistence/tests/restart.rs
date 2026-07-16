use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::UsrRollbackReverseAdmission,
        startup_recovery::{
            DurableUsrRollbackReverseRecord, UsrRollbackReversePersistenceError,
            persist_usr_rollback_reverse_and_reopen,
        },
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{
        RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{OperationKind, capture_record, durable_authority, expected_usr_restored, fixture_for_outcome};

#[test]
fn startup_usr_rollback_reverse_persistence_source_fault_restart_finishes_without_second_exchange() {
    for first_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
        let fixture = fixture_for_outcome(OperationKind::ActiveReblit, first_outcome);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_retained_exchange_syscall_count();
        let authority = durable_authority(&fixture, &journal, &reservation, first_outcome);
        let expected_exchange_count = usize::from(first_outcome == RollbackActionOutcome::Applied);
        arm_next_temporary_sync_fault();

        let error = persist_usr_rollback_reverse_and_reopen(journal, authority).unwrap_err();

        assert_temporary_sync_fault_consumed();
        assert!(matches!(
            error,
            UsrRollbackReversePersistenceError::Advance {
                durable: DurableUsrRollbackReverseRecord::Source,
                ..
            }
        ));
        assert_eq!(fixture.fixture.canonical_record(), fixture.record);
        assert_eq!(retained_exchange_syscall_count(), expected_exchange_count);

        drop(reservation);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = durable_authority(
            &fixture,
            &journal,
            &reservation,
            RollbackActionOutcome::AlreadySatisfied,
        );
        let expected = expected_usr_restored(&fixture, RollbackActionOutcome::AlreadySatisfied);
        let (reopened, actual) = persist_usr_rollback_reverse_and_reopen(journal, authority).unwrap();

        assert_eq!(actual, expected);
        assert_eq!(reopened.load().unwrap(), Some(expected));
        assert_eq!(retained_exchange_syscall_count(), expected_exchange_count);
    }
}

#[test]
fn startup_usr_rollback_reverse_persistence_usr_restored_fault_restart_skips_reverse_effect() {
    for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
        let fixture = fixture_for_outcome(OperationKind::Archived, outcome);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_retained_exchange_syscall_count();
        let authority = durable_authority(&fixture, &journal, &reservation, outcome);
        let expected_exchange_count = usize::from(outcome == RollbackActionOutcome::Applied);
        let expected = expected_usr_restored(&fixture, outcome);
        arm_next_update_first_directory_sync_fault();

        let error = persist_usr_rollback_reverse_and_reopen(journal, authority).unwrap_err();

        assert_update_first_directory_sync_fault_consumed();
        assert!(matches!(
            error,
            UsrRollbackReversePersistenceError::Advance {
                durable: DurableUsrRollbackReverseRecord::UsrRestored,
                ..
            }
        ));
        assert_eq!(fixture.fixture.canonical_record(), expected);
        assert_eq!(retained_exchange_syscall_count(), expected_exchange_count);

        drop(reservation);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        assert!(matches!(
            capture_record(&fixture.fixture, &journal, &reservation, &expected),
            UsrRollbackReverseAdmission::NotApplicable
        ));
        assert_eq!(journal.load().unwrap(), Some(expected));
        assert_eq!(retained_exchange_syscall_count(), expected_exchange_count);
    }
}
