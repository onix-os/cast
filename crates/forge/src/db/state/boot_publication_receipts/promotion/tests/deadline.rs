use diesel::connection::SimpleConnection as _;

use super::*;

#[test]
fn deadline_expiring_after_exclusive_admission_blocks_head_mutation() {
    let database = Database::new(":memory:").unwrap();
    let pending = receipt('f', None, 0xf1);
    stage(&database, &pending);
    let before = database.boot_publication_receipt_state().unwrap();
    database.conn.exec(|connection| {
        connection
            .batch_execute(
                "CREATE TRIGGER reject_expired_deadline_head_update \
                 BEFORE UPDATE ON boot_publication_receipt_head \
                 BEGIN SELECT RAISE(ABORT, 'expired promotion reached head mutation'); END;",
            )
            .unwrap();
    });

    let deadline = Instant::now() + Duration::from_secs(60);
    let expired_now = deadline + Duration::from_nanos(1);
    arm_before_head_update(move |_| arm_promotion_deadline_now(expired_now));
    let result = database.promote_boot_publication_receipt(&pending, deadline);

    match result {
        Err(BootPublicationReceiptPromotionError::DeadlineExceeded {
            deadline: actual,
        }) => assert_eq!(actual, deadline),
        other => panic!("unexpected promotion result: {other:?}"),
    }
    assert_eq!(database.boot_publication_receipt_state().unwrap(), before);
    assert_eq!(receipt_row_count(&database), 1);
    assert_eq!(stored_body(&database, pending.fingerprint()), pending.canonical_body());
}

#[test]
fn deadline_equality_at_exclusive_mutation_boundary_allows_promotion() {
    let database = Database::new(":memory:").unwrap();
    let pending = receipt('0', None, 0x01);
    stage(&database, &pending);
    let deadline = Instant::now() + Duration::from_secs(60);
    arm_before_head_update(move |_| arm_promotion_deadline_now(deadline));

    assert_eq!(
        database
            .promote_boot_publication_receipt(&pending, deadline)
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
    let state = database.boot_publication_receipt_state().unwrap();
    assert_eq!(state.head().committed(), Some(pending.fingerprint()));
    assert!(state.head().pending().is_none());
    assert_eq!(state.committed(), Some(&pending));
}
