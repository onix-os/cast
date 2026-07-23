//! Focused contracts for read-only forward `BootSyncComplete` startup adoption.

use std::{fs, os::unix::fs::PermissionsExt};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            ActiveReblitBootSyncCompleteAdmission, ActiveReblitBootSyncCompleteRecordAdvanceError,
            arm_before_active_reblit_boot_sync_complete_fresh_namespace_capture,
            arm_between_active_reblit_boot_sync_complete_database_captures,
        },
    },
    state,
    transition_journal::Phase,
};

use super::{
    boot_sync_complete_support::{
        boot_sync_complete_fixture, capture_boot_sync_complete, capture_boot_sync_complete_ready,
        capture_boot_sync_complete_record, open_boot_sync_complete_journal,
    },
    support::Epoch,
};

#[test]
fn exact_promoted_receipt_full_state_selection_and_source_binding_admit() {
    {
        let fixture = boot_sync_complete_fixture(Epoch::Current, false);
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        assert!(matches!(
            capture_boot_sync_complete(&fixture, &journal, &reservation).unwrap(),
            ActiveReblitBootSyncCompleteAdmission::Deferred
        ));
    }

    for epoch in Epoch::ALL {
        let fixture = boot_sync_complete_fixture(epoch, true);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = capture_boot_sync_complete_ready(&fixture, &journal, &reservation);
        assert_eq!(authority.record(), &fixture.fixture.source);
        assert_eq!(authority.installation().root, fixture.fixture.installation.root);
        authority.revalidate(&journal).unwrap();

        let pair = fixture
            .fixture
            .source
            .boot_publication_receipt_correlation()
            .unwrap()
            .unwrap();
        fixture
            .fixture
            .database
            .load_exact_promoted_boot_publication_receipt_state(&fixture.fixture.source.transition_id, &pair)
            .unwrap();
        assert_eq!(
            fixture.fixture.installation.active_state,
            Some(fixture.fixture.candidate_state)
        );
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        drop(authority);
        drop(journal);
        drop(reservation);
    }
}

#[test]
fn stable_wrong_selection_defers_but_malformed_target_fails_stop() {
    let mut wrong_selection = boot_sync_complete_fixture(Epoch::Current, true);
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
        capture_boot_sync_complete(&wrong_selection, &journal, &reservation).unwrap(),
        ActiveReblitBootSyncCompleteAdmission::Deferred
    ));
    drop(reservation);
    drop(journal);

    let malformed = boot_sync_complete_fixture(Epoch::Current, true);
    let mut malformed_record = malformed.fixture.source.clone();
    malformed_record.candidate.id = None;
    let journal = open_boot_sync_complete_journal(&malformed);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(
        capture_boot_sync_complete_record(&malformed, &journal, &reservation, &malformed_record)
            .is_err()
    );
    assert_eq!(malformed.fixture.canonical_record(), malformed.fixture.source);
}

#[test]
fn database_and_source_binding_races_fail_stop_instead_of_deferring() {
    let full_state = boot_sync_complete_fixture(Epoch::Current, true);
    let database = full_state.fixture.database.clone();
    let candidate = full_state.fixture.candidate_state;
    arm_between_active_reblit_boot_sync_complete_database_captures(move || {
        database
            .change_summary_for_test(candidate, Some("changed inside BootSyncComplete database sandwich"))
            .unwrap();
    });
    let journal = open_boot_sync_complete_journal(&full_state);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(capture_boot_sync_complete(&full_state, &journal, &reservation).is_err());
    assert_eq!(full_state.fixture.canonical_record(), full_state.fixture.source);
    drop(reservation);
    drop(journal);

    let fixture = boot_sync_complete_fixture(Epoch::Current, true);
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    arm_between_active_reblit_boot_sync_complete_database_captures(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(capture_boot_sync_complete(&fixture, &journal, &reservation).is_err());
    drop(reservation);
    drop(journal);

    let stale = boot_sync_complete_fixture(Epoch::Current, true);
    let mut stale_record = stale.fixture.source.clone();
    stale_record.generation += 1;
    let journal = open_boot_sync_complete_journal(&stale);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(
        capture_boot_sync_complete_record(&stale, &journal, &reservation, &stale_record).is_err()
    );
    assert_eq!(stale.fixture.canonical_record(), stale.fixture.source);
}

#[test]
fn fresh_namespace_race_fails_revalidation_without_journal_advance() {
    let fixture = boot_sync_complete_fixture(Epoch::Current, true);
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = capture_boot_sync_complete_ready(&fixture, &journal, &reservation);
    let changed = fixture
        .fixture
        .active_reblit_reservation
        .as_ref()
        .expect("ActiveReblit fixture retains the replacement wrapper")
        .clone();
    let original_mode = fs::metadata(&changed).unwrap().permissions().mode() & 0o7777;
    let changed_mode = if original_mode == 0o700 { 0o755 } else { 0o700 };
    arm_before_active_reblit_boot_sync_complete_fresh_namespace_capture(move || {
        fs::set_permissions(changed, fs::Permissions::from_mode(changed_mode)).unwrap();
    });

    assert!(authority.revalidate(&journal).is_err());
    assert_eq!(fixture.fixture.canonical_record(), fixture.fixture.source);
    assert_eq!(
        fs::metadata(fixture.fixture.active_reblit_reservation.as_ref().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        changed_mode,
    );
}

#[test]
fn caller_supplied_non_successor_is_rejected_before_bound_persistence() {
    let fixture = boot_sync_complete_fixture(Epoch::Current, true);
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = capture_boot_sync_complete_ready(&fixture, &journal, &reservation);
    let mut wrong_successor = fixture.fixture.source.clone();
    wrong_successor.phase = Phase::CommitDecided;

    assert!(matches!(
        authority.advance_record_binding(&journal, &wrong_successor),
        Err(ActiveReblitBootSyncCompleteRecordAdvanceError::UnexpectedSuccessor)
    ));
    assert_eq!(fixture.fixture.canonical_record(), fixture.fixture.source);
}

#[test]
fn post_advance_evidence_validates_same_store_and_canonical_reopen() {
    for epoch in Epoch::ALL {
        let fixture = boot_sync_complete_fixture(epoch, true);
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = capture_boot_sync_complete_ready(&fixture, &journal, &reservation);
        let successor = fixture.fixture.source.forward_successor(None).unwrap();
        assert_eq!(successor.phase, Phase::CommitDecided);
        let (successor_binding, post_advance) = authority
            .advance_record_binding(&journal, &successor)
            .unwrap();

        post_advance
            .revalidate_successor_same_store(&journal, &successor_binding, &successor)
            .unwrap();
        drop(journal);
        let reopened = open_boot_sync_complete_journal(&fixture);
        post_advance
            .revalidate_successor_reopened(&reopened, &successor_binding, &successor)
            .unwrap();
        assert_eq!(fixture.fixture.canonical_record(), successor);
        drop(post_advance);
        drop(successor_binding);
        drop(reopened);
        drop(reservation);
    }
}
