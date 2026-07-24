//! Content-addressed blob store for the declaration-migration bridge (Phase L8).
//!
//! Immutable converted `.lua` bytes live at
//! `<installation-root>/.cast/declaration-migrations/v1/blobs/<sha256>.lua`.
//! The hash in the filename is the content address: a blob is only ever named
//! by the SHA-256 of its own bytes, and every read revalidates that the
//! reopened file still hashes to its name. A committed catalog row selects a
//! blob; a blob with no committed row is unreachable residue. This module owns
//! only the blob filesystem authority, never the catalog authority.

use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use fs_err as fs;
use sha2::{Digest as _, Sha256};

use crate::db::state::{
    CATALOG_SCHEMA_VERSION, Database, DeclarationMigrationCommit, DeclarationMigrationError,
    DeclarationMigrationRow,
};

const BLOB_STORE_RELATIVE: &str = ".cast/declaration-migrations/v1";

/// The content-addressed blob store rooted beneath a retained private `.cast`
/// directory authority.
pub(crate) struct DeclarationMigrationBlobStore {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BlobStoreError {
    #[error("declaration-migration blob I/O error")]
    Io(#[from] io::Error),
    #[error("declaration-migration blob {sha256} exists with different bytes")]
    ContentMismatch { sha256: String },
    #[error("reopened declaration-migration blob {expected} hashes to {actual}")]
    ContentAddressMismatch { expected: String, actual: String },
}

impl DeclarationMigrationBlobStore {
    pub(crate) fn new(installation_root: &Path) -> Self {
        Self {
            root: installation_root.join(BLOB_STORE_RELATIVE),
        }
    }

    fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs")
    }

    fn blob_path(&self, sha256: &str) -> PathBuf {
        self.blobs_dir().join(format!("{sha256}.lua"))
    }

    /// Write `bytes` to their content-addressed blob with no-replace semantics,
    /// synchronize and reopen-verify the file, then synchronize the retained
    /// parent directory. Returns the SHA-256 content address.
    ///
    /// A pre-existing blob is accepted only when its bytes are byte-identical
    /// (an idempotent retry); differing bytes under the same name fail closed
    /// (impossible without a hash collision, but never silently trusted).
    pub(crate) fn write(&self, bytes: &[u8]) -> Result<String, BlobStoreError> {
        let sha256 = content_address(bytes);
        let dir = self.blobs_dir();
        fs::create_dir_all(&dir)?;
        let path = self.blob_path(&sha256);

        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(bytes)?;
                file.sync_all()?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = self.read(&sha256)?;
                return if existing == bytes {
                    Ok(sha256)
                } else {
                    Err(BlobStoreError::ContentMismatch { sha256 })
                };
            }
            Err(error) => return Err(error.into()),
        }

        // Reopen and revalidate the content address before the blob is trusted.
        let reopened = self.read(&sha256)?;
        if reopened != bytes {
            return Err(BlobStoreError::ContentMismatch { sha256 });
        }
        // Synchronize the retained parent directory so the durable name survives.
        fs::File::open(&dir)?.sync_all()?;
        Ok(sha256)
    }

    /// Read one blob by content address, revalidating that the reopened bytes
    /// still hash to the requested name. A mismatch fails closed.
    pub(crate) fn read(&self, sha256: &str) -> Result<Vec<u8>, BlobStoreError> {
        let mut bytes = Vec::new();
        fs::File::open(self.blob_path(sha256))?.read_to_end(&mut bytes)?;
        let actual = content_address(&bytes);
        if actual != sha256 {
            return Err(BlobStoreError::ContentAddressMismatch {
                expected: sha256.to_owned(),
                actual,
            });
        }
        Ok(bytes)
    }
}

fn content_address(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// One state-owned declaration slot to migrate to Lua.
pub(crate) struct DeclarationMigrationRequest {
    pub state_id: i32,
    pub logical_slot: String,
    pub state_tree_marker: Vec<u8>,
    pub original_language: String,
    pub original_logical_path: String,
    pub original_sha256: Vec<u8>,
    pub migrated_language: String,
    pub converted_bytes: Vec<u8>,
    pub evaluation_identity: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BridgeError {
    #[error("declaration-migration blob store error")]
    Blob(#[from] BlobStoreError),
    #[error("declaration-migration catalog error")]
    Catalog(#[from] DeclarationMigrationError),
    #[error(
        "committed declaration migration for slot {logical_slot:?} no longer binds the authenticated state-tree marker"
    )]
    StateTreeMarkerDrift { logical_slot: String },
    #[error(
        "committed declaration migration for slot {logical_slot:?} no longer matches the original source hash"
    )]
    OriginalSourceDrift { logical_slot: String },
}

/// Migrate one state-owned slot: make the converted blob durable *first*
/// (write→sync→reopen-verify→dir-sync), and only then commit the catalog row in
/// one exclusive transaction. A crash after the blob but before the commit
/// leaves an unreachable blob (safe residue); a committed row therefore always
/// points at a durable, verified blob — never the reverse.
pub(crate) fn migrate_declaration(
    database: &Database,
    blobs: &DeclarationMigrationBlobStore,
    request: DeclarationMigrationRequest,
) -> Result<DeclarationMigrationCommit, BridgeError> {
    let blob_address = blobs.write(&request.converted_bytes)?;
    let migrated_blob_sha256 = hex::decode(&blob_address)
        .expect("the blob store returns a lowercase hex content address");

    let row = DeclarationMigrationRow {
        state_id: request.state_id,
        logical_slot: request.logical_slot,
        catalog_schema_version: CATALOG_SCHEMA_VERSION,
        state_tree_marker: request.state_tree_marker,
        original_language: request.original_language,
        original_logical_path: request.original_logical_path,
        original_sha256: request.original_sha256,
        migrated_language: request.migrated_language,
        migrated_blob_sha256,
        evaluation_identity: request.evaluation_identity,
    };
    Ok(database.commit_declaration_migration(&row)?)
}

/// Resolve the migrated blob bytes for a state-owned slot. Only a committed
/// catalog row selects a blob; a blob with no committed row is unreachable
/// residue and yields `None`. The blob content address is revalidated on read.
pub(crate) fn resolve_migrated_blob(
    database: &Database,
    blobs: &DeclarationMigrationBlobStore,
    state_id: i32,
    logical_slot: &str,
) -> Result<Option<Vec<u8>>, BridgeError> {
    let Some(row) = database.declaration_migration(state_id, logical_slot)? else {
        return Ok(None);
    };
    Ok(Some(blobs.read(&hex::encode(&row.migrated_blob_sha256))?))
}

/// Bridge-era selection: resolve a slot's Lua blob only after revalidating that
/// the committed row still binds the authenticated state-tree marker and the
/// original source hash. Any drift fails closed rather than selecting the blob
/// or falling back to the legacy reader — the caller decides that separately.
pub(crate) fn resolve_migrated_blob_revalidated(
    database: &Database,
    blobs: &DeclarationMigrationBlobStore,
    state_id: i32,
    logical_slot: &str,
    expected_state_tree_marker: &[u8],
    expected_original_sha256: &[u8],
) -> Result<Option<Vec<u8>>, BridgeError> {
    let Some(row) = database.declaration_migration(state_id, logical_slot)? else {
        return Ok(None);
    };
    if row.state_tree_marker != expected_state_tree_marker {
        return Err(BridgeError::StateTreeMarkerDrift {
            logical_slot: logical_slot.to_owned(),
        });
    }
    if row.original_sha256 != expected_original_sha256 {
        return Err(BridgeError::OriginalSourceDrift {
            logical_slot: logical_slot.to_owned(),
        });
    }
    Ok(Some(blobs.read(&hex::encode(&row.migrated_blob_sha256))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, DeclarationMigrationBlobStore) {
        let temporary = tempfile::tempdir().unwrap();
        let store = DeclarationMigrationBlobStore::new(temporary.path());
        (temporary, store)
    }

    #[test]
    fn a_written_blob_is_named_by_its_hash_and_reads_back_exactly() {
        let (_dir, store) = store();
        let bytes = b"return { root = \"UUID=1\" }\n";

        let sha256 = store.write(bytes).unwrap();
        assert_eq!(sha256, content_address(bytes));
        assert_eq!(store.read(&sha256).unwrap(), bytes);
    }

    #[test]
    fn writing_the_same_bytes_twice_is_idempotent() {
        let (_dir, store) = store();
        let bytes = b"return {}\n";

        let first = store.write(bytes).unwrap();
        let second = store.write(bytes).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_tampered_blob_fails_the_content_address_check() {
        let (_dir, store) = store();
        let sha256 = store.write(b"original bytes").unwrap();

        // Corrupt the stored blob in place; the content address no longer holds.
        fs::write(store.blob_path(&sha256), b"tampered bytes").unwrap();
        assert!(matches!(
            store.read(&sha256),
            Err(BlobStoreError::ContentAddressMismatch { .. })
        ));
    }

    #[test]
    fn reading_a_missing_blob_is_an_io_error() {
        let (_dir, store) = store();
        assert!(matches!(store.read(&"0".repeat(64)), Err(BlobStoreError::Io(_))));
    }

    fn database() -> Database {
        Database::new(":memory:").expect("in-memory state database")
    }

    fn state_id(database: &Database) -> i32 {
        i32::from(database.add(&[], None, None).expect("state row").id)
    }

    fn request(state_id: i32, converted: &[u8]) -> DeclarationMigrationRequest {
        DeclarationMigrationRequest {
            state_id,
            logical_slot: "etc/cast/system.glu".to_owned(),
            state_tree_marker: vec![1u8; 32],
            original_language: "gluon".to_owned(),
            original_logical_path: "etc/cast/system.glu".to_owned(),
            original_sha256: vec![2u8; 32],
            migrated_language: "lua".to_owned(),
            converted_bytes: converted.to_vec(),
            evaluation_identity: vec![7u8, 8, 9],
        }
    }

    #[test]
    fn a_migrated_slot_resolves_its_converted_blob() {
        let (_dir, blobs) = store();
        let database = database();
        let state_id = state_id(&database);
        let converted = b"return { disable_warning = false }\n";

        assert_eq!(
            migrate_declaration(&database, &blobs, request(state_id, converted)).unwrap(),
            DeclarationMigrationCommit::Committed
        );
        assert_eq!(
            resolve_migrated_blob(&database, &blobs, state_id, "etc/cast/system.glu").unwrap(),
            Some(converted.to_vec())
        );
    }

    #[test]
    fn a_blob_written_without_a_committed_row_is_unreachable_residue() {
        // Simulate a crash after the blob is durable but before the catalog
        // commit: the blob exists on disk, but no committed row selects it, so
        // resolution treats the slot as unmigrated rather than an implicit
        // candidate.
        let (_dir, blobs) = store();
        let database = database();
        let state_id = state_id(&database);
        blobs.write(b"orphan blob\n").unwrap();

        assert_eq!(
            resolve_migrated_blob(&database, &blobs, state_id, "etc/cast/system.glu").unwrap(),
            None
        );
    }

    #[test]
    fn repeating_an_identical_migration_is_idempotent() {
        let (_dir, blobs) = store();
        let database = database();
        let state_id = state_id(&database);
        let converted = b"return {}\n";

        migrate_declaration(&database, &blobs, request(state_id, converted)).unwrap();
        assert_eq!(
            migrate_declaration(&database, &blobs, request(state_id, converted)).unwrap(),
            DeclarationMigrationCommit::AlreadyPresent
        );
    }

    #[test]
    fn revalidated_resolution_selects_the_blob_only_when_marker_and_source_match() {
        let (_dir, blobs) = store();
        let database = database();
        let state_id = state_id(&database);
        let converted = b"return { mold = true }\n";
        // request() uses marker [1;32] and original_sha256 [2;32].
        migrate_declaration(&database, &blobs, request(state_id, converted)).unwrap();

        let marker = vec![1u8; 32];
        let original = vec![2u8; 32];
        assert_eq!(
            resolve_migrated_blob_revalidated(
                &database, &blobs, state_id, "etc/cast/system.glu", &marker, &original,
            )
            .unwrap(),
            Some(converted.to_vec())
        );
    }

    #[test]
    fn revalidated_resolution_fails_closed_on_state_tree_or_source_drift() {
        let (_dir, blobs) = store();
        let database = database();
        let state_id = state_id(&database);
        migrate_declaration(&database, &blobs, request(state_id, b"x")).unwrap();

        let marker = vec![1u8; 32];
        let original = vec![2u8; 32];
        let wrong = vec![0u8; 32];

        assert!(matches!(
            resolve_migrated_blob_revalidated(
                &database, &blobs, state_id, "etc/cast/system.glu", &wrong, &original,
            ),
            Err(BridgeError::StateTreeMarkerDrift { .. })
        ));
        assert!(matches!(
            resolve_migrated_blob_revalidated(
                &database, &blobs, state_id, "etc/cast/system.glu", &marker, &wrong,
            ),
            Err(BridgeError::OriginalSourceDrift { .. })
        ));
    }
}
