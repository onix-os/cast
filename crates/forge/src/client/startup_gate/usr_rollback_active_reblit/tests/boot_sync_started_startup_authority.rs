//! Focused contracts for read-only promoted `BootSyncStarted` recovery.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            ActiveReblitBootSyncStartedRecoveryAdmission,
            arm_before_active_reblit_boot_sync_started_fresh_namespace_capture,
            arm_between_active_reblit_boot_sync_started_database_captures,
        },
    },
    state,
};

use super::{
    boot_sync_complete_support::{
        BootSyncCompleteReadOnlySnapshot, boot_sync_started_fixture,
        capture_boot_sync_started, capture_boot_sync_started_ready,
        capture_boot_sync_started_record, open_boot_sync_complete_journal,
        same_byte_different_inode_hook,
    },
    support::Epoch,
};

#[test]
fn exact_promoted_chain_plan_state_selection_namespace_and_binding_admit() {
    {
        let fixture = boot_sync_started_fixture(Epoch::Current, false);
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        assert!(matches!(
            capture_boot_sync_started(&fixture, &journal, &reservation).unwrap(),
            ActiveReblitBootSyncStartedRecoveryAdmission::RollbackEligible
        ));
    }

    for epoch in Epoch::ALL {
        let fixture = boot_sync_started_fixture(epoch, true);
        let source = fixture.fixture.source.clone();
        let read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority =
            capture_boot_sync_started_ready(&fixture, &journal, &reservation);

        assert_eq!(authority.record(), &source);
        assert_eq!(authority.installation().root, fixture.fixture.installation.root);
        authority.revalidate(&journal).unwrap();
        let plan = authority.cleanup_plan(&journal).unwrap();
        let pair = source
            .boot_publication_receipt_correlation()
            .unwrap()
            .unwrap();
        assert_eq!(plan.promoted_receipt(), pair.pending);
        drop(plan);
        assert_eq!(fixture.fixture.canonical_record(), source);
        read_only.assert_unchanged(&fixture);
        drop(authority);
        drop(journal);
        drop(reservation);
    }
}

#[test]
fn stable_wrong_selection_defers_but_unbound_record_fails_stop() {
    let mut wrong_selection = boot_sync_started_fixture(Epoch::Current, true);
    let other = state::Id::from(i32::from(wrong_selection.fixture.candidate_state) + 100);
    wrong_selection.fixture.installation.active_state = Some(other);
    fs::write(
        wrong_selection.fixture.installation.root.join("usr/.stateID"),
        i32::from(other).to_string(),
    )
    .unwrap();
    let journal = open_boot_sync_complete_journal(&wrong_selection);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        capture_boot_sync_started(&wrong_selection, &journal, &reservation).unwrap(),
        ActiveReblitBootSyncStartedRecoveryAdmission::Deferred
    ));
    drop(reservation);
    drop(journal);

    let malformed = boot_sync_started_fixture(Epoch::Current, true);
    let mut unbound_record = malformed.fixture.source.clone();
    unbound_record.candidate.id = None;
    let journal = open_boot_sync_complete_journal(&malformed);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(
        capture_boot_sync_started_record(
            &malformed,
            &journal,
            &reservation,
            &unbound_record,
        )
        .is_err()
    );
    assert_eq!(malformed.fixture.canonical_record(), malformed.fixture.source);
}

#[test]
fn database_receipt_chain_and_source_binding_races_fail_stop() {
    let database_race = boot_sync_started_fixture(Epoch::Current, true);
    let database = database_race.fixture.database.clone();
    let candidate = database_race.fixture.candidate_state;
    arm_between_active_reblit_boot_sync_started_database_captures(move || {
        database
            .change_summary_for_test(
                candidate,
                Some("changed inside BootSyncStarted recovery sandwich"),
            )
            .unwrap();
    });
    let journal = open_boot_sync_complete_journal(&database_race);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(capture_boot_sync_started(&database_race, &journal, &reservation).is_err());
    assert_eq!(
        database_race.fixture.canonical_record(),
        database_race.fixture.source,
    );
    drop(reservation);
    drop(journal);

    let receipt_race = boot_sync_started_fixture(Epoch::Current, true);
    let database = receipt_race.fixture.database.clone();
    arm_between_active_reblit_boot_sync_started_database_captures(move || {
        database.clear_boot_publication_receipt_head_for_test().unwrap();
    });
    let journal = open_boot_sync_complete_journal(&receipt_race);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(capture_boot_sync_started(&receipt_race, &journal, &reservation).is_err());
    assert_eq!(
        receipt_race.fixture.canonical_record(),
        receipt_race.fixture.source,
    );
    drop(reservation);
    drop(journal);

    let binding_race = boot_sync_started_fixture(Epoch::Historical, true);
    arm_between_active_reblit_boot_sync_started_database_captures(
        same_byte_different_inode_hook(&binding_race, "boot-sync-started-authority"),
    );
    let journal = open_boot_sync_complete_journal(&binding_race);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(capture_boot_sync_started(&binding_race, &journal, &reservation).is_err());
}

#[test]
fn fresh_namespace_race_fails_full_plan_revalidation_without_effects() {
    let fixture = boot_sync_started_fixture(Epoch::Current, true);
    let source = fixture.fixture.source.clone();
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = capture_boot_sync_started_ready(&fixture, &journal, &reservation);
    let changed = fixture
        .fixture
        .active_reblit_reservation
        .as_ref()
        .expect("ActiveReblit fixture retains the replacement wrapper")
        .clone();
    let original_mode = fs::metadata(&changed).unwrap().permissions().mode() & 0o7777;
    let changed_mode = if original_mode == 0o700 { 0o755 } else { 0o700 };
    arm_before_active_reblit_boot_sync_started_fresh_namespace_capture(move || {
        fs::set_permissions(changed, fs::Permissions::from_mode(changed_mode)).unwrap();
    });

    assert!(authority.cleanup_plan(&journal).is_err());
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(
        fs::metadata(fixture.fixture.active_reblit_reservation.as_ref().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        changed_mode,
    );
}
