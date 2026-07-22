//! Focused contracts for the promoted `BootSyncStarted` bound successor.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::ActiveReblitBootSyncStartedRecordAdvanceError,
    },
    transition_journal::Phase,
};

use super::{
    boot_sync_complete_support::{
        boot_sync_started_fixture, capture_boot_sync_started_ready,
        open_boot_sync_complete_journal,
    },
    support::Epoch,
};

#[test]
fn caller_supplied_non_successor_is_rejected_before_bound_persistence() {
    let fixture = boot_sync_started_fixture(Epoch::Current, true);
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority =
        capture_boot_sync_started_ready(&fixture, &journal, &reservation);
    let mut wrong_successor = fixture.fixture.source.clone();
    wrong_successor.phase = Phase::BootSyncComplete;

    assert!(matches!(
        authority.advance_record_binding(&journal, &wrong_successor),
        Err(ActiveReblitBootSyncStartedRecordAdvanceError::UnexpectedSuccessor)
    ));
    assert_eq!(fixture.fixture.canonical_record(), fixture.fixture.source);
}

#[test]
fn exact_successor_validates_same_store_and_canonical_reopen() {
    for epoch in Epoch::ALL {
        let fixture = boot_sync_started_fixture(epoch, true);
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority =
            capture_boot_sync_started_ready(&fixture, &journal, &reservation);
        let pair = fixture
            .fixture
            .source
            .boot_publication_receipt_correlation()
            .unwrap()
            .unwrap();
        let successor = fixture
            .fixture
            .source
            .boot_sync_complete_successor(pair)
            .unwrap();
        let (successor_binding, post_advance) = authority
            .advance_record_binding(&journal, &successor)
            .unwrap();

        post_advance
            .revalidate_successor_same_store(
                &journal,
                &successor_binding,
                &successor,
            )
            .unwrap();
        drop(journal);
        let reopened = open_boot_sync_complete_journal(&fixture);
        post_advance
            .revalidate_successor_reopened(
                &reopened,
                &successor_binding,
                &successor,
            )
            .unwrap();
        assert_eq!(fixture.fixture.canonical_record(), successor);
        drop(post_advance);
        drop(successor_binding);
        drop(reopened);
        drop(reservation);
    }
}
