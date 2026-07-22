//! Production startup routing at the BootSyncStarted receipt-promotion edge.

use std::{
    fs,
    os::unix::fs::MetadataExt as _,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    boot_publication::BootPublicationReceiptFingerprint,
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{self, active_reblit_boot_sync_started},
        startup_reconciliation::arm_between_usr_rollback_decision_database_captures,
    },
    db,
    transition_journal::{
        ForwardPhase, Phase, TransitionJournalRecordBinding, TransitionRecord, encode,
    },
};

use super::{
    super::test_fixture::BootSyncStartedLayout,
    boot_sync_complete_support::{
        BootSyncCompleteReadOnlySnapshot, boot_sync_started_fixture,
        open_boot_sync_complete_journal,
    },
    support::{
        BootRepairFixture, Epoch, assert_complete_route_journal_only,
        assert_pending_phase, build_legacy_boot_sync_started, enter_boot,
        reset_complete_route_effect_observers,
    },
};

struct ExactJournalSnapshot {
    path: PathBuf,
    bytes: Vec<u8>,
    device: u64,
    inode: u64,
    record: TransitionRecord,
    binding: TransitionJournalRecordBinding,
}

impl ExactJournalSnapshot {
    fn capture(fixture: &BootRepairFixture) -> Self {
        let path = fixture
            .fixture
            .installation
            .root
            .join(".cast/journal/state-transition");
        let bytes = fs::read(&path).unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let record = fixture.fixture.source.clone();
        let journal = open_boot_sync_complete_journal(fixture);
        let cast = fixture
            .fixture
            .installation
            .retained_mutable_cast_directory()
            .unwrap();
        let binding = journal.record_binding(cast, &record).unwrap();
        drop(journal);
        Self {
            path,
            bytes,
            device: metadata.dev(),
            inode: metadata.ino(),
            record,
            binding,
        }
    }

    fn assert_unchanged(self, fixture: &BootRepairFixture) {
        assert_eq!(fs::read(&self.path).unwrap(), self.bytes);
        let metadata = fs::symlink_metadata(&self.path).unwrap();
        assert_eq!((metadata.dev(), metadata.ino()), (self.device, self.inode));
        let reopened = open_boot_sync_complete_journal(fixture);
        let cast = fixture
            .fixture
            .installation
            .retained_mutable_cast_directory()
            .unwrap();
        assert!(
            reopened
                .has_reopened_record_binding(cast, &self.binding, &self.record)
                .unwrap(),
            "startup changed the exact canonical journal inode binding"
        );
    }
}

#[test]
fn startup_promoted_boot_sync_started_enters_recovery_without_rollback() {
    for epoch in Epoch::ALL {
        let fixture = boot_sync_started_fixture(epoch, true);
        let source = fixture.fixture.source.clone();
        let journal = ExactJournalSnapshot::capture(&fixture);
        let read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
        reset_complete_route_effect_observers();

        let error = enter_boot(&fixture);

        assert!(
            matches!(
                &error,
                startup_gate::Error::ActiveReblitBootSyncStartedDispatch(
                    active_reblit_boot_sync_started::Error::Recovery(_)
                )
            ),
            "expected promoted BootSyncStarted to enter forward recovery, got {error:?}"
        );
        assert_eq!(fixture.fixture.canonical_record(), source);
        assert_eq!(source.rollback, None);
        journal.assert_unchanged(&fixture);
        read_only.assert_unchanged(&fixture);
        assert_complete_route_journal_only();
    }
}

#[test]
fn startup_exact_pending_boot_sync_started_remains_rollback_eligible() {
    let fixture = boot_sync_started_fixture(Epoch::Current, false);
    let source = fixture.fixture.source.clone();
    let read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
    reset_complete_route_effect_observers();

    let error = enter_boot(&fixture);

    assert_pending_phase(&error, Phase::RollbackDecided);
    let decided = fixture.fixture.canonical_record();
    assert_eq!(decided.generation, source.generation + 1);
    assert_eq!(decided.rollback.as_ref().unwrap().source, ForwardPhase::BootSyncStarted);
    read_only.assert_unchanged(&fixture);
    assert_complete_route_journal_only();
}

#[test]
fn startup_legacy_boot_sync_started_remains_rollback_eligible() {
    let fixture = build_legacy_boot_sync_started(
        Epoch::Historical,
        BootSyncStartedLayout::Post,
        2,
    );
    let source = fixture.fixture.source.clone();
    reset_complete_route_effect_observers();

    let error = enter_boot(&fixture);

    assert_pending_phase(&error, Phase::RollbackDecided);
    let decided = fixture.fixture.canonical_record();
    assert_eq!(decided.generation, source.generation + 1);
    assert_eq!(decided.rollback.as_ref().unwrap().source, ForwardPhase::BootSyncStarted);
    assert_complete_route_journal_only();
}

#[test]
fn startup_conflicting_pending_receipt_correlation_fails_stop_without_rollback() {
    let mut fixture = boot_sync_started_fixture(Epoch::Current, false);
    let mut conflicting = fixture.fixture.source.clone();
    let pair = conflicting.boot_publication_receipts.as_mut().unwrap();
    let retained_pending = pair.pending;
    pair.pending = BootPublicationReceiptFingerprint::from_bytes([0x7d; 32]);
    assert_ne!(pair.pending, retained_pending);
    fs::write(
        fixture
            .fixture
            .installation
            .root
            .join(".cast/journal/state-transition"),
        encode(&conflicting).unwrap(),
    )
    .unwrap();
    fixture.fixture.source = conflicting.clone();
    let read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
    reset_complete_route_effect_observers();

    let error = enter_boot(&fixture);

    assert!(
        matches!(
            &error,
            startup_gate::Error::ActiveReblitBootSyncStartedDispatch(
                active_reblit_boot_sync_started::Error::Authority(_)
            )
        ),
        "expected conflicting pending receipt correlation to fail stop, got {error:?}"
    );
    assert!(format!("{error:?}").contains("PendingReceiptCorrelationMismatch"));
    assert_eq!(fixture.fixture.canonical_record(), conflicting);
    assert_eq!(conflicting.rollback, None);
    read_only.assert_unchanged(&fixture);
    assert_complete_route_journal_only();
}

#[test]
fn startup_dangling_pending_receipt_body_fails_stop_without_journal_or_rollback_mutation() {
    let fixture = boot_sync_started_fixture(Epoch::Current, false);
    let source = fixture.fixture.source.clone();
    let pair = source
        .boot_publication_receipt_correlation()
        .unwrap()
        .unwrap();
    let journal = ExactJournalSnapshot::capture(&fixture);
    fixture
        .fixture
        .database
        .delete_boot_publication_receipt_body_for_test(pair.pending);
    assert!(matches!(
        fixture.fixture.database.boot_publication_receipt_state(),
        Err(db::state::BootPublicationReceiptStateError::DanglingReference { .. })
    ));
    reset_complete_route_effect_observers();

    let error = enter_boot(&fixture);

    assert!(
        matches!(
            &error,
            startup_gate::Error::ActiveReblitBootSyncStartedDispatch(
                active_reblit_boot_sync_started::Error::Authority(_)
            )
        ),
        "expected dangling pending receipt body to fail stop, got {error:?}"
    );
    assert!(format!("{error:?}").contains("DanglingReference"));
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(source.rollback, None);
    journal.assert_unchanged(&fixture);
    assert!(matches!(
        fixture.fixture.database.boot_publication_receipt_state(),
        Err(db::state::BootPublicationReceiptStateError::DanglingReference { .. })
    ));
    assert_complete_route_journal_only();
}

#[test]
fn startup_cooperating_writer_cannot_promote_between_pending_guard_and_rollback() {
    let fixture = boot_sync_started_fixture(Epoch::Current, false);
    let pair = fixture
        .fixture
        .source
        .boot_publication_receipt_correlation()
        .unwrap()
        .unwrap();
    let contender_promoted = Arc::new(AtomicBool::new(false));
    let contender = Arc::new(Mutex::new(None));
    let contender_in_hook = Arc::clone(&contender);
    let promoted_in_thread = Arc::clone(&contender_promoted);
    let promoted_in_hook = Arc::clone(&contender_promoted);
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let acquired_rx = Arc::new(Mutex::new(acquired_rx));
    let acquired_rx_in_hook = Arc::clone(&acquired_rx);
    let (completed_tx, completed_rx) = mpsc::channel();
    let hook_database = fixture.fixture.database.clone();
    let promotion_database = fixture.fixture.database.clone();
    let transition_id = fixture.fixture.source.transition_id.clone();
    let transition_id_in_hook = transition_id.clone();
    arm_between_usr_rollback_decision_database_captures(move || {
        let (started_tx, started_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let reservation = ActiveStateReservation::acquire().unwrap();
            acquired_tx.send(()).unwrap();
            let state = promotion_database.boot_publication_receipt_state().unwrap();
            let pending = state.pending().expect("the guarded receipt remains pending");
            promotion_database
                .promote_boot_publication_receipt(
                    pending,
                    Instant::now() + Duration::from_secs(30),
                )
                .unwrap();
            promoted_in_thread.store(true, Ordering::SeqCst);
            completed_tx.send(()).unwrap();
            drop(reservation);
        });
        *contender_in_hook.lock().unwrap() = Some(handle);
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            matches!(
                acquired_rx_in_hook
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "cooperating writer acquired between pending classification and rollback admission"
        );
        assert!(
            !promoted_in_hook.load(Ordering::SeqCst),
            "receipt was promoted between pending classification and rollback admission"
        );
        let pending = hook_database.boot_publication_receipt_state().unwrap();
        assert_eq!(pending.receipt_pair_for(&transition_id_in_hook), Some(pair));
    });

    let error = enter_boot(&fixture);

    assert_pending_phase(&error, Phase::RollbackDecided);
    acquired_rx
        .lock()
        .unwrap()
        .recv_timeout(Duration::from_secs(2))
        .expect("cooperating writer did not acquire after startup returned");
    completed_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("receipt-promotion contender did not complete after startup returned");
    contender
        .lock()
        .unwrap()
        .take()
        .expect("rollback-decision hook spawned a contender")
        .join()
        .unwrap();
    assert!(contender_promoted.load(Ordering::SeqCst));
    assert_eq!(fixture.fixture.canonical_record().phase, Phase::RollbackDecided);
    fixture
        .fixture
        .database
        .load_exact_promoted_boot_publication_receipt_state(&transition_id, &pair)
        .unwrap();
}
