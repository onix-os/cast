use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation, startup_recovery::persist_usr_rollback_reverse_and_reopen,
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::support::{
    OperationKind, durable_authority, expected_usr_restored, fixture_for_outcome, non_journal_namespace_snapshot,
};

fn exercise_success_matrix(outcome: RollbackActionOutcome) {
    for kind in OperationKind::ALL {
        let fixture = fixture_for_outcome(kind, outcome);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_retained_exchange_syscall_count();
        let authority = durable_authority(&fixture, &journal, &reservation, outcome);
        let expected = expected_usr_restored(&fixture, outcome);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = non_journal_namespace_snapshot(&fixture);

        let (reopened, actual) = persist_usr_rollback_reverse_and_reopen(journal, authority).unwrap();

        assert_eq!(actual, expected, "{kind:?} {outcome:?}");
        assert_eq!(actual.phase, Phase::UsrRestored, "{kind:?} {outcome:?}");
        assert_eq!(reopened.load().unwrap(), Some(expected.clone()), "{kind:?} {outcome:?}");
        assert_eq!(fixture.fixture.canonical_record(), expected, "{kind:?} {outcome:?}");
        assert_eq!(
            fixture.fixture.database_snapshot(),
            database_before,
            "{kind:?} {outcome:?}"
        );
        assert_eq!(
            non_journal_namespace_snapshot(&fixture),
            namespace_before,
            "{kind:?} {outcome:?}"
        );
        assert_eq!(
            retained_exchange_syscall_count(),
            usize::from(outcome == RollbackActionOutcome::Applied),
            "{kind:?} {outcome:?}"
        );
    }
}

#[test]
fn startup_usr_rollback_reverse_persistence_applied_matrix_persists_exact_usr_restored() {
    exercise_success_matrix(RollbackActionOutcome::Applied);
}

#[test]
fn startup_usr_rollback_reverse_persistence_already_satisfied_matrix_persists_exact_usr_restored() {
    exercise_success_matrix(RollbackActionOutcome::AlreadySatisfied);
}

#[test]
fn startup_usr_rollback_reverse_persistence_changes_only_the_canonical_journal() {
    let fixture = fixture_for_outcome(OperationKind::ActiveReblit, RollbackActionOutcome::Applied);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = durable_authority(&fixture, &journal, &reservation, RollbackActionOutcome::Applied);
    let canonical_before = fixture.fixture.canonical_bytes();
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = non_journal_namespace_snapshot(&fixture);

    let (reopened, actual) = persist_usr_rollback_reverse_and_reopen(journal, authority).unwrap();

    assert_ne!(fixture.fixture.canonical_bytes(), canonical_before);
    assert_eq!(reopened.load().unwrap(), Some(actual));
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
}
