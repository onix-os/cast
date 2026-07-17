//! Focused retained-evidence and race contracts.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, arm_before_usr_rollback_candidate_preserve_fresh_namespace_capture,
            arm_between_usr_rollback_candidate_preserve_database_captures,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::{
    fixture::{OperationKind, create_private_directory},
    support::{CandidateLayout, CandidatePreserveFixture, CandidateSource, reserved_active_reblit_wrapper_path},
};

#[test]
fn startup_candidate_preserve_rejects_a_different_open_journal_binding() {
    for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
        let fixture = CandidatePreserveFixture::new(
            OperationKind::Archived,
            CandidateSource::Intent,
            RollbackActionOutcome::Applied,
            layout,
        );
        let first = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let admission = fixture.capture(&first, &reservation);
        drop(first);
        let second = fixture.open_journal();
        match admission {
            UsrRollbackCandidatePreserveAdmission::Apply(authority) => {
                assert!(authority.revalidate(&second).is_err());
            }
            UsrRollbackCandidatePreserveAdmission::Finish(authority) => {
                assert!(authority.revalidate(&second).is_err());
            }
            _ => panic!("exact {layout:?} evidence was not admitted"),
        }
    }
}

#[test]
fn startup_candidate_preserve_database_and_provenance_changes_invalidate_authority() {
    let fixture = CandidatePreserveFixture::new(
        OperationKind::NewState,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateLayout::Staged,
    );
    let before = fixture.fixture.canonical_bytes();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("staged NewState evidence did not admit");
    };
    fixture
        .fixture
        .database
        .clear_transition_if_matches(fixture.fixture.candidate_state, &fixture.candidate_intent.transition_id)
        .unwrap();
    assert!(authority.revalidate(&journal).is_err());
    assert_eq!(fixture.fixture.canonical_bytes(), before);
    drop(reservation);
    drop(journal);

    let fixture = CandidatePreserveFixture::new(
        OperationKind::Archived,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateLayout::Preserved,
    );
    let before = fixture.fixture.canonical_bytes();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Finish(authority) = fixture.capture(&journal, &reservation) else {
        panic!("preserved archived evidence did not admit");
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
fn startup_candidate_preserve_namespace_changes_invalidate_authority() {
    for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
        let fixture = CandidatePreserveFixture::new(
            OperationKind::ActiveReblit,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            layout,
        );
        let before = fixture.fixture.canonical_bytes();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let admission = fixture.capture(&journal, &reservation);
        create_private_directory(
            &fixture
                .fixture
                .installation
                .state_quarantine_dir()
                .join(format!("candidate-preserve-change-{layout:?}")),
        );
        match admission {
            UsrRollbackCandidatePreserveAdmission::Apply(authority) => {
                assert!(authority.revalidate(&journal).is_err());
            }
            UsrRollbackCandidatePreserveAdmission::Finish(authority) => {
                assert!(authority.revalidate(&journal).is_err());
            }
            _ => panic!("exact {layout:?} evidence was not admitted"),
        }
        assert_eq!(fixture.fixture.canonical_bytes(), before);
    }
}

#[test]
fn startup_candidate_preserve_capture_races_defer_without_authority() {
    let fixture = CandidatePreserveFixture::new(
        OperationKind::NewState,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        CandidateLayout::Staged,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let before = fixture.evidence_snapshots();
    arm_between_usr_rollback_candidate_preserve_database_captures(fixture.candidate_transition_clear_hook());
    assert!(matches!(
        fixture.capture(&journal, &reservation),
        UsrRollbackCandidatePreserveAdmission::Deferred
    ));
    assert_eq!(fixture.fixture.canonical_bytes(), before.0);
    assert_eq!(fixture.fixture.namespace_snapshot(), before.2);
    drop(reservation);
    drop(journal);

    let fixture = CandidatePreserveFixture::new(
        OperationKind::Archived,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateLayout::Preserved,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    arm_between_usr_rollback_candidate_preserve_database_captures(
        fixture.namespace_change_hook("candidate-preserve-capture-race".to_owned()),
    );
    assert!(matches!(
        fixture.capture(&journal, &reservation),
        UsrRollbackCandidatePreserveAdmission::Deferred
    ));
}

#[test]
fn startup_candidate_preserve_fresh_namespace_race_fails_revalidation() {
    for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
        let fixture = CandidatePreserveFixture::new(
            OperationKind::Archived,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            layout,
        );
        let before = fixture.fixture.canonical_bytes();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let admission = fixture.capture(&journal, &reservation);
        arm_before_usr_rollback_candidate_preserve_fresh_namespace_capture(
            fixture.namespace_change_hook(format!("candidate-preserve-fresh-race-{layout:?}")),
        );
        match admission {
            UsrRollbackCandidatePreserveAdmission::Apply(authority) => {
                assert!(authority.revalidate(&journal).is_err());
            }
            UsrRollbackCandidatePreserveAdmission::Finish(authority) => {
                assert!(authority.revalidate(&journal).is_err());
            }
            _ => panic!("exact {layout:?} evidence was not admitted"),
        }
        assert_eq!(fixture.fixture.canonical_bytes(), before);
    }

    for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
        let fixture = CandidatePreserveFixture::new(
            OperationKind::ActiveReblit,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            layout,
        );
        let reserved = reserved_active_reblit_wrapper_path(&fixture, layout);
        let changed = reserved.clone();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let admission = fixture.capture(&journal, &reservation);
        arm_before_usr_rollback_candidate_preserve_fresh_namespace_capture(move || {
            fs::set_permissions(changed, fs::Permissions::from_mode(0o755)).unwrap();
        });
        match admission {
            UsrRollbackCandidatePreserveAdmission::Apply(authority) => {
                assert!(authority.revalidate(&journal).is_err());
            }
            UsrRollbackCandidatePreserveAdmission::Finish(authority) => {
                assert!(authority.revalidate(&journal).is_err());
            }
            _ => panic!("exact {layout:?} ActiveReblit evidence was not admitted"),
        }
        assert_eq!(fs::metadata(reserved).unwrap().permissions().mode() & 0o7777, 0o755);
        fixture.assert_non_namespace_unchanged();
    }
}
