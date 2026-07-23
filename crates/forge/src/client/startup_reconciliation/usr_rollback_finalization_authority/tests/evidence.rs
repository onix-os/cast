use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackFinalizationAdmission, arm_before_usr_rollback_finalization_fresh_namespace_capture,
            arm_between_usr_rollback_finalization_database_captures,
        },
    },
    transition_journal::{RollbackActionOutcome, encode},
};

use super::support::{
    CandidateResult, FinalizationFixture, FreshDbOutcome, Source, canonical_journal, create_private_directory,
    transition_quarantine_path,
};

#[test]
fn startup_usr_rollback_finalization_capture_sandwich_rejects_database_and_namespace_changes() {
    for namespace_change in [false, true] {
        let fixture = FinalizationFixture::new(
            FreshDbOutcome::Applied,
            Source::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateResult::Applied,
        );
        let canonical = fixture.canonical_bytes();
        if namespace_change {
            let target = transition_quarantine_path(&fixture.fixture.fixture.fixture, &fixture.record);
            arm_between_usr_rollback_finalization_database_captures(move || {
                fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap();
            });
        } else {
            let database = fixture.fixture.fixture.fixture.database.clone();
            let previous = fixture.fixture.fixture.fixture.previous_state;
            arm_between_usr_rollback_finalization_database_captures(move || {
                database.remove(&previous).unwrap();
            });
        }
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let admission = fixture.capture(&journal, &reservation);
        assert!(
            admission.is_err() || matches!(admission.unwrap(), UsrRollbackFinalizationAdmission::Deferred),
            "namespace_change={namespace_change}"
        );
        assert_eq!(fixture.canonical_bytes(), canonical);
        assert_eq!(fixture.canonical_record(), fixture.record);
        fixture.fixture.assert_exact_joint_absence();
    }
}

#[test]
fn startup_usr_rollback_finalization_revalidation_rejects_reopened_and_changed_authority() {
    let fixture = FinalizationFixture::new(
        FreshDbOutcome::AlreadySatisfied,
        Source::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateResult::AlreadySatisfied,
    );
    let canonical = fixture.canonical_bytes();
    let namespace = fixture.namespace_snapshot();
    let first = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&first, &reservation);
    drop(first);
    let reopened = fixture.open_journal();
    assert!(authority.revalidate(&reopened).is_err());
    fixture.assert_terminal_unchanged(&canonical, &namespace);
    drop(authority);
    drop(reopened);
    drop(reservation);

    for race in 0..4 {
        let fixture = FinalizationFixture::new(
            FreshDbOutcome::Applied,
            Source::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateResult::Applied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        match race {
            0 => {
                let database = fixture.fixture.fixture.fixture.database.clone();
                let previous = fixture.fixture.fixture.fixture.previous_state;
                database.remove(&previous).unwrap();
            }
            1 => fs::write(
                canonical_journal(&fixture.fixture.fixture.fixture.installation.root),
                encode(&fixture.fixture.record).unwrap(),
            )
            .unwrap(),
            2 => {
                let target = transition_quarantine_path(&fixture.fixture.fixture.fixture, &fixture.record);
                arm_before_usr_rollback_finalization_fresh_namespace_capture(move || {
                    fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap();
                });
            }
            3 => {
                let cast = fixture.fixture.fixture.fixture.installation.root.join(".cast");
                let displaced = fixture
                    .fixture
                    .fixture
                    .fixture
                    .installation
                    .root
                    .join(".cast-rollback-finalization-rebound");
                fs::rename(&cast, displaced).unwrap();
                fs::create_dir(&cast).unwrap();
                fs::set_permissions(cast, fs::Permissions::from_mode(0o700)).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(authority.revalidate(&journal).is_err(), "race={race}");
        assert!(
            canonical_journal(&fixture.fixture.fixture.fixture.installation.root).exists() || race == 3,
            "authority revalidation must never delete the journal: race={race}"
        );
        fixture.fixture.assert_exact_joint_absence();
    }
}

#[test]
fn startup_usr_rollback_finalization_refuses_terminal_namespace_lookalikes() {
    for case in 0..3 {
        let fixture = FinalizationFixture::new(
            FreshDbOutcome::AlreadySatisfied,
            Source::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateResult::AlreadySatisfied,
        );
        let target = transition_quarantine_path(&fixture.fixture.fixture.fixture, &fixture.record);
        match case {
            0 => fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap(),
            1 => fs::rename(
                &target,
                fixture
                    .fixture
                    .fixture
                    .fixture
                    .installation
                    .state_quarantine_dir()
                    .join("displaced-rollback-finalization-target"),
            )
            .unwrap(),
            2 => create_private_directory(
                &fixture
                    .fixture
                    .fixture
                    .fixture
                    .installation
                    .root
                    .join(".cast/root")
                    .join(fixture.fixture.fixture.fixture.candidate_state.to_string()),
            ),
            _ => unreachable!(),
        }
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        assert!(matches!(
            fixture.capture(&journal, &reservation).unwrap(),
            UsrRollbackFinalizationAdmission::Deferred
        ));
        assert_eq!(fixture.canonical_record(), fixture.record);
        fixture.fixture.assert_exact_joint_absence();
    }
}
