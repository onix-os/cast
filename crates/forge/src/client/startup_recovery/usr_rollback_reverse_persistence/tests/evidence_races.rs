use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::arm_before_usr_rollback_reverse_durable_namespace_capture,
        startup_recovery::{
            UsrRollbackReversePersistenceError, arm_before_usr_rollback_reverse_persistence_final_revalidation,
            persist_usr_rollback_reverse_and_reopen,
        },
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::RollbackActionOutcome,
};

use super::support::{OperationKind, durable_authority, fixture_for_outcome};

#[derive(Clone, Copy, Debug)]
enum EvidenceRace {
    Database,
    Journal,
    Namespace,
}

fn mutate(fixture: &super::support::Fixture, race: EvidenceRace, suffix: &str) {
    match race {
        EvidenceRace::Database => fixture.candidate_transition_clear_hook()(),
        EvidenceRace::Journal => fixture.journal_change_hook()(),
        EvidenceRace::Namespace => fixture.namespace_change_hook(format!("reverse-persistence-{suffix}"))(),
    }
}

fn operation_for_race(race: EvidenceRace) -> OperationKind {
    match race {
        EvidenceRace::Database => OperationKind::NewState,
        EvidenceRace::Journal => OperationKind::Archived,
        EvidenceRace::Namespace => OperationKind::ActiveReblit,
    }
}

#[test]
fn startup_usr_rollback_reverse_persistence_rejects_different_open_and_cross_root_journal_bindings() {
    let fixture = fixture_for_outcome(OperationKind::Archived, RollbackActionOutcome::Applied);
    let first = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = durable_authority(&fixture, &first, &reservation, RollbackActionOutcome::Applied);
    drop(first);
    let second = fixture.open_journal();

    let error = persist_usr_rollback_reverse_and_reopen(second, authority).unwrap_err();

    assert!(matches!(error, UsrRollbackReversePersistenceError::Authority(_)));
    assert_eq!(fixture.fixture.canonical_record(), fixture.record);
    drop(reservation);

    let first_fixture = fixture_for_outcome(OperationKind::Archived, RollbackActionOutcome::AlreadySatisfied);
    let second_fixture = fixture_for_outcome(OperationKind::Archived, RollbackActionOutcome::AlreadySatisfied);
    let first = first_fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = durable_authority(
        &first_fixture,
        &first,
        &reservation,
        RollbackActionOutcome::AlreadySatisfied,
    );
    drop(first);
    fs::write(
        second_fixture
            .fixture
            .installation
            .root
            .join(".cast/journal/state-transition"),
        first_fixture.fixture.canonical_bytes(),
    )
    .unwrap();
    let foreign = second_fixture.open_journal();

    let error = persist_usr_rollback_reverse_and_reopen(foreign, authority).unwrap_err();

    assert!(matches!(error, UsrRollbackReversePersistenceError::Authority(_)));
    assert_eq!(first_fixture.fixture.canonical_record(), first_fixture.record);
    assert_eq!(second_fixture.fixture.canonical_record(), first_fixture.record);
}

#[test]
fn startup_usr_rollback_reverse_persistence_database_journal_and_namespace_changes_never_advance() {
    for race in [EvidenceRace::Database, EvidenceRace::Journal, EvidenceRace::Namespace] {
        for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            let fixture = fixture_for_outcome(operation_for_race(race), outcome);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_retained_exchange_syscall_count();
            let authority = durable_authority(&fixture, &journal, &reservation, outcome);
            let expected_exchange_count = usize::from(outcome == RollbackActionOutcome::Applied);
            mutate(&fixture, race, "before-first-revalidation");

            let error = persist_usr_rollback_reverse_and_reopen(journal, authority).unwrap_err();

            assert!(matches!(error, UsrRollbackReversePersistenceError::Authority(_)));
            assert_eq!(retained_exchange_syscall_count(), expected_exchange_count);
            if !matches!(race, EvidenceRace::Journal) {
                assert_eq!(fixture.fixture.canonical_record(), fixture.record);
            }
        }
    }
}

#[test]
fn startup_usr_rollback_reverse_persistence_final_revalidation_races_fail_before_advance() {
    for race in [EvidenceRace::Database, EvidenceRace::Journal, EvidenceRace::Namespace] {
        for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            let fixture = fixture_for_outcome(operation_for_race(race), outcome);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_retained_exchange_syscall_count();
            let authority = durable_authority(&fixture, &journal, &reservation, outcome);
            let expected_exchange_count = usize::from(outcome == RollbackActionOutcome::Applied);
            let hook: Box<dyn FnOnce()> = match race {
                EvidenceRace::Database => Box::new(fixture.candidate_transition_clear_hook()),
                EvidenceRace::Journal => Box::new(fixture.journal_change_hook()),
                EvidenceRace::Namespace => {
                    let namespace_change =
                        fixture.namespace_change_hook("reverse-persistence-final-revalidation".to_owned());
                    Box::new(move || {
                        arm_before_usr_rollback_reverse_durable_namespace_capture(namespace_change);
                    })
                }
            };
            arm_before_usr_rollback_reverse_persistence_final_revalidation(hook);

            let error = persist_usr_rollback_reverse_and_reopen(journal, authority).unwrap_err();

            assert!(matches!(error, UsrRollbackReversePersistenceError::Authority(_)));
            assert_eq!(retained_exchange_syscall_count(), expected_exchange_count);
            if !matches!(race, EvidenceRace::Journal) {
                assert_eq!(fixture.fixture.canonical_record(), fixture.record);
            }
        }
    }
}
