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
use gluon_config::GLUON_GENERATED_MARKER;
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

    /// Enumerate the content addresses of every stored blob (its `<sha256>.lua`
    /// filename with the extension stripped). Returns an empty list when the
    /// blob directory does not yet exist.
    pub(crate) fn addresses(&self) -> Result<Vec<String>, BlobStoreError> {
        let dir = self.blobs_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut addresses = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                if let Some(address) = name.strip_suffix(".lua") {
                    addresses.push(address.to_owned());
                }
            }
        }
        Ok(addresses)
    }

    /// Remove one blob by content address. Used only by the retained-authority
    /// garbage-collection pass after it has proven no committed row references
    /// the blob.
    pub(crate) fn remove(&self, sha256: &str) -> Result<(), BlobStoreError> {
        fs::remove_file(self.blob_path(sha256))?;
        Ok(())
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

/// Rooted enumeration of every remaining generated Gluon authority beneath a
/// mutable config store root: any file named `*.glu` whose first bytes are the
/// generated ownership marker. After a mutable-store migration switches its
/// generated-slot language to Lua, this must return empty — it is the proof
/// that no generated `.glu` authority remains, complementing the state catalog.
/// Authored `.glu` files (which carry no generated marker) are never included.
pub(crate) fn generated_gluon_authorities(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    collect_generated_gluon_authorities(root, &mut found)?;
    found.sort();
    Ok(found)
}

fn collect_generated_gluon_authorities(dir: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            collect_generated_gluon_authorities(&path, found)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("glu") {
            let bytes = fs::read(&path)?;
            if bytes.starts_with(GLUON_GENERATED_MARKER.as_bytes()) {
                found.push(path);
            }
        }
    }
    Ok(())
}

/// Aggregate coverage of the required state-owned declaration slots: which have
/// a committed catalog row and which remain unmigrated. A future Lua-only
/// release refuses upgrade unless coverage is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclarationMigrationCoverage {
    pub covered: Vec<(i32, String)>,
    pub missing: Vec<(i32, String)>,
}

impl DeclarationMigrationCoverage {
    /// True when every required state-owned slot has a committed row.
    pub(crate) fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// Report catalog coverage of every required `(state_id, logical_slot)` pair.
pub(crate) fn migration_coverage(
    database: &Database,
    required: &[(i32, String)],
) -> Result<DeclarationMigrationCoverage, BridgeError> {
    let mut covered = Vec::new();
    let mut missing = Vec::new();
    for (state_id, logical_slot) in required {
        if database.declaration_migration(*state_id, logical_slot)?.is_some() {
            covered.push((*state_id, logical_slot.clone()));
        } else {
            missing.push((*state_id, logical_slot.clone()));
        }
    }
    Ok(DeclarationMigrationCoverage { covered, missing })
}

/// Operator readiness gate for removing the legacy Gluon authority: a stronger
/// precondition than catalog coverage. A slot is *ready* only when it has a
/// committed row *and* that row's blob resolves and revalidates its content
/// address on disk. This separates "a row claims the slot is migrated" from
/// "the migrated bytes are actually durable and intact" — a committed row whose
/// blob is missing or corrupt is [`unresolved`](Self::unresolved), never ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclarationMigrationReadiness {
    /// Slots with a committed row whose blob resolved and revalidated.
    pub ready: Vec<(i32, String)>,
    /// Slots with no committed row.
    pub missing: Vec<(i32, String)>,
    /// Slots with a committed row whose blob failed to resolve or revalidate.
    pub unresolved: Vec<(i32, String)>,
}

impl DeclarationMigrationReadiness {
    /// True only when every required slot is migrated *and* its blob resolves —
    /// the condition under which dropping the Gluon authority is safe.
    pub(crate) fn is_ready(&self) -> bool {
        self.missing.is_empty() && self.unresolved.is_empty()
    }
}

/// Verify that every required slot is not merely cataloged but backed by a
/// resolvable, content-valid blob. A catalog-covered slot whose blob is missing
/// or fails its content-address check is reported as `unresolved` rather than
/// aborting the scan, so an operator sees every problem in one pass. Any blob
/// read failure fails closed (the slot is not ready); only a genuine catalog
/// query error propagates.
pub(crate) fn verify_migration_readiness(
    database: &Database,
    blobs: &DeclarationMigrationBlobStore,
    required: &[(i32, String)],
) -> Result<DeclarationMigrationReadiness, BridgeError> {
    let mut ready = Vec::new();
    let mut missing = Vec::new();
    let mut unresolved = Vec::new();
    for (state_id, logical_slot) in required {
        match database.declaration_migration(*state_id, logical_slot)? {
            None => missing.push((*state_id, logical_slot.clone())),
            Some(row) => match blobs.read(&hex::encode(&row.migrated_blob_sha256)) {
                Ok(_) => ready.push((*state_id, logical_slot.clone())),
                Err(_) => unresolved.push((*state_id, logical_slot.clone())),
            },
        }
    }
    Ok(DeclarationMigrationReadiness {
        ready,
        missing,
        unresolved,
    })
}

/// Deferred, retained-authority garbage collection: remove every blob that no
/// committed catalog row references, and keep every blob that one does. This
/// runs only after the catalog is the sole selection authority, so a crash that
/// left an unreachable blob is cleaned up here — but a blob still referenced by
/// a committed row is never removed. Returns the addresses collected.
pub(crate) fn collect_unreferenced_blobs(
    database: &Database,
    blobs: &DeclarationMigrationBlobStore,
) -> Result<Vec<String>, BridgeError> {
    let referenced: std::collections::HashSet<String> = database
        .referenced_migration_blobs()?
        .iter()
        .map(hex::encode)
        .collect();

    let mut collected = Vec::new();
    for address in blobs.addresses()? {
        if !referenced.contains(&address) {
            blobs.remove(&address)?;
            collected.push(address);
        }
    }
    Ok(collected)
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
    fn readiness_requires_a_resolvable_blob_not_merely_a_catalog_row() {
        let (_dir, blobs) = store();
        let database = database();
        let state_id = state_id(&database);
        let converted = b"return { ready = true }\n";
        // Matches the slot `request` records.
        let required = vec![(state_id, "etc/cast/system.glu".to_owned())];

        // Before migration the slot is missing and not ready.
        let before = verify_migration_readiness(&database, &blobs, &required).unwrap();
        assert_eq!(before.missing, required);
        assert!(before.ready.is_empty() && before.unresolved.is_empty());
        assert!(!before.is_ready());

        // After migration the slot has a committed row and a resolvable blob.
        migrate_declaration(&database, &blobs, request(state_id, converted)).unwrap();
        let ready = verify_migration_readiness(&database, &blobs, &required).unwrap();
        assert_eq!(ready.ready, required);
        assert!(ready.is_ready());

        // Corrupt the committed blob: catalog coverage still reports the slot
        // complete, but the stronger readiness gate reports it unresolved and
        // refuses to declare the state ready to drop its Gluon authority.
        fs::write(blobs.blob_path(&content_address(converted)), b"tampered\n").unwrap();
        assert!(migration_coverage(&database, &required).unwrap().is_complete());
        let after = verify_migration_readiness(&database, &blobs, &required).unwrap();
        assert_eq!(after.unresolved, required);
        assert!(after.ready.is_empty() && after.missing.is_empty());
        assert!(!after.is_ready());
    }

    #[test]
    fn rooted_enumeration_finds_only_remaining_generated_gluon_authority() {
        use lua_config::GENERATED_LUA_MARKER;

        let root = tempfile::tempdir().unwrap();
        let fragments = root.path().join("repo.d");
        fs::create_dir_all(&fragments).unwrap();
        // A generated Gluon authority (what a migration must eliminate).
        fs::write(
            fragments.join("main.glu"),
            format!("{GLUON_GENERATED_MARKER}[]\n"),
        )
        .unwrap();
        // A generated Lua authority (the migration target) and an authored
        // `.glu` with no generated marker — neither is a remaining generated
        // Gluon authority.
        fs::write(fragments.join("main.lua"), format!("{GENERATED_LUA_MARKER}return {{}}\n")).unwrap();
        fs::write(fragments.join("authored.glu"), "[]\n").unwrap();

        let remaining = generated_gluon_authorities(root.path()).unwrap();
        assert_eq!(remaining, vec![fragments.join("main.glu")]);

        // After the generated `.glu` authority is removed, none remains.
        fs::remove_file(fragments.join("main.glu")).unwrap();
        assert!(generated_gluon_authorities(root.path()).unwrap().is_empty());
    }

    #[test]
    fn a_failed_catalog_commit_leaves_no_row_and_orphans_the_blob() {
        // Simulate a crash during the exclusive commit: the blob is made durable
        // first, then a catalog CHECK violation (an invalid state-tree marker)
        // aborts the transaction. The exclusive transaction is atomic, so no row
        // is written; the durable blob is left as unreachable residue.
        let (_dir, blobs) = store();
        let database = database();
        let sid = state_id(&database);
        let converted = b"return { half_written = false }\n";
        let mut request = request(sid, converted);
        request.state_tree_marker = vec![1u8; 8]; // not 32 bytes: fails the CHECK

        assert!(migrate_declaration(&database, &blobs, request).is_err());
        assert_eq!(
            resolve_migrated_blob(&database, &blobs, sid, "etc/cast/system.glu").unwrap(),
            None
        );
        assert!(blobs.read(&content_address(converted)).is_ok());
    }

    #[test]
    fn coverage_reports_covered_and_missing_state_slots() {
        let (_dir, blobs) = store();
        let database = database();
        let sid = state_id(&database);
        migrate_declaration(&database, &blobs, request(sid, b"a")).unwrap();

        let required = vec![
            (sid, "etc/cast/system.glu".to_owned()),
            (sid, "usr/lib/system-model.glu".to_owned()),
        ];
        let coverage = migration_coverage(&database, &required).unwrap();

        assert!(!coverage.is_complete());
        assert_eq!(coverage.covered, vec![(sid, "etc/cast/system.glu".to_owned())]);
        assert_eq!(coverage.missing, vec![(sid, "usr/lib/system-model.glu".to_owned())]);
    }

    #[test]
    fn garbage_collection_removes_only_unreferenced_blobs() {
        let (_dir, blobs) = store();
        let database = database();
        let sid = state_id(&database);
        let referenced = b"return { referenced = true }\n";
        migrate_declaration(&database, &blobs, request(sid, referenced)).unwrap();
        let referenced_address = content_address(referenced);

        // An orphan blob with no committed row (e.g. left by a crash before the
        // catalog commit).
        let orphan_address = blobs.write(b"orphan\n").unwrap();

        let collected = collect_unreferenced_blobs(&database, &blobs).unwrap();

        assert_eq!(collected, vec![orphan_address.clone()]);
        // The referenced blob survives and still resolves; the orphan is gone.
        assert!(blobs.read(&referenced_address).is_ok());
        assert!(matches!(blobs.read(&orphan_address), Err(BlobStoreError::Io(_))));
        assert_eq!(
            resolve_migrated_blob(&database, &blobs, sid, "etc/cast/system.glu").unwrap(),
            Some(referenced.to_vec())
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
    fn pruning_a_state_cascades_its_catalog_row_and_leaves_an_orphan_blob() {
        let (_dir, blobs) = store();
        let database = database();
        let sid = state_id(&database);
        let converted = b"return { emul32 = false }\n";
        migrate_declaration(&database, &blobs, request(sid, converted)).unwrap();
        let address = content_address(converted);

        // Prune the state transactionally.
        database.remove(&crate::state::Id::from(sid)).unwrap();

        // The catalog authority is cascaded away — the slot is unmigrated again.
        assert_eq!(
            resolve_migrated_blob(&database, &blobs, sid, "etc/cast/system.glu").unwrap(),
            None
        );
        // The content-addressed blob survives as unreachable residue; a later
        // retained-authority GC pass removes it, never the prune transaction.
        assert!(blobs.read(&address).is_ok());
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
