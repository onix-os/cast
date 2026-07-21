use diesel::{RunQueryDsl as _, connection::SimpleConnection as _};

use super::*;

#[test]
fn committed_and_pending_head_references_must_have_canonical_bodies() {
    let committed = Database::new(":memory:").unwrap();
    let missing_committed = BootPublicationReceiptFingerprint::from_bytes([0x81; 32]);
    committed
        .replace_boot_publication_receipt_head_for_test(Some(missing_committed), None)
        .unwrap();
    assert!(matches!(
        committed.boot_publication_receipt_state(),
        Err(BootPublicationReceiptStateError::DanglingReference {
            reference: ReceiptReference::Committed,
            fingerprint,
        }) if fingerprint == missing_committed
    ));

    let pending = Database::new(":memory:").unwrap();
    let missing_pending = BootPublicationReceiptFingerprint::from_bytes([0x82; 32]);
    let transition = transition('8');
    pending
        .replace_boot_publication_receipt_head_for_test(None, Some((&transition, missing_pending)))
        .unwrap();
    assert!(matches!(
        pending.boot_publication_receipt_state(),
        Err(BootPublicationReceiptStateError::DanglingReference {
            reference: ReceiptReference::Pending,
            fingerprint,
        }) if fingerprint == missing_pending
    ));
}

#[test]
fn tampered_noncanonical_and_hash_mismatched_bodies_fail_closed() {
    let database = Database::new(":memory:").unwrap();
    let original = receipt('9', None, 0x91);
    let replacement = receipt('a', None, 0xa1);
    database.stage_boot_publication_receipt(&original).unwrap();
    database.conn.exec(|connection| {
        diesel::update(
            boot_publication_receipts::table.filter(
                boot_publication_receipts::receipt_sha256
                    .eq(original.fingerprint().as_bytes().as_slice()),
            ),
        )
        .set(boot_publication_receipts::canonical_body.eq(replacement.canonical_body()))
        .execute(connection)
        .unwrap();
    });
    assert!(matches!(
        database.boot_publication_receipt_state(),
        Err(BootPublicationReceiptStateError::BodyFingerprintMismatch { .. })
    ));

    let database = Database::new(":memory:").unwrap();
    let original = receipt('a', None, 0xa2);
    database.stage_boot_publication_receipt(&original).unwrap();
    let mut noncanonical = Vec::with_capacity(original.canonical_body().len() + 1);
    noncanonical.push(b' ');
    noncanonical.extend_from_slice(original.canonical_body());
    database.conn.exec(|connection| {
        diesel::update(
            boot_publication_receipts::table.filter(
                boot_publication_receipts::receipt_sha256
                    .eq(original.fingerprint().as_bytes().as_slice()),
            ),
        )
        .set(boot_publication_receipts::canonical_body.eq(noncanonical))
        .execute(connection)
        .unwrap();
    });
    assert!(matches!(
        database.boot_publication_receipt_state(),
        Err(BootPublicationReceiptStateError::Codec(
            BootPublicationReceiptCodecError::NonCanonicalBody
        ))
    ));
}

#[test]
fn mistyped_and_oversized_bodies_fail_before_typed_body_loading() {
    let mistyped = Database::new(":memory:").unwrap();
    let mistyped_receipt = receipt('b', None, 0xb1);
    mistyped
        .stage_boot_publication_receipt(&mistyped_receipt)
        .unwrap();
    mistyped.conn.exec(|connection| {
        connection.batch_execute("PRAGMA ignore_check_constraints = ON").unwrap();
        diesel::sql_query(
            "UPDATE boot_publication_receipts SET canonical_body = CAST(canonical_body AS TEXT)",
        )
        .execute(connection)
        .unwrap();
        connection.batch_execute("PRAGMA ignore_check_constraints = OFF").unwrap();
    });
    assert!(matches!(
        mistyped.boot_publication_receipt_state(),
        Err(BootPublicationReceiptStateError::InvalidStorageType {
            field: "canonical_body",
            expected: "blob",
            ..
        })
    ));

    let oversized = Database::new(":memory:").unwrap();
    let oversized_receipt = receipt('c', None, 0xc1);
    oversized
        .stage_boot_publication_receipt(&oversized_receipt)
        .unwrap();
    oversized.conn.exec(|connection| {
        connection.batch_execute("PRAGMA ignore_check_constraints = ON").unwrap();
        diesel::sql_query(
            "UPDATE boot_publication_receipts SET canonical_body = zeroblob(16777217)",
        )
        .execute(connection)
        .unwrap();
        connection.batch_execute("PRAGMA ignore_check_constraints = OFF").unwrap();
    });
    assert!(matches!(
        oversized.boot_publication_receipt_state(),
        Err(BootPublicationReceiptStateError::InvalidStoredBodyLength {
            actual: 16_777_217,
            limit: 16_777_216,
        })
    ));
}

#[test]
fn row_head_transition_and_pending_predecessor_linkage_is_exact() {
    let row_transition = Database::new(":memory:").unwrap();
    let row_receipt = receipt('d', None, 0xd1);
    row_transition
        .stage_boot_publication_receipt(&row_receipt)
        .unwrap();
    row_transition.conn.exec(|connection| {
        diesel::update(boot_publication_receipts::table)
            .set(boot_publication_receipts::transition_id.eq(transition('e').as_str()))
            .execute(connection)
            .unwrap();
    });
    assert!(matches!(
        row_transition.boot_publication_receipt_state(),
        Err(BootPublicationReceiptStateError::BodyTransitionMismatch { .. })
    ));

    let head_transition = Database::new(":memory:").unwrap();
    let head_receipt = receipt('d', None, 0xd2);
    insert_raw_receipt(&head_transition, &head_receipt);
    let foreign = transition('e');
    head_transition
        .replace_boot_publication_receipt_head_for_test(None, Some((&foreign, head_receipt.fingerprint())))
        .unwrap();
    assert!(matches!(
        head_transition.boot_publication_receipt_state(),
        Err(BootPublicationReceiptStateError::PendingTransitionMismatch { .. })
    ));

    let predecessor = Database::new(":memory:").unwrap();
    let committed = receipt('d', None, 0xd3);
    let pending = receipt('e', None, 0xe3);
    insert_raw_receipt(&predecessor, &committed);
    insert_raw_receipt(&predecessor, &pending);
    predecessor
        .replace_boot_publication_receipt_head_for_test(
            Some(committed.fingerprint()),
            Some((pending.body().transition_id(), pending.fingerprint())),
        )
        .unwrap();
    assert!(matches!(
        predecessor.boot_publication_receipt_state(),
        Err(BootPublicationReceiptStateError::PendingPredecessorMismatch { .. })
    ));
}

#[test]
fn a_conflicting_preexisting_body_cannot_be_adopted_or_stage_the_head() {
    let exact_orphan = Database::new(":memory:").unwrap();
    let exact = receipt('1', None, 0xf0);
    insert_raw_receipt(&exact_orphan, &exact);
    assert!(matches!(
        exact_orphan.stage_boot_publication_receipt(&exact),
        Err(BootPublicationReceiptStateError::OrphanTransitionConflict {
            existing_fingerprint,
            requested_fingerprint,
            ..
        }) if existing_fingerprint == exact.fingerprint()
            && requested_fingerprint == exact.fingerprint()
    ));
    assert_eq!(receipt_row_count(&exact_orphan), 1);
    assert_eq!(exact_orphan.boot_publication_receipt_head().unwrap().pending(), None);

    let database = Database::new(":memory:").unwrap();
    let requested = receipt('1', None, 0xf1);
    let foreign = receipt('2', None, 0xf2);
    database.conn.exec(|connection| {
        diesel::sql_query(
            "INSERT INTO boot_publication_receipts (receipt_sha256, transition_id, canonical_body) \
             VALUES (?, ?, ?)",
        )
        .bind::<Binary, _>(requested.fingerprint().as_bytes().as_slice())
        .bind::<Text, _>(foreign.body().transition_id().as_str())
        .bind::<Binary, _>(foreign.canonical_body())
        .execute(connection)
        .unwrap();
    });

    assert!(matches!(
        database.stage_boot_publication_receipt(&requested),
        Err(BootPublicationReceiptStateError::BodyFingerprintMismatch { .. })
    ));
    assert_eq!(receipt_row_count(&database), 1);
    assert_eq!(database.boot_publication_receipt_head().unwrap().pending(), None);
}
