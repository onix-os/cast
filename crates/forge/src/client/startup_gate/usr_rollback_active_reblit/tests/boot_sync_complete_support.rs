//! Shared construction and read-only assertions for forward boot completion.

use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    time::{Duration, Instant},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::ActiveReblitBootSyncCompleteSeal,
        startup_reconciliation::{
            ActiveReblitBootSyncCompleteAdmission, ActiveReblitBootSyncCompleteAuthority,
            ActiveReblitBootSyncCompleteAuthorityError,
        },
    },
    db,
    transition_journal::{Phase, TransitionJournalStore, TransitionRecord, encode},
};

use super::{
    super::test_fixture::{BootSyncStartedLayout, DatabaseSnapshot, NamespaceEntry},
    support::{BootRepairFixture, Epoch, build_boot_sync_started},
};

/// Database, exact promoted receipt, and non-journal namespace evidence which
/// the journal-only startup route must retain byte-for-byte and identity-for-
/// identity.
pub(super) struct BootSyncCompleteReadOnlySnapshot {
    database: DatabaseSnapshot,
    namespace: Vec<NamespaceEntry>,
    receipt: db::state::BootPublicationReceiptState,
}

impl BootSyncCompleteReadOnlySnapshot {
    pub(super) fn capture(fixture: &BootRepairFixture) -> Self {
        Self {
            database: fixture.fixture.database_snapshot(),
            namespace: fixture.fixture.namespace_snapshot(),
            receipt: fixture.fixture.database.boot_publication_receipt_state().unwrap(),
        }
    }

    pub(super) fn assert_unchanged(self, fixture: &BootRepairFixture) {
        assert_eq!(fixture.fixture.database_snapshot(), self.database);
        assert_eq!(fixture.fixture.namespace_snapshot(), self.namespace);
        assert_eq!(
            fixture.fixture.database.boot_publication_receipt_state().unwrap(),
            self.receipt
        );
    }
}

pub(super) fn boot_sync_complete_fixture(epoch: Epoch, promote_receipt: bool) -> BootRepairFixture {
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
    let journal = open_boot_sync_complete_journal(&fixture);
    journal.advance(&fixture.fixture.source, &completed).unwrap();
    drop(journal);
    fixture.fixture.source = completed;
    fixture
}

/// Recreate an already-existing legacy boot-completion checkpoint. Legacy
/// payloads cannot newly cross this edge, but a valid on-disk v1/v2 record
/// must stay forward-pending rather than being reinterpreted as rollback.
pub(super) fn legacy_boot_sync_complete_fixture(epoch: Epoch, version: u16) -> BootRepairFixture {
    let mut fixture = boot_sync_complete_fixture(epoch, true);
    assert!(matches!(version, 1 | 2));
    fixture.fixture.source.version = version;
    fixture.fixture.source.boot_publication_receipts = None;
    fs::write(
        fixture
            .fixture
            .installation
            .root
            .join(".cast/journal/state-transition"),
        encode(&fixture.fixture.source).unwrap(),
    )
    .unwrap();
    assert_eq!(fixture.fixture.canonical_record(), fixture.fixture.source);
    fixture
}

pub(super) fn open_boot_sync_complete_journal(fixture: &BootRepairFixture) -> TransitionJournalStore {
    TransitionJournalStore::open_retained(
        fixture.fixture.installation.root_directory(),
        &fixture.fixture.installation.root,
    )
    .unwrap()
}

pub(super) fn exact_commit_decided(fixture: &BootRepairFixture) -> TransitionRecord {
    let successor = fixture.fixture.source.forward_successor(None).unwrap();
    assert_eq!(successor.phase, Phase::CommitDecided);
    successor
}

pub(super) fn same_byte_different_inode_hook(
    fixture: &BootRepairFixture,
    label: &str,
) -> impl FnOnce() + 'static {
    let canonical = fixture
        .fixture
        .installation
        .root
        .join(".cast/journal/state-transition");
    let displaced = fixture
        .fixture
        .installation
        .root
        .join(".cast/journal")
        .join(format!(".{label}-displaced"));
    move || {
        let bytes = fs::read(&canonical).unwrap();
        fs::rename(&canonical, &displaced).unwrap();
        let retained = fs::symlink_metadata(&displaced).unwrap();
        fs::write(&canonical, &bytes).unwrap();
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
        let replacement = fs::symlink_metadata(&canonical).unwrap();
        assert_eq!(fs::read(&canonical).unwrap(), bytes);
        assert_ne!((retained.dev(), retained.ino()), (replacement.dev(), replacement.ino()));
        fs::remove_file(displaced).unwrap();
    }
}

pub(super) fn capture_boot_sync_complete<'reservation>(
    fixture: &BootRepairFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> Result<ActiveReblitBootSyncCompleteAdmission<'reservation>, ActiveReblitBootSyncCompleteAuthorityError> {
    capture_boot_sync_complete_record(fixture, journal, reservation, &fixture.fixture.source)
}

pub(super) fn capture_boot_sync_complete_record<'reservation>(
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

pub(super) fn capture_boot_sync_complete_ready<'reservation>(
    fixture: &BootRepairFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> ActiveReblitBootSyncCompleteAuthority<'reservation> {
    match capture_boot_sync_complete(fixture, journal, reservation).unwrap() {
        ActiveReblitBootSyncCompleteAdmission::Ready(authority) => authority,
        _ => panic!("exact promoted ActiveReblit BootSyncComplete evidence did not admit"),
    }
}

pub(super) fn exact_promoted_receipt_state(
    fixture: &BootRepairFixture,
) -> db::state::BootPublicationReceiptState {
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
        .unwrap()
}
