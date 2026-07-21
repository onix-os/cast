use diesel::connection::SimpleConnection as _;

use super::*;

#[test]
fn first_stage_persists_one_exact_body_and_exact_retry_is_read_only() {
    let database = Database::new(":memory:").unwrap();
    let receipt = receipt('1', None, 0x11);

    assert_eq!(
        database.stage_boot_publication_receipt(&receipt).unwrap(),
        BootPublicationReceiptStageOutcome::Staged
    );
    let first = database.boot_publication_receipt_state().unwrap();
    assert_eq!(receipt_row_count(&database), 1);
    assert_eq!(first.head().committed(), None);
    assert_eq!(first.committed(), None);
    assert_eq!(first.pending(), Some(&receipt));
    assert_eq!(
        first.receipt_pair_for(receipt.body().transition_id()),
        Some(BootPublicationReceiptPair {
            committed: None,
            pending: receipt.fingerprint(),
        })
    );

    assert_eq!(
        database.stage_boot_publication_receipt(&receipt).unwrap(),
        BootPublicationReceiptStageOutcome::AlreadyStaged
    );
    assert_eq!(database.boot_publication_receipt_state().unwrap(), first);
    assert_eq!(receipt_row_count(&database), 1);
}

#[test]
fn exact_committed_predecessor_and_pending_body_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let url = path.to_str().unwrap();
    let committed = receipt('2', None, 0x21);
    let pending = receipt('3', Some(committed.fingerprint()), 0x31);

    let database = Database::new(url).unwrap();
    insert_raw_receipt(&database, &committed);
    database
        .replace_boot_publication_receipt_head_for_test(Some(committed.fingerprint()), None)
        .unwrap();
    assert_eq!(
        database.stage_boot_publication_receipt(&pending).unwrap(),
        BootPublicationReceiptStageOutcome::Staged
    );
    let expected = database.boot_publication_receipt_state().unwrap();
    assert_eq!(expected.committed(), Some(&committed));
    assert_eq!(expected.pending(), Some(&pending));
    drop(database);

    let reopened = Database::new(url).unwrap();
    assert_eq!(reopened.boot_publication_receipt_state().unwrap(), expected);
    assert_eq!(receipt_row_count(&reopened), 2);
}

#[test]
fn committed_and_pending_conflicts_leave_the_exact_state_unchanged() {
    let database = Database::new(":memory:").unwrap();
    let first = receipt('4', None, 0x41);
    database.stage_boot_publication_receipt(&first).unwrap();
    let before = database.boot_publication_receipt_state().unwrap();
    let conflicting_pending = receipt('5', None, 0x51);

    assert!(matches!(
        database.stage_boot_publication_receipt(&conflicting_pending),
        Err(BootPublicationReceiptStateError::PendingConflict { .. })
    ));
    assert_eq!(database.boot_publication_receipt_state().unwrap(), before);
    assert_eq!(receipt_row_count(&database), 1);

    let clean = Database::new(":memory:").unwrap();
    let foreign_predecessor = BootPublicationReceiptFingerprint::from_bytes([0x66; 32]);
    let committed_conflict = receipt('6', Some(foreign_predecessor), 0x61);
    assert!(matches!(
        clean.stage_boot_publication_receipt(&committed_conflict),
        Err(BootPublicationReceiptStateError::CommittedMismatch {
            expected: Some(expected),
            actual: None,
        }) if expected == foreign_predecessor
    ));
    assert_eq!(receipt_row_count(&clean), 0);
    assert_eq!(clean.boot_publication_receipt_state().unwrap().head().pending(), None);
}

#[test]
fn head_update_failure_rolls_back_the_body_insert_in_the_same_transaction() {
    let database = Database::new(":memory:").unwrap();
    database.conn.exec(|connection| {
        connection
            .batch_execute(
                "CREATE TRIGGER reject_receipt_head_stage \
                 BEFORE UPDATE OF pending_transition_id ON boot_publication_receipt_head \
                 BEGIN SELECT RAISE(ABORT, 'synthetic receipt-head stage failure'); END;",
            )
            .unwrap();
    });
    let receipt = receipt('7', None, 0x71);

    assert!(matches!(
        database.stage_boot_publication_receipt(&receipt),
        Err(BootPublicationReceiptStateError::Database(_))
    ));
    assert_eq!(receipt_row_count(&database), 0);
    let state = database.boot_publication_receipt_state().unwrap();
    assert_eq!(state.head().committed(), None);
    assert_eq!(state.head().pending(), None);
    assert_eq!(state.pending(), None);
}
