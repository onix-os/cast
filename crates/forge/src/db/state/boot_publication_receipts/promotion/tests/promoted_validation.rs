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

    let correlated = database
        .load_exact_promoted_boot_publication_receipt_state(
            receipt.body().transition_id(),
            &receipt_pair(&receipt),
        )
        .unwrap();
    assert_eq!(correlated, before);
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

    let correlated = database
        .load_exact_promoted_boot_publication_receipt_state(
            committed.body().transition_id(),
            &receipt_pair(&committed),
        )
        .unwrap();
    assert_eq!(correlated.head().committed(), Some(committed.fingerprint()));
    assert_eq!(correlated.committed(), Some(&committed));
    assert!(correlated.head().pending().is_none());
    assert!(correlated.pending().is_none());
    database
        .require_promoted_boot_publication_receipt(&committed)
        .unwrap();
    database.delete_boot_publication_receipt_body_for_test(predecessor.fingerprint());
    assert!(matches!(
        database.load_exact_promoted_boot_publication_receipt_state(
            committed.body().transition_id(),
            &receipt_pair(&committed),
        ),
        Err(ExactPromotedBootPublicationReceiptStateError::State(
            BootPublicationReceiptStateError::DanglingReference {
                reference: ReceiptReference::CommittedPredecessor,
                fingerprint,
            }
        )) if fingerprint == predecessor.fingerprint()
    ));
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

#[test]
fn exact_startup_query_rejects_empty_pending_and_wrong_identity_without_mutation() {
    let empty = Database::new(":memory:").unwrap();
    let expected = receipt('c', None, 0xc1);
    let empty_before = empty.boot_publication_receipt_state().unwrap();
    assert!(matches!(
        empty.load_exact_promoted_boot_publication_receipt_state(
            expected.body().transition_id(),
            &receipt_pair(&expected),
        ),
        Err(ExactPromotedBootPublicationReceiptStateError::CommittedHeadMismatch {
            expected: fingerprint,
            actual: None,
        }) if fingerprint == expected.fingerprint()
    ));
    assert_eq!(empty.boot_publication_receipt_state().unwrap(), empty_before);

    let pending = Database::new(":memory:").unwrap();
    let expected = receipt('d', None, 0xd1);
    stage(&pending, &expected);
    let pending_before = pending.boot_publication_receipt_state().unwrap();
    assert!(matches!(
        pending.load_exact_promoted_boot_publication_receipt_state(
            expected.body().transition_id(),
            &receipt_pair(&expected),
        ),
        Err(ExactPromotedBootPublicationReceiptStateError::PendingHeadPresent {
            transition_id,
            fingerprint,
        }) if transition_id == *expected.body().transition_id()
            && fingerprint == expected.fingerprint()
    ));
    assert_eq!(pending.boot_publication_receipt_state().unwrap(), pending_before);

    let promoted = Database::new(":memory:").unwrap();
    let predecessor = receipt('e', None, 0xe1);
    stage_and_promote(&promoted, &predecessor);
    let expected = receipt('f', Some(predecessor.fingerprint()), 0xf1);
    stage_and_promote(&promoted, &expected);
    let promoted_before = promoted.boot_publication_receipt_state().unwrap();

    let foreign_fingerprint = BootPublicationReceiptFingerprint::from_bytes([0x71; 32]);
    let wrong_fingerprint = BootPublicationReceiptPair {
        committed: Some(predecessor.fingerprint()),
        pending: foreign_fingerprint,
    };
    assert!(matches!(
        promoted.load_exact_promoted_boot_publication_receipt_state(
            expected.body().transition_id(),
            &wrong_fingerprint,
        ),
        Err(ExactPromotedBootPublicationReceiptStateError::CommittedHeadMismatch {
            expected: expected_fingerprint,
            actual: Some(actual),
        }) if expected_fingerprint == foreign_fingerprint && actual == expected.fingerprint()
    ));

    let foreign_transition = transition('7');
    assert!(matches!(
        promoted.load_exact_promoted_boot_publication_receipt_state(
            &foreign_transition,
            &receipt_pair(&expected),
        ),
        Err(ExactPromotedBootPublicationReceiptStateError::TransitionMismatch {
            expected: expected_transition,
            actual,
        }) if expected_transition == foreign_transition
            && actual == *expected.body().transition_id()
    ));

    let wrong_predecessor = BootPublicationReceiptPair {
        committed: None,
        pending: expected.fingerprint(),
    };
    assert!(matches!(
        promoted.load_exact_promoted_boot_publication_receipt_state(
            expected.body().transition_id(),
            &wrong_predecessor,
        ),
        Err(ExactPromotedBootPublicationReceiptStateError::CommittedPredecessorMismatch {
            expected: None,
            actual: Some(actual),
        }) if actual == predecessor.fingerprint()
    ));
    assert_eq!(promoted.boot_publication_receipt_state().unwrap(), promoted_before);
}

#[test]
fn exact_startup_query_rejects_a_corrupt_retained_predecessor_without_mutation() {
    let database = Database::new(":memory:").unwrap();
    let predecessor = receipt('1', None, 0x11);
    stage_and_promote(&database, &predecessor);
    let expected = receipt('2', Some(predecessor.fingerprint()), 0x21);
    stage_and_promote(&database, &expected);
    let head_before = database.boot_publication_receipt_head().unwrap();
    let row_count_before = receipt_row_count(&database);

    database.conn.exec(|connection| {
        let changed = diesel::update(
            boot_publication_receipts::table.filter(
                boot_publication_receipts::receipt_sha256
                    .eq(predecessor.fingerprint().as_bytes().as_slice()),
            ),
        )
        .set(boot_publication_receipts::canonical_body.eq(vec![0xff_u8]))
        .execute(connection)
        .unwrap();
        assert_eq!(changed, 1);
    });

    assert!(matches!(
        database.load_exact_promoted_boot_publication_receipt_state(
            expected.body().transition_id(),
            &receipt_pair(&expected),
        ),
        Err(ExactPromotedBootPublicationReceiptStateError::State(
            BootPublicationReceiptStateError::Codec(_)
        ))
    ));
    assert_eq!(database.boot_publication_receipt_head().unwrap(), head_before);
    assert_eq!(receipt_row_count(&database), row_count_before);
}
