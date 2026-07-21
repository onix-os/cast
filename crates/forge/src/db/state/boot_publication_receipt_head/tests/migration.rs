use diesel::{RunQueryDsl as _, sql_types::{Binary, Integer, Nullable, Text}};
use diesel_migrations::MigrationHarness as _;

use super::*;
use crate::db::state::{MIGRATIONS, MetadataProvenance};

#[test]
fn prior_state_schema_upgrades_without_losing_state_or_provenance() {
    let database = Database::new(":memory:").unwrap();
    database.conn.exec(|conn| {
        conn.revert_last_migration(MIGRATIONS).unwrap();
    });
    let transition_id = transition('e');
    let state = database
        .add_with_transition(&transition_id, &[], Some("preexisting"), Some("preserve me"))
        .unwrap();
    let provenance = MetadataProvenance::from_outputs(b"old os-release", b"old system model");
    database
        .insert_fresh_metadata_provenance_if_transition_matches(state.id, &transition_id, &provenance)
        .unwrap();

    database.conn.exec(|conn| {
        conn.run_pending_migrations(MIGRATIONS).unwrap();
    });
    let rows = database.conn.exec(|conn| {
        diesel::sql_query(
            "SELECT singleton, committed_receipt_sha256, pending_transition_id, pending_receipt_sha256 \
             FROM boot_publication_receipt_head ORDER BY singleton",
        )
        .load::<StoredMigrationHead>(conn)
        .unwrap()
    });

    assert_eq!(
        rows,
        vec![StoredMigrationHead {
            singleton: 1,
            committed_receipt_sha256: None,
            pending_transition_id: None,
            pending_receipt_sha256: None,
        }]
    );
    assert_eq!(database.get_by_transition(&transition_id).unwrap(), Some(state.clone()));
    database
        .required_metadata_provenance(state.id)
        .unwrap()
        .require_outputs(state.id, b"old os-release", b"old system model")
        .unwrap();
}

#[test]
fn migration_rejects_non_singleton_rows_and_invalid_fingerprint_storage() {
    let database = Database::new(":memory:").unwrap();

    for statement in [
        "INSERT INTO boot_publication_receipt_head (singleton) VALUES (2)",
        "UPDATE boot_publication_receipt_head SET singleton = 0",
        "UPDATE boot_publication_receipt_head SET committed_receipt_sha256 = X'00'",
        "UPDATE boot_publication_receipt_head SET committed_receipt_sha256 = zeroblob(33)",
        "UPDATE boot_publication_receipt_head SET committed_receipt_sha256 = '00000000000000000000000000000000'",
        "UPDATE boot_publication_receipt_head SET pending_transition_id = '11111111111111111111111111111111'",
        "UPDATE boot_publication_receipt_head SET pending_receipt_sha256 = zeroblob(32)",
        "UPDATE boot_publication_receipt_head SET pending_transition_id = '11111111111111111111111111111111', pending_receipt_sha256 = X'00'",
        "UPDATE boot_publication_receipt_head SET pending_transition_id = '11111111111111111111111111111111', pending_receipt_sha256 = zeroblob(33)",
        "UPDATE boot_publication_receipt_head SET pending_transition_id = 'GGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG', pending_receipt_sha256 = zeroblob(32)",
        "UPDATE boot_publication_receipt_head SET pending_transition_id = '1111111111111111111111111111111A', pending_receipt_sha256 = zeroblob(32)",
    ] {
        assert!(database.conn.exec(|conn| diesel::sql_query(statement).execute(conn)).is_err());
        assert_eq!(
            database.boot_publication_receipt_head().unwrap(),
            BootPublicationReceiptHead {
                committed: None,
                pending: None,
            }
        );
    }
}

#[test]
fn transition_constraint_measures_text_bytes_past_embedded_nul() {
    let database = Database::new(":memory:").unwrap();
    let nul_suffix_hex = format!("{}0067", "61".repeat(TransitionId::TEXT_LENGTH));
    let trailing_nul_hex = format!("{}00", "61".repeat(TransitionId::TEXT_LENGTH - 1));

    for encoded in [nul_suffix_hex, trailing_nul_hex] {
        let statement = format!(
            "UPDATE boot_publication_receipt_head \
             SET pending_transition_id = CAST(X'{encoded}' AS TEXT), \
                 pending_receipt_sha256 = zeroblob(32)"
        );
        assert!(
            database
                .conn
                .exec(|conn| diesel::sql_query(statement).execute(conn))
                .is_err()
        );
    }
    assert_eq!(database.boot_publication_receipt_head().unwrap().pending(), None);
}

#[test]
fn migration_accepts_only_the_complete_canonical_storage_shapes() {
    let database = Database::new(":memory:").unwrap();
    let transition_id = transition('d');
    database.conn.exec(|conn| {
        diesel::sql_query(
            "UPDATE boot_publication_receipt_head \
             SET committed_receipt_sha256 = ?, pending_transition_id = ?, pending_receipt_sha256 = ?",
        )
        .bind::<Binary, _>(vec![0xdd; 32])
        .bind::<Text, _>(transition_id.as_str())
        .bind::<Binary, _>(vec![0xde; 32])
        .execute(conn)
        .unwrap();
    });

    let head = database.boot_publication_receipt_head().unwrap();
    assert_eq!(head.committed(), Some(fingerprint(0xdd)));
    assert_eq!(
        head.receipt_pair_for(&transition_id),
        Some(BootPublicationReceiptPair {
            committed: Some(fingerprint(0xdd)),
            pending: fingerprint(0xde),
        })
    );
}

#[derive(Debug, diesel::QueryableByName, Eq, PartialEq)]
struct StoredMigrationHead {
    #[diesel(sql_type = Integer)]
    singleton: i32,
    #[diesel(sql_type = Nullable<Binary>)]
    committed_receipt_sha256: Option<Vec<u8>>,
    #[diesel(sql_type = Nullable<Text>)]
    pending_transition_id: Option<String>,
    #[diesel(sql_type = Nullable<Binary>)]
    pending_receipt_sha256: Option<Vec<u8>>,
}
