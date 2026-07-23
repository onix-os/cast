use diesel::{RunQueryDsl as _, sql_types::Integer};

use super::*;

#[test]
fn malformed_committed_and_pending_fingerprint_lengths_fail_closed() {
    let database = Database::new(":memory:").unwrap();

    let mut committed = empty_raw_head();
    committed.committed_receipt_sha256 = Some(vec![0_u8; 31]);
    database
        .replace_boot_publication_receipt_head_raw_for_test(&committed)
        .unwrap();
    assert!(matches!(
        database.boot_publication_receipt_head(),
        Err(BootPublicationReceiptHeadError::InvalidStoredFieldLength {
            field: "committed_receipt_sha256",
            ..
        })
    ));
    assert!(matches!(
        database.stage_boot_publication_receipt_pair(
            &transition('b'),
            &BootPublicationReceiptPair {
                committed: None,
                pending: fingerprint(0xb0),
            },
        ),
        Err(BootPublicationReceiptHeadError::InvalidStoredFieldLength {
            field: "committed_receipt_sha256",
            ..
        })
    ));

    let mut pending = empty_raw_head();
    pending.pending_transition_id = Some(transition('b').to_string());
    pending.pending_receipt_sha256 = Some(vec![0_u8; 33]);
    database
        .replace_boot_publication_receipt_head_raw_for_test(&pending)
        .unwrap();
    assert!(matches!(
        database.boot_publication_receipt_head(),
        Err(BootPublicationReceiptHeadError::InvalidStoredFieldLength {
            field: "pending_receipt_sha256",
            ..
        })
    ));
}

#[test]
fn partial_pending_rows_and_noncanonical_transition_ids_fail_closed() {
    let database = Database::new(":memory:").unwrap();

    for raw in [
        BootPublicationReceiptHeadRawForTest {
            committed_receipt_sha256: None,
            pending_transition_id: Some(transition('c').to_string()),
            pending_receipt_sha256: None,
        },
        BootPublicationReceiptHeadRawForTest {
            committed_receipt_sha256: None,
            pending_transition_id: None,
            pending_receipt_sha256: Some(vec![0xcc; 32]),
        },
    ] {
        database
            .replace_boot_publication_receipt_head_raw_for_test(&raw)
            .unwrap();
        assert!(matches!(
            database.boot_publication_receipt_head(),
            Err(BootPublicationReceiptHeadError::IncompletePendingPair { .. })
        ));
    }

    let malformed = BootPublicationReceiptHeadRawForTest {
        committed_receipt_sha256: None,
        pending_transition_id: Some("g".repeat(TransitionId::TEXT_LENGTH)),
        pending_receipt_sha256: Some(vec![0xcd; 32]),
    };
    database
        .replace_boot_publication_receipt_head_raw_for_test(&malformed)
        .unwrap();
    assert!(matches!(
        database.boot_publication_receipt_head(),
        Err(BootPublicationReceiptHeadError::InvalidPendingTransitionId { .. })
    ));
}

#[test]
fn missing_wrong_and_duplicate_singletons_are_rejected_by_bounded_inspection() {
    let missing = Database::new(":memory:").unwrap();
    missing.delete_boot_publication_receipt_head_for_test().unwrap();
    assert!(matches!(
        missing.boot_publication_receipt_head(),
        Err(BootPublicationReceiptHeadError::MissingSingleton)
    ));

    let wrong = Database::new(":memory:").unwrap();
    wrong.conn.exec(|conn| {
        diesel::sql_query("PRAGMA ignore_check_constraints = ON")
            .execute(conn)
            .unwrap();
        diesel::sql_query("UPDATE boot_publication_receipt_head SET singleton = 2")
            .execute(conn)
            .unwrap();
        diesel::sql_query("PRAGMA ignore_check_constraints = OFF")
            .execute(conn)
            .unwrap();
    });
    assert!(matches!(
        wrong.boot_publication_receipt_head(),
        Err(BootPublicationReceiptHeadError::InvalidSingleton { actual: 2 })
    ));

    let duplicate = Database::new(":memory:").unwrap();
    duplicate.conn.exec(|conn| {
        diesel::sql_query("PRAGMA ignore_check_constraints = ON")
            .execute(conn)
            .unwrap();
        diesel::sql_query(
            "INSERT INTO boot_publication_receipt_head (singleton) VALUES (?)",
        )
        .bind::<Integer, _>(2)
        .execute(conn)
        .unwrap();
        diesel::sql_query("PRAGMA ignore_check_constraints = OFF")
            .execute(conn)
            .unwrap();
    });
    assert!(matches!(
        duplicate.boot_publication_receipt_head(),
        Err(BootPublicationReceiptHeadError::MultipleRows)
    ));
}

#[test]
fn dynamically_mistyped_storage_is_a_typed_error_not_adopted_evidence() {
    let database = Database::new(":memory:").unwrap();
    database.conn.exec(|conn| {
        diesel::sql_query("PRAGMA ignore_check_constraints = ON")
            .execute(conn)
            .unwrap();
        diesel::sql_query(
            "UPDATE boot_publication_receipt_head \
             SET committed_receipt_sha256 = CAST('00000000000000000000000000000000' AS TEXT)",
        )
        .execute(conn)
        .unwrap();
        diesel::sql_query("PRAGMA ignore_check_constraints = OFF")
            .execute(conn)
            .unwrap();
    });

    assert!(matches!(
        database.boot_publication_receipt_head(),
        Err(BootPublicationReceiptHeadError::InvalidStorageType {
            field: "committed_receipt_sha256",
            expected: "blob",
            ..
        })
    ));
}
