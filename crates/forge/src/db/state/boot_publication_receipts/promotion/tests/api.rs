use diesel::{Connection as _, connection::SimpleConnection as _};

use super::*;

#[test]
fn first_pending_receipt_promotes_atomically_and_retains_its_body() {
    let database = Database::new(":memory:").unwrap();
    let pending = receipt('1', None, 0x11);
    stage(&database, &pending);

    assert_eq!(
        database
            .promote_boot_publication_receipt(&pending, promotion_deadline())
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
    let state = database.boot_publication_receipt_state().unwrap();
    assert_eq!(state.head().committed(), Some(pending.fingerprint()));
    assert!(state.head().pending().is_none());
    assert_eq!(state.committed(), Some(&pending));
    assert!(state.pending().is_none());
    assert_eq!(receipt_row_count(&database), 1);
    assert_eq!(stored_body(&database, pending.fingerprint()), pending.canonical_body());
}

#[test]
fn chained_promotion_preserves_predecessor_and_successor_bodies() {
    let database = Database::new(":memory:").unwrap();
    let predecessor = receipt('2', None, 0x21);
    stage_and_promote(&database, &predecessor);
    let pending = receipt('3', Some(predecessor.fingerprint()), 0x31);
    stage(&database, &pending);

    assert_eq!(
        database
            .promote_boot_publication_receipt(&pending, promotion_deadline())
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
    let state = database.boot_publication_receipt_state().unwrap();
    assert_eq!(state.head().committed(), Some(pending.fingerprint()));
    assert_eq!(state.committed(), Some(&pending));
    assert!(state.pending().is_none());
    assert_eq!(receipt_row_count(&database), 2);
    assert_eq!(
        stored_body(&database, predecessor.fingerprint()),
        predecessor.canonical_body(),
    );
    assert_eq!(stored_body(&database, pending.fingerprint()), pending.canonical_body());
}

#[test]
fn exact_promoted_retry_is_read_only_even_when_updates_are_rejected() {
    let database = Database::new(":memory:").unwrap();
    let receipt = receipt('4', None, 0x41);
    stage_and_promote(&database, &receipt);
    let before = database.boot_publication_receipt_state().unwrap();
    let body = stored_body(&database, receipt.fingerprint());
    database.conn.exec(|connection| {
        connection
            .batch_execute(
                "CREATE TRIGGER reject_any_promotion_retry \
                 BEFORE UPDATE ON boot_publication_receipt_head \
                 BEGIN SELECT RAISE(ABORT, 'promotion retry wrote'); END;",
            )
            .unwrap();
    });

    assert_eq!(
        database
            .promote_boot_publication_receipt(&receipt, promotion_deadline())
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::AlreadyPromoted,
    );
    assert_eq!(database.boot_publication_receipt_state().unwrap(), before);
    assert_eq!(stored_body(&database, receipt.fingerprint()), body);
    assert_eq!(receipt_row_count(&database), 1);
}

#[test]
fn exact_promoted_retry_uses_no_exclusive_lock_against_an_independent_reader() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let url = path.to_str().unwrap();
    let receipt = receipt('5', None, 0x51);
    let database = Database::new(url).unwrap();
    stage_and_promote(&database, &receipt);

    let mut reader = SqliteConnection::establish(url).unwrap();
    reader.batch_execute("BEGIN DEFERRED").unwrap();
    let rows: i64 = boot_publication_receipt_head::table
        .count()
        .get_result(&mut reader)
        .unwrap();
    assert_eq!(rows, 1);

    assert_eq!(
        database
            .promote_boot_publication_receipt(&receipt, promotion_deadline())
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::AlreadyPromoted,
    );
    reader.batch_execute("ROLLBACK").unwrap();
}

#[test]
fn stale_replay_cannot_displace_a_pending_or_promoted_successor() {
    let database = Database::new(":memory:").unwrap();
    let predecessor = receipt('6', None, 0x61);
    stage_and_promote(&database, &predecessor);
    let successor = receipt('7', Some(predecessor.fingerprint()), 0x71);
    stage(&database, &successor);

    let pending_successor = database.boot_publication_receipt_state().unwrap();
    assert!(matches!(
        database.promote_boot_publication_receipt(&predecessor, promotion_deadline()),
        Err(BootPublicationReceiptPromotionError::CommittedPredecessorMismatch { .. })
    ));
    assert_eq!(
        database.boot_publication_receipt_state().unwrap(),
        pending_successor,
    );

    assert_eq!(
        database
            .promote_boot_publication_receipt(&successor, promotion_deadline())
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
    let promoted_successor = database.boot_publication_receipt_state().unwrap();
    assert!(matches!(
        database.promote_boot_publication_receipt(&predecessor, promotion_deadline()),
        Err(BootPublicationReceiptPromotionError::CommittedPredecessorMismatch { .. })
    ));
    assert_eq!(
        database.boot_publication_receipt_state().unwrap(),
        promoted_successor,
    );
}

#[test]
fn promoted_head_and_body_survive_a_real_database_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let url = path.to_str().unwrap();
    let receipt = receipt('8', None, 0x81);
    let database = Database::new(url).unwrap();
    stage_and_promote(&database, &receipt);
    drop(database);

    let reopened = Database::new(url).unwrap();
    let state = reopened.boot_publication_receipt_state().unwrap();
    assert_eq!(state.head().committed(), Some(receipt.fingerprint()));
    assert!(state.head().pending().is_none());
    assert_eq!(state.committed(), Some(&receipt));
    assert!(state.pending().is_none());
    assert_eq!(receipt_row_count(&reopened), 1);
    assert_eq!(
        reopened
            .promote_boot_publication_receipt(&receipt, promotion_deadline())
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::AlreadyPromoted,
    );
}
