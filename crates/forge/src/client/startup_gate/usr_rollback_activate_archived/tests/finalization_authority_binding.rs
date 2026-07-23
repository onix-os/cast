//! Direct negative proof for authority/store pairings startup cannot construct.

use std::{
    fs,
    io::Write as _,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackActivateArchivedFinalizationSeal,
        startup_reconciliation::{
            arm_between_usr_rollback_activate_archived_finalization_database_captures,
            UsrRollbackActivateArchivedFinalizationAdmission, UsrRollbackActivateArchivedFinalizationAuthority,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOutcome, Epoch, RouteFixture, assert_pending_phase, candidate_move_count, enter_route,
        persist_rollback_complete, reset_candidate_observers,
    },
};

#[test]
fn startup_activate_archived_finalization_authority_covers_both_epochs_and_rejects_wrong_bindings() {
    for epoch in Epoch::ALL {
        let fixture = RouteFixture::new(
            epoch,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateOutcome::Applied,
        );
        let terminal = persist_rollback_complete(&fixture);
        let other = RouteFixture::new(
            Epoch::Historical,
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOutcome::AlreadySatisfied,
        );
        let _other_terminal = persist_rollback_complete(&other);
        let database_before = fixture.database_snapshot();
        let namespace_before = fixture.namespace_snapshot();
        let other_database = other.database_snapshot();
        let other_namespace = other.namespace_snapshot();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_candidate_observers();

        fixture.assert_exact_database_pair();
        fixture.assert_exact_archived_topology();
        let authority = super::support::capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
        authority.revalidate(&journal).unwrap();
        drop(journal);

        let reopened = fixture.open_journal();
        let reopened_error = authority.revalidate(&reopened).unwrap_err();
        assert_eq!(
            reopened_error.to_string(),
            "ActivateArchived rollback-finalization authority was paired with a different open journal store"
        );
        drop(reopened);

        let other_journal = other.open_journal();
        let cross_root_error = authority.revalidate(&other_journal).unwrap_err();
        assert_eq!(
            cross_root_error.to_string(),
            "ActivateArchived rollback-finalization authority was paired with a different open journal store"
        );
        assert_eq!(fixture.canonical_record(), terminal);
        assert_eq!(fixture.database_snapshot(), database_before);
        assert_eq!(fixture.namespace_snapshot(), namespace_before);
        assert_eq!(other.database_snapshot(), other_database);
        assert_eq!(other.namespace_snapshot(), other_namespace);
        assert_eq!(candidate_move_count(), 0);
    }
}

#[test]
fn startup_activate_archived_finalization_binding_rejects_same_bytes_on_a_different_inode() {
    for during_capture in [true, false] {
        let fixture = RouteFixture::new(
            Epoch::Current,
            CandidateSource::RootLinksComplete,
            RollbackActionOutcome::Applied,
            CandidateOutcome::AlreadySatisfied,
        );
        let terminal = persist_rollback_complete(&fixture);
        assert_eq!(terminal.generation, 12);
        let canonical = fixture
            .fixture
            .fixture
            .installation
            .root
            .join(".cast/journal/state-transition");
        let displaced = fixture
            .fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join("state-transition-original-inode");
        let bytes = fs::read(&canonical).unwrap();
        let hook_canonical = canonical.clone();
        let hook_displaced = displaced.clone();
        let hook_bytes = bytes.clone();
        let replace = move || replace_with_same_bytes(&hook_canonical, &hook_displaced, &hook_bytes);
        reset_candidate_observers();

        if during_capture {
            arm_between_usr_rollback_activate_archived_finalization_database_captures(replace);

            let error = enter_route(&fixture);

            assert_pending_phase(&error, crate::transition_journal::Phase::RollbackComplete);
        } else {
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = super::support::capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
            replace();

            let error = authority.revalidate(&journal).unwrap_err();

            assert_eq!(
                error.to_string(),
                "the exact retained ActivateArchived terminal journal inode no longer matches its captured binding"
            );
        }
        assert_eq!(fs::read(&canonical).unwrap(), bytes);
        assert_eq!(fs::read(&displaced).unwrap(), bytes);
        assert_eq!(candidate_move_count(), 0);
    }
}

#[test]
fn startup_activate_archived_finalization_authority_refuses_inexact_terminal_identities() {
    for mutation in 0..3 {
        let fixture = RouteFixture::new(
            Epoch::Current,
            CandidateSource::Intent,
            RollbackActionOutcome::Applied,
            CandidateOutcome::AlreadySatisfied,
        );
        let terminal = persist_rollback_complete(&fixture);
        let mut wrong = terminal.clone();
        match mutation {
            0 => wrong.candidate.id = None,
            1 => wrong.previous.id = None,
            2 => wrong.previous.id = wrong.candidate.id,
            _ => unreachable!(),
        }
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let seal = UsrRollbackActivateArchivedFinalizationSeal::new_for_test();
        let database_before = fixture.database_snapshot();
        let namespace_before = fixture.namespace_snapshot();
        reset_candidate_observers();

        let admission = UsrRollbackActivateArchivedFinalizationAuthority::capture(
            &seal,
            &fixture.fixture.fixture.installation,
            &journal,
            &fixture.fixture.fixture.database,
            &reservation,
            &wrong,
        )
        .unwrap();

        assert!(matches!(
            admission,
            UsrRollbackActivateArchivedFinalizationAdmission::Deferred
        ));
        assert_eq!(fixture.canonical_record(), terminal);
        assert_eq!(fixture.database_snapshot(), database_before);
        assert_eq!(fixture.namespace_snapshot(), namespace_before);
        assert_eq!(candidate_move_count(), 0);
    }
}

#[test]
fn startup_activate_archived_finalization_admits_root_links_only_at_generation_twelve() {
    let fixture = RouteFixture::new(
        Epoch::Historical,
        CandidateSource::RootLinksComplete,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::Applied,
    );
    let terminal = persist_rollback_complete(&fixture);
    assert_eq!(terminal.generation, 12);
    let mut wrong_generation = terminal.clone();
    wrong_generation.generation += 1;
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackActivateArchivedFinalizationSeal::new_for_test();

    let admission = UsrRollbackActivateArchivedFinalizationAuthority::capture(
        &seal,
        &fixture.fixture.fixture.installation,
        &journal,
        &fixture.fixture.fixture.database,
        &reservation,
        &wrong_generation,
    )
    .unwrap();

    assert!(matches!(
        admission,
        UsrRollbackActivateArchivedFinalizationAdmission::Deferred
    ));
    assert_eq!(fixture.canonical_record(), terminal);
}

fn replace_with_same_bytes(canonical: &Path, displaced: &Path, bytes: &[u8]) {
    fs::rename(canonical, displaced).unwrap();
    let mut replacement = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(canonical)
        .unwrap();
    replacement
        .set_permissions(fs::Permissions::from_mode(0o600))
        .unwrap();
    replacement.write_all(bytes).unwrap();
    replacement.sync_all().unwrap();
}
