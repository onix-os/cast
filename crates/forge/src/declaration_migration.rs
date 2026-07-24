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
}
