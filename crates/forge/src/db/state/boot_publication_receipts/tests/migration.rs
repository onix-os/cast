use diesel::RunQueryDsl as _;
use diesel_migrations::MigrationHarness as _;

use super::*;
use crate::db::state::MIGRATIONS;

#[test]
fn receipt_table_migration_is_additive_and_initially_empty() {
    let database = Database::new(":memory:").unwrap();
    assert_eq!(receipt_row_count(&database), 0);
    let head = database.boot_publication_receipt_head().unwrap();
    assert_eq!(head.committed(), None);
    assert_eq!(head.pending(), None);

    database.conn.exec(|connection| {
        connection.revert_last_migration(MIGRATIONS).unwrap();
        connection.run_pending_migrations(MIGRATIONS).unwrap();
    });
    assert_eq!(receipt_row_count(&database), 0);
    let state = database.boot_publication_receipt_state().unwrap();
    assert_eq!(state.committed(), None);
    assert_eq!(state.pending(), None);
}

#[test]
fn receipt_table_constraints_reject_invalid_storage_shapes() {
    let database = Database::new(":memory:").unwrap();
    let stored_receipt = receipt('3', None, 0x33);
    let fingerprint = stored_receipt.fingerprint();
    let transition = stored_receipt.body().transition_id();

    for statement in [
        "INSERT INTO boot_publication_receipts VALUES (X'00', '33333333333333333333333333333333', X'7b7d')",
        "INSERT INTO boot_publication_receipts VALUES (zeroblob(33), '33333333333333333333333333333333', X'7b7d')",
        "INSERT INTO boot_publication_receipts VALUES ('00000000000000000000000000000000', '33333333333333333333333333333333', X'7b7d')",
        "INSERT INTO boot_publication_receipts VALUES (zeroblob(32), '3333333333333333333333333333333A', X'7b7d')",
        "INSERT INTO boot_publication_receipts VALUES (zeroblob(32), 'gggggggggggggggggggggggggggggggg', X'7b7d')",
        "INSERT INTO boot_publication_receipts VALUES (zeroblob(32), '33333333333333333333333333333333', X'')",
        "INSERT INTO boot_publication_receipts VALUES (zeroblob(32), '33333333333333333333333333333333', zeroblob(16777217))",
        "INSERT INTO boot_publication_receipts VALUES (zeroblob(32), '33333333333333333333333333333333', 'not-a-blob')",
    ] {
        assert!(database.conn.exec(|connection| diesel::sql_query(statement).execute(connection)).is_err());
        assert_eq!(receipt_row_count(&database), 0);
    }

    database.conn.exec(|connection| {
        diesel::sql_query(
            "INSERT INTO boot_publication_receipts (receipt_sha256, transition_id, canonical_body) \
             VALUES (?, ?, ?)",
        )
        .bind::<Binary, _>(fingerprint.as_bytes().as_slice())
        .bind::<Text, _>(transition.as_str())
        .bind::<Binary, _>(stored_receipt.canonical_body())
        .execute(connection)
        .unwrap();
    });
    assert_eq!(receipt_row_count(&database), 1);

    let same_transition = receipt('3', None, 0x34);
    assert!(database.conn.exec(|connection| {
        diesel::sql_query(
            "INSERT INTO boot_publication_receipts (receipt_sha256, transition_id, canonical_body) \
             VALUES (?, ?, ?)",
        )
        .bind::<Binary, _>(same_transition.fingerprint().as_bytes().as_slice())
        .bind::<Text, _>(same_transition.body().transition_id().as_str())
        .bind::<Binary, _>(same_transition.canonical_body())
        .execute(connection)
    }).is_err());
    assert_eq!(receipt_row_count(&database), 1);
}

#[test]
fn transition_constraint_counts_bytes_after_embedded_nul() {
    let database = Database::new(":memory:").unwrap();
    let nul_suffix_hex = format!("{}0067", "61".repeat(TransitionId::TEXT_LENGTH));
    let statement = format!(
        "INSERT INTO boot_publication_receipts VALUES (zeroblob(32), \
         CAST(X'{nul_suffix_hex}' AS TEXT), X'7b7d')"
    );
    assert!(
        database
            .conn
            .exec(|connection| diesel::sql_query(statement).execute(connection))
            .is_err()
    );
    assert_eq!(receipt_row_count(&database), 0);
}
