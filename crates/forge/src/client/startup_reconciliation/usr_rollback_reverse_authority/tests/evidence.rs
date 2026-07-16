//! Focused retained-evidence revalidation contracts.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackReverseAdmission, arm_before_usr_rollback_reverse_fresh_namespace_capture,
            arm_between_usr_rollback_reverse_database_captures,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::{
    fixture::{OperationKind, create_private_directory},
    support::{ReverseFixture, ReverseLayout},
};

#[test]
fn startup_usr_rollback_reverse_rejects_a_different_open_journal_binding() {
    for layout in [ReverseLayout::Post, ReverseLayout::Pre] {
        let fixture = ReverseFixture::new(OperationKind::Archived, layout);
        let first = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let admission = fixture.capture(&first, &reservation);
        drop(first);
        let second = fixture.open_journal();
        match admission {
            UsrRollbackReverseAdmission::Apply(authority) => {
                assert!(authority.revalidate(&second).is_err());
            }
            UsrRollbackReverseAdmission::Finish(authority) => {
                assert!(authority.revalidate(&second).is_err());
            }
            _ => panic!("exact {layout:?} evidence was not admitted"),
        }
    }
}

#[test]
fn startup_usr_rollback_reverse_database_and_provenance_changes_invalidate_authority() {
    let fixture = ReverseFixture::new(OperationKind::NewState, ReverseLayout::Post);
    let before = fixture.fixture.canonical_bytes();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackReverseAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("POST evidence did not admit apply authority");
    };
    fixture
        .fixture
        .database
        .clear_transition_if_matches(fixture.fixture.candidate_state, &fixture.reverse_intent.transition_id)
        .unwrap();
    assert!(authority.revalidate(&journal).is_err());
    assert_eq!(fixture.fixture.canonical_bytes(), before);
    drop(reservation);
    drop(journal);

    let fixture = ReverseFixture::new(OperationKind::Archived, ReverseLayout::Pre);
    let before = fixture.fixture.canonical_bytes();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackReverseAdmission::Finish(authority) = fixture.capture(&journal, &reservation) else {
        panic!("PRE evidence did not admit finish authority");
    };
    fixture
        .fixture
        .database
        .delete_metadata_provenance_for_test(fixture.fixture.candidate_state)
        .unwrap();
    assert!(authority.revalidate(&journal).is_err());
    assert_eq!(fixture.fixture.canonical_bytes(), before);
}

#[test]
fn startup_usr_rollback_reverse_namespace_and_journal_changes_invalidate_authority() {
    let fixture = ReverseFixture::new(OperationKind::Archived, ReverseLayout::Post);
    let before = fixture.fixture.canonical_bytes();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackReverseAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("POST evidence did not admit apply authority");
    };
    create_private_directory(
        &fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join("rollback-reverse-evidence-change"),
    );
    assert!(authority.revalidate(&journal).is_err());
    assert_eq!(fixture.fixture.canonical_bytes(), before);
    drop(reservation);
    drop(journal);

    let fixture = ReverseFixture::new(OperationKind::ActiveReblit, ReverseLayout::Pre);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackReverseAdmission::Finish(authority) = fixture.capture(&journal, &reservation) else {
        panic!("PRE evidence did not admit finish authority");
    };
    let restored = fixture
        .reverse_intent
        .rollback_successor(Some(RollbackActionOutcome::Applied))
        .unwrap();
    journal.advance(&fixture.reverse_intent, &restored).unwrap();
    assert!(authority.revalidate(&journal).is_err());
}

#[test]
fn startup_usr_rollback_reverse_capture_races_defer_without_authority() {
    let fixture = ReverseFixture::new(OperationKind::NewState, ReverseLayout::Post);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    let transition = fixture.reverse_intent.transition_id.clone();
    arm_between_usr_rollback_reverse_database_captures(move || {
        database.clear_transition_if_matches(candidate, &transition).unwrap();
    });
    assert!(matches!(
        fixture.capture(&journal, &reservation),
        UsrRollbackReverseAdmission::Deferred
    ));
    drop(reservation);
    drop(journal);

    let fixture = ReverseFixture::new(OperationKind::Archived, ReverseLayout::Pre);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let inserted = fixture
        .fixture
        .installation
        .state_quarantine_dir()
        .join("rollback-reverse-capture-race");
    arm_between_usr_rollback_reverse_database_captures(move || {
        create_private_directory(&inserted);
    });
    assert!(matches!(
        fixture.capture(&journal, &reservation),
        UsrRollbackReverseAdmission::Deferred
    ));
}

#[test]
fn startup_usr_rollback_reverse_fresh_namespace_race_fails_revalidation() {
    for layout in [ReverseLayout::Post, ReverseLayout::Pre] {
        let fixture = ReverseFixture::new(OperationKind::Archived, layout);
        let before = fixture.fixture.canonical_bytes();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let admission = fixture.capture(&journal, &reservation);
        let inserted = fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join(format!("rollback-reverse-fresh-race-{layout:?}"));
        arm_before_usr_rollback_reverse_fresh_namespace_capture(move || {
            create_private_directory(&inserted);
        });
        match admission {
            UsrRollbackReverseAdmission::Apply(authority) => {
                assert!(authority.revalidate(&journal).is_err());
            }
            UsrRollbackReverseAdmission::Finish(authority) => {
                assert!(authority.revalidate(&journal).is_err());
            }
            _ => panic!("exact {layout:?} evidence was not admitted"),
        }
        assert_eq!(fixture.fixture.canonical_bytes(), before);
    }
}
