//! Focused contracts for read-only forward `BootSyncComplete` startup adoption.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::ActiveReblitBootSyncCompleteSeal,
        startup_reconciliation::{
            ActiveReblitBootSyncCompleteAdmission, ActiveReblitBootSyncCompleteAuthority,
            ActiveReblitBootSyncCompleteAuthorityError, ActiveReblitBootSyncCompleteRecordAdvanceError,
            arm_before_active_reblit_boot_sync_complete_fresh_namespace_capture,
            arm_between_active_reblit_boot_sync_complete_database_captures,
        },
    },
    state,
    transition_journal::{Phase, TransitionJournalStore, TransitionRecord},
};

use super::super::test_fixture::BootSyncStartedLayout;
use super::support::{BootRepairFixture, Epoch, build_boot_sync_started};

#[test]
fn exact_promoted_receipt_full_state_selection_and_source_binding_admit() {
    {
        let fixture = boot_sync_complete_fixture(Epoch::Current, false);
        let journal = open_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        assert!(matches!(
            capture(&fixture, &journal, &reservation).unwrap(),
            ActiveReblitBootSyncCompleteAdmission::Deferred
        ));
    }

    for epoch in Epoch::ALL {
        let fixture = boot_sync_complete_fixture(epoch, true);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        let journal = open_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = capture_ready(&fixture, &journal, &reservation);
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
    let journal = open_journal(&wrong_selection);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        capture(&wrong_selection, &journal, &reservation).unwrap(),
        ActiveReblitBootSyncCompleteAdmission::Deferred
    ));
    drop(reservation);
    drop(journal);

    let malformed = boot_sync_complete_fixture(Epoch::Current, true);
    let mut malformed_record = malformed.fixture.source.clone();
    malformed_record.candidate.id = None;
    let journal = open_journal(&malformed);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(capture_record(&malformed, &journal, &reservation, &malformed_record).is_err());
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
    let journal = open_journal(&full_state);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(capture(&full_state, &journal, &reservation).is_err());
    assert_eq!(full_state.fixture.canonical_record(), full_state.fixture.source);
    drop(reservation);
    drop(journal);

    let fixture = boot_sync_complete_fixture(Epoch::Current, true);
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    arm_between_active_reblit_boot_sync_complete_database_captures(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });
    let journal = open_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(capture(&fixture, &journal, &reservation).is_err());
    drop(reservation);
    drop(journal);

    let stale = boot_sync_complete_fixture(Epoch::Current, true);
    let mut stale_record = stale.fixture.source.clone();
    stale_record.generation += 1;
    let journal = open_journal(&stale);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(capture_record(&stale, &journal, &reservation, &stale_record).is_err());
    assert_eq!(stale.fixture.canonical_record(), stale.fixture.source);
}

#[test]
fn fresh_namespace_race_fails_revalidation_without_journal_advance() {
    let fixture = boot_sync_complete_fixture(Epoch::Current, true);
    let journal = open_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = capture_ready(&fixture, &journal, &reservation);
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
    let journal = open_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = capture_ready(&fixture, &journal, &reservation);
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
        let journal = open_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = capture_ready(&fixture, &journal, &reservation);
        let successor = fixture.fixture.source.forward_successor(None).unwrap();
        assert_eq!(successor.phase, Phase::CommitDecided);
        let (successor_binding, post_advance) = authority
            .advance_record_binding(&journal, &successor)
            .unwrap();

        post_advance
            .revalidate_successor_same_store(&journal, &successor_binding, &successor)
            .unwrap();
        drop(journal);
        let reopened = open_journal(&fixture);
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

fn boot_sync_complete_fixture(epoch: Epoch, promote_receipt: bool) -> BootRepairFixture {
    let mut fixture = build_boot_sync_started(epoch, BootSyncStartedLayout::Post);
    let pair = fixture
        .fixture
        .source
        .boot_publication_receipt_correlation()
        .unwrap()
        .unwrap();
    if promote_receipt {
        let receipt_state = fixture.fixture.database.boot_publication_receipt_state().unwrap();
        let pending = receipt_state.pending().expect("fixture carries the staged receipt");
        fixture
            .fixture
            .database
            .promote_boot_publication_receipt(pending, Instant::now() + Duration::from_secs(30))
            .unwrap();
    }
    let completed = fixture
        .fixture
        .source
        .boot_sync_complete_successor(pair)
        .unwrap();
    let journal = open_journal(&fixture);
    journal.advance(&fixture.fixture.source, &completed).unwrap();
    drop(journal);
    fixture.fixture.source = completed;
    fixture
}

fn open_journal(fixture: &BootRepairFixture) -> TransitionJournalStore {
    TransitionJournalStore::open_retained(
        fixture.fixture.installation.root_directory(),
        &fixture.fixture.installation.root,
    )
    .unwrap()
}

fn capture<'reservation>(
    fixture: &BootRepairFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> Result<ActiveReblitBootSyncCompleteAdmission<'reservation>, ActiveReblitBootSyncCompleteAuthorityError> {
    capture_record(fixture, journal, reservation, &fixture.fixture.source)
}

fn capture_record<'reservation>(
    fixture: &BootRepairFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> Result<ActiveReblitBootSyncCompleteAdmission<'reservation>, ActiveReblitBootSyncCompleteAuthorityError> {
    let seal = ActiveReblitBootSyncCompleteSeal::new_for_test();
    ActiveReblitBootSyncCompleteAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        journal,
        &fixture.fixture.database,
        reservation,
        record,
    )
}

fn capture_ready<'reservation>(
    fixture: &BootRepairFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> ActiveReblitBootSyncCompleteAuthority<'reservation> {
    match capture(fixture, journal, reservation).unwrap() {
        ActiveReblitBootSyncCompleteAdmission::Ready(authority) => authority,
        _ => panic!("exact promoted ActiveReblit BootSyncComplete evidence did not admit"),
    }
}
