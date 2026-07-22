use diesel::connection::SimpleConnection as _;

use super::*;

#[test]
fn dangling_and_tampered_pending_bodies_fail_before_head_mutation() {
    let dangling = Database::new(":memory:").unwrap();
    let pending = receipt('d', None, 0xd1);
    stage(&dangling, &pending);
    let head = dangling.boot_publication_receipt_head().unwrap();
    dangling.delete_boot_publication_receipt_body_for_test(pending.fingerprint());
    assert!(matches!(
        dangling.promote_boot_publication_receipt(&pending, promotion_deadline()),
        Err(BootPublicationReceiptPromotionError::State(
            BootPublicationReceiptStateError::DanglingReference {
                reference: ReceiptReference::Pending,
                ..
            }
        ))
    ));
    assert_eq!(dangling.boot_publication_receipt_head().unwrap(), head);

    let tampered = Database::new(":memory:").unwrap();
    let pending = receipt('e', None, 0xe1);
    let foreign = receipt('f', None, 0xf1);
    stage(&tampered, &pending);
    let head = tampered.boot_publication_receipt_head().unwrap();
    tampered.conn.exec(|connection| {
        let changed = diesel::update(
            boot_publication_receipts::table.filter(
                boot_publication_receipts::receipt_sha256
                    .eq(pending.fingerprint().as_bytes().as_slice()),
            ),
        )
        .set(boot_publication_receipts::canonical_body.eq(foreign.canonical_body()))
        .execute(connection)
        .unwrap();
        assert_eq!(changed, 1);
    });
    assert!(matches!(
        tampered.promote_boot_publication_receipt(&pending, promotion_deadline()),
        Err(BootPublicationReceiptPromotionError::State(
            BootPublicationReceiptStateError::BodyFingerprintMismatch { .. }
        ))
    ));
    assert_eq!(tampered.boot_publication_receipt_head().unwrap(), head);
}

#[test]
fn dangling_and_tampered_committed_predecessors_block_promotion() {
    let dangling = Database::new(":memory:").unwrap();
    let predecessor = receipt('1', None, 0x12);
    stage_and_promote(&dangling, &predecessor);
    let pending = receipt('2', Some(predecessor.fingerprint()), 0x22);
    stage(&dangling, &pending);
    let head = dangling.boot_publication_receipt_head().unwrap();
    dangling.delete_boot_publication_receipt_body_for_test(predecessor.fingerprint());
    assert!(matches!(
        dangling.promote_boot_publication_receipt(&pending, promotion_deadline()),
        Err(BootPublicationReceiptPromotionError::State(
            BootPublicationReceiptStateError::DanglingReference {
                reference: ReceiptReference::Committed,
                ..
            }
        ))
    ));
    assert_eq!(dangling.boot_publication_receipt_head().unwrap(), head);

    let tampered = Database::new(":memory:").unwrap();
    let predecessor = receipt('3', None, 0x32);
    stage_and_promote(&tampered, &predecessor);
    let pending = receipt('4', Some(predecessor.fingerprint()), 0x42);
    stage(&tampered, &pending);
    let head = tampered.boot_publication_receipt_head().unwrap();
    tampered.conn.exec(|connection| {
        let changed = diesel::update(
            boot_publication_receipts::table.filter(
                boot_publication_receipts::receipt_sha256
                    .eq(predecessor.fingerprint().as_bytes().as_slice()),
            ),
        )
        .set(boot_publication_receipts::canonical_body.eq(pending.canonical_body()))
        .execute(connection)
        .unwrap();
        assert_eq!(changed, 1);
    });
    assert!(matches!(
        tampered.promote_boot_publication_receipt(&pending, promotion_deadline()),
        Err(BootPublicationReceiptPromotionError::State(
            BootPublicationReceiptStateError::BodyFingerprintMismatch { .. }
        ))
    ));
    assert_eq!(tampered.boot_publication_receipt_head().unwrap(), head);
}

#[test]
fn terminal_validation_and_exact_retry_require_the_committed_predecessor_body() {
    let terminal = Database::new(":memory:").unwrap();
    let predecessor = receipt('5', None, 0x52);
    stage_and_promote(&terminal, &predecessor);
    let pending = receipt('6', Some(predecessor.fingerprint()), 0x62);
    stage(&terminal, &pending);
    let before = terminal.boot_publication_receipt_state().unwrap();
    terminal.conn.exec(|connection| {
        connection
            .batch_execute(
                "CREATE TRIGGER remove_predecessor_after_promotion \
                 AFTER UPDATE OF committed_receipt_sha256 \
                 ON boot_publication_receipt_head \
                 WHEN OLD.committed_receipt_sha256 IS NOT NULL \
                 BEGIN \
                     DELETE FROM boot_publication_receipts \
                     WHERE receipt_sha256 = OLD.committed_receipt_sha256; \
                 END;",
            )
            .unwrap();
    });
    assert!(matches!(
        terminal.promote_boot_publication_receipt(&pending, promotion_deadline()),
        Err(BootPublicationReceiptPromotionError::State(
            BootPublicationReceiptStateError::DanglingReference {
                reference: ReceiptReference::CommittedPredecessor,
                ..
            }
        ))
    ));
    assert_eq!(terminal.boot_publication_receipt_state().unwrap(), before);
    assert_eq!(receipt_row_count(&terminal), 2);

    let retry = Database::new(":memory:").unwrap();
    let predecessor = receipt('7', None, 0x72);
    stage_and_promote(&retry, &predecessor);
    let committed = receipt('8', Some(predecessor.fingerprint()), 0x82);
    stage_and_promote(&retry, &committed);
    retry.delete_boot_publication_receipt_body_for_test(predecessor.fingerprint());
    assert!(matches!(
        retry.promote_boot_publication_receipt(&committed, promotion_deadline()),
        Err(BootPublicationReceiptPromotionError::State(
            BootPublicationReceiptStateError::DanglingReference {
                reference: ReceiptReference::CommittedPredecessor,
                ..
            }
        ))
    ));
}
