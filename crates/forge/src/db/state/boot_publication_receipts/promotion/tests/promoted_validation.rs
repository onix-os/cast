use diesel::{Connection as _, connection::SimpleConnection as _};

use super::*;

#[test]
fn read_only_validator_rejects_pending_then_accepts_exact_promoted_without_writes_or_exclusive_lock() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let url = path.to_str().unwrap();
    let database = Database::new(url).unwrap();
    let receipt = receipt('9', None, 0x91);
    stage(&database, &receipt);

    assert!(matches!(
        database.require_promoted_boot_publication_receipt(&receipt),
        Err(BootPublicationReceiptPromotionError::RequiredPromotedReceiptStillPending)
    ));

    assert_eq!(
        database
            .promote_boot_publication_receipt(&receipt, promotion_deadline())
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
    let before = database.boot_publication_receipt_state().unwrap();
    let body = stored_body(&database, receipt.fingerprint());
    database.conn.exec(|connection| {
        connection
            .batch_execute(
                "CREATE TRIGGER reject_promoted_validation_head_insert \
                 BEFORE INSERT ON boot_publication_receipt_head \
                 BEGIN SELECT RAISE(ABORT, 'promoted validation inserted head'); END; \
                 CREATE TRIGGER reject_promoted_validation_head_update \
                 BEFORE UPDATE ON boot_publication_receipt_head \
                 BEGIN SELECT RAISE(ABORT, 'promoted validation updated head'); END; \
                 CREATE TRIGGER reject_promoted_validation_head_delete \
                 BEFORE DELETE ON boot_publication_receipt_head \
                 BEGIN SELECT RAISE(ABORT, 'promoted validation deleted head'); END; \
                 CREATE TRIGGER reject_promoted_validation_body_insert \
                 BEFORE INSERT ON boot_publication_receipts \
                 BEGIN SELECT RAISE(ABORT, 'promoted validation inserted body'); END; \
                 CREATE TRIGGER reject_promoted_validation_body_update \
                 BEFORE UPDATE ON boot_publication_receipts \
                 BEGIN SELECT RAISE(ABORT, 'promoted validation updated body'); END; \
                 CREATE TRIGGER reject_promoted_validation_body_delete \
                 BEFORE DELETE ON boot_publication_receipts \
                 BEGIN SELECT RAISE(ABORT, 'promoted validation deleted body'); END;",
            )
            .unwrap();
    });

    let mut independent_reader = SqliteConnection::establish(url).unwrap();
    independent_reader.batch_execute("BEGIN DEFERRED").unwrap();
    let rows: i64 = boot_publication_receipt_head::table
        .count()
        .get_result(&mut independent_reader)
        .unwrap();
    assert_eq!(rows, 1);

    database
        .require_promoted_boot_publication_receipt(&receipt)
        .unwrap();
    independent_reader.batch_execute("ROLLBACK").unwrap();
    assert_eq!(database.boot_publication_receipt_state().unwrap(), before);
    assert_eq!(stored_body(&database, receipt.fingerprint()), body);
    assert_eq!(receipt_row_count(&database), 1);
}

#[test]
fn read_only_validator_requires_the_retained_committed_predecessor_body() {
    let database = Database::new(":memory:").unwrap();
    let predecessor = receipt('a', None, 0xa1);
    stage_and_promote(&database, &predecessor);
    let committed = receipt('b', Some(predecessor.fingerprint()), 0xb1);
    stage_and_promote(&database, &committed);

    database
        .require_promoted_boot_publication_receipt(&committed)
        .unwrap();
    database.delete_boot_publication_receipt_body_for_test(predecessor.fingerprint());
    assert!(matches!(
        database.require_promoted_boot_publication_receipt(&committed),
        Err(BootPublicationReceiptPromotionError::State(
            BootPublicationReceiptStateError::DanglingReference {
                reference: ReceiptReference::CommittedPredecessor,
                ..
            }
        ))
    ));
}
