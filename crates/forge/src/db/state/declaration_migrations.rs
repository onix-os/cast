//! Durable declaration-migration catalog (Phase L8).
//!
//! One committed row is the single authority that a state-owned declaration
//! slot has been converted to Lua. A committed row is written in one exclusive
//! transaction with no-replace semantics: a second commit for the same
//! `(state_id, logical_slot)` succeeds only when every stored field is
//! byte-identical (an idempotent retry), and otherwise fails closed rather than
//! overwriting. Immutable converted blobs are content-addressed on disk; this
//! module owns only the catalog authority, never blob or filesystem authority.

use diesel::prelude::*;
use diesel::SqliteConnection;

use super::{Database, Error, schema::declaration_migrations};

/// The catalog schema version stamped into every committed row.
pub(crate) const CATALOG_SCHEMA_VERSION: i32 = 1;

/// One catalog row: the immutable record that a state-owned slot was migrated.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, Insertable)]
#[diesel(table_name = declaration_migrations)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct DeclarationMigrationRow {
    pub state_id: i32,
    pub logical_slot: String,
    pub catalog_schema_version: i32,
    pub state_tree_marker: Vec<u8>,
    pub original_language: String,
    pub original_logical_path: String,
    pub original_sha256: Vec<u8>,
    pub migrated_language: String,
    pub migrated_blob_sha256: Vec<u8>,
    pub evaluation_identity: Vec<u8>,
}

/// Outcome of a catalog commit: whether this call inserted the row or found an
/// exact-equal committed row already present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeclarationMigrationCommit {
    Committed,
    AlreadyPresent,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DeclarationMigrationError {
    #[error("declaration-migration catalog database error")]
    Database(#[from] diesel::result::Error),
    #[error(
        "a committed declaration migration for state {state_id} slot {logical_slot:?} does not match the requested row"
    )]
    Mismatch { state_id: i32, logical_slot: String },
    #[error("declaration-migration catalog insert changed {changed} rows")]
    InsertRowMismatch { changed: usize },
}

impl Database {
    /// Commit one catalog row in an exclusive transaction with no-replace
    /// semantics. The database commit is the single authority switch: a row is
    /// inserted only if absent, and a pre-existing row is accepted only after
    /// exact byte equality, otherwise the commit fails closed.
    pub(crate) fn commit_declaration_migration(
        &self,
        row: &DeclarationMigrationRow,
    ) -> Result<DeclarationMigrationCommit, DeclarationMigrationError> {
        self.conn.exclusive_tx(|tx| {
            let existing = declaration_migrations::table
                .find((row.state_id, row.logical_slot.as_str()))
                .select(DeclarationMigrationRow::as_select())
                .first::<DeclarationMigrationRow>(tx)
                .optional()?;

            if let Some(existing) = existing {
                return if &existing == row {
                    Ok(DeclarationMigrationCommit::AlreadyPresent)
                } else {
                    Err(DeclarationMigrationError::Mismatch {
                        state_id: row.state_id,
                        logical_slot: row.logical_slot.clone(),
                    })
                };
            }

            let inserted = diesel::insert_into(declaration_migrations::table)
                .values(row)
                .execute(tx)?;
            if inserted == 1 {
                Ok(DeclarationMigrationCommit::Committed)
            } else {
                Err(DeclarationMigrationError::InsertRowMismatch { changed: inserted })
            }
        })
    }

    /// Every distinct blob content address referenced by a committed catalog
    /// row. The garbage-collection pass keeps exactly these blobs and removes
    /// the rest as unreachable residue.
    pub(crate) fn referenced_migration_blobs(&self) -> Result<Vec<Vec<u8>>, DeclarationMigrationError> {
        self.conn.exec(|conn| {
            declaration_migrations::table
                .select(declaration_migrations::migrated_blob_sha256)
                .distinct()
                .load::<Vec<u8>>(conn)
                .map_err(DeclarationMigrationError::from)
        })
    }

    /// Load the committed catalog row for a state-owned slot, if one exists.
    pub(crate) fn declaration_migration(
        &self,
        state_id: i32,
        logical_slot: &str,
    ) -> Result<Option<DeclarationMigrationRow>, DeclarationMigrationError> {
        self.conn.exec(|conn| {
            declaration_migrations::table
                .find((state_id, logical_slot))
                .select(DeclarationMigrationRow::as_select())
                .first::<DeclarationMigrationRow>(conn)
                .optional()
                .map_err(DeclarationMigrationError::from)
        })
    }
}

/// Transactionally remove the catalog rows of pruned states. Called from the
/// state-prune transaction so pruning a state cascades its catalog authority;
/// the content-addressed blobs it referenced become unreachable residue,
/// removed only by a later retained-authority garbage-collection pass.
pub(super) fn delete_declaration_migrations(
    tx: &mut SqliteConnection,
    states: &[i32],
) -> Result<(), Error> {
    diesel::delete(
        declaration_migrations::table.filter(declaration_migrations::state_id.eq_any(states)),
    )
    .execute(tx)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Database {
        Database::new(":memory:").expect("in-memory state database")
    }

    fn add_state(database: &Database) -> i32 {
        let state = database.add(&[], None, None).expect("state row");
        i32::from(state.id)
    }

    fn row(state_id: i32) -> DeclarationMigrationRow {
        DeclarationMigrationRow {
            state_id,
            logical_slot: "etc/cast/system.glu".to_owned(),
            catalog_schema_version: CATALOG_SCHEMA_VERSION,
            state_tree_marker: vec![1u8; 32],
            original_language: "gluon".to_owned(),
            original_logical_path: "etc/cast/system.glu".to_owned(),
            original_sha256: vec![2u8; 32],
            migrated_language: "lua".to_owned(),
            migrated_blob_sha256: vec![3u8; 32],
            evaluation_identity: vec![4u8, 5, 6],
        }
    }

    #[test]
    fn commit_then_load_round_trips_the_catalog_row() {
        let database = database();
        let state_id = add_state(&database);
        let row = row(state_id);

        assert_eq!(
            database.commit_declaration_migration(&row).unwrap(),
            DeclarationMigrationCommit::Committed
        );
        assert_eq!(
            database.declaration_migration(state_id, "etc/cast/system.glu").unwrap(),
            Some(row)
        );
    }

    #[test]
    fn an_exact_repeat_commit_is_idempotent_but_a_divergent_one_fails_closed() {
        let database = database();
        let state_id = add_state(&database);
        let row = row(state_id);
        database.commit_declaration_migration(&row).unwrap();

        // Byte-identical retry is accepted.
        assert_eq!(
            database.commit_declaration_migration(&row).unwrap(),
            DeclarationMigrationCommit::AlreadyPresent
        );

        // A divergent row for the same slot never overwrites the committed one.
        let mut divergent = row.clone();
        divergent.migrated_blob_sha256 = vec![9u8; 32];
        assert!(matches!(
            database.commit_declaration_migration(&divergent),
            Err(DeclarationMigrationError::Mismatch { .. })
        ));
        // The original row is unchanged.
        assert_eq!(
            database.declaration_migration(state_id, "etc/cast/system.glu").unwrap(),
            Some(row)
        );
    }

    #[test]
    fn a_missing_slot_has_no_committed_row() {
        let database = database();
        let state_id = add_state(&database);
        assert_eq!(
            database.declaration_migration(state_id, "etc/cast/system.glu").unwrap(),
            None
        );
    }
}
