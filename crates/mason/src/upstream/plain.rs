// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    fs::Permissions,
    io::{self, Read, Write},
    ops::Deref,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    str::FromStr,
};

use forge::{request, util};
use fs_err as fs;
use sha2::{Digest, Sha256};
use stone_recipe::spec::is_canonical_sha256;
use thiserror::Error;
use tui::{ProgressBar, ProgressStyle};
use url::Url;

/// Upstream based on an archive (typically a tarball).
#[derive(Debug, Clone)]
pub struct Plain {
    /// URL from where the source archive is fetched.
    pub url: Url,
    /// SHA256 hash of the archive.
    pub hash: Hash,
    /// Name of the upstream when stored in the storage
    /// directory. If None, a default name is implied from [Self::url].
    pub rename: Option<String>,
}

impl Plain {
    /// Returns the name of the source archive.
    /// If [Self::rename] is not defined, it is implied from the URL.
    pub fn name(&self) -> &str {
        if let Some(name) = &self.rename {
            name
        } else {
            util::uri_file_name(&self.url)
        }
    }

    /// Stores the source archive into the storage directory.
    ///
    /// If the upstream was already stored and [Self::hash] matches,
    /// no write operation takes place. If the source archive was
    /// not stored or the hash does not match, it is overwritten.
    pub async fn store(&self, storage_dir: &Path, pb: &ProgressBar) -> Result<StoredPlain, Error> {
        use fs_err::tokio as fs;

        match self.stored(storage_dir) {
            Ok(stored) => return Ok(stored),
            Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => {}
            Err(Error::HashMismatch { .. }) => {}
            Err(err) => return Err(err),
        }

        let path = self.stored_path(storage_dir);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(&parent).await?;
        }

        let hash = fetch(self.url.clone(), &path, pb).await?;
        if hash != self.hash {
            fs::remove_file(&path).await?;

            return Err(Error::HashMismatch {
                name: self.name().to_owned(),
                expected: self.hash.to_string(),
                got: hash,
            });
        }

        Ok(StoredPlain {
            name: self.name().to_owned(),
            path,
            hash: self.hash.clone(),
            was_cached: false,
        })
    }

    /// Returns an already-stored source archive.
    /// An error is instead returned if the source archive is
    /// not found in the storage directory, or its hash doesn't match
    /// [Self::hash].
    pub fn stored(&self, storage_dir: &Path) -> Result<StoredPlain, Error> {
        let path = self.stored_path(storage_dir);

        let mut file = fs_err::File::open(&path)?;
        let hash = util::sha256_hash(&mut file)?;
        if hash != self.hash.deref() {
            return Err(Error::HashMismatch {
                name: self.name().to_owned(),
                expected: self.hash.to_string(),
                got: Hash(hash),
            });
        }

        Ok(StoredPlain {
            name: self.name().to_owned(),
            path,
            hash: self.hash.clone(),
            was_cached: true,
        })
    }

    /// Returns a relative PathBuf where this source archive
    /// should be stored within the storage directory.
    fn stored_path(&self, storage_dir: &Path) -> PathBuf {
        storage_dir.join("fetched").join(self.file_path())
    }

    /// Returns a relative PathBuf based on the hashes of [Self::url]
    /// and [Self::hash].
    ///
    /// Hashing both ensures the path is unique and becomes invalid
    /// as soon as either the URL or the hash changes.
    fn file_path(&self) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(self.url.as_str());
        hasher.update(self.hash.as_bytes());

        let hash = hex::encode(hasher.finalize());
        // Type safe guaranteed to be >= 5 bytes.
        [&hash[..5], &hash[hash.len() - 5..], &hash].iter().collect()
    }
}

/// Information available after [Plain] is stored on disk.
#[derive(Clone)]
pub struct StoredPlain {
    /// Name of the upstream, as returned by [Plain::name].
    pub name: String,
    /// Path of the source archive after it was stored.
    pub path: PathBuf,
    /// Exact digest admitted by the source lock. Sharing hashes the bytes it
    /// copies rather than trusting an earlier check of the cache pathname.
    pub hash: Hash,
    /// Whether the source archived was already stored with valid hash.
    pub was_cached: bool,
}

impl StoredPlain {
    /// Shares the source archive in preparation of a build.
    ///
    /// The build-visible file must have an inode independent from the shared
    /// cache. Build scripts are allowed to modify their source directory; a
    /// hard link here would silently mutate the verified cache entry too.
    pub fn share(&self, dest_dir: &Path, source_date_epoch: i64) -> Result<(), Error> {
        let target = dest_dir.join(self.name.clone());
        let mut source = fs::File::open(&self.path)?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".cast-archive-")
            .tempfile_in(dest_dir)
            .map_err(|source| Error::CreateStaging {
                parent: dest_dir.to_owned(),
                source,
            })?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            temporary.as_file_mut().write_all(&buffer[..read])?;
        }
        let found = Hash(hex::encode(hasher.finalize()));
        if found != self.hash {
            return Err(Error::HashMismatch {
                name: self.name.clone(),
                expected: self.hash.to_string(),
                got: found,
            });
        }

        temporary.as_file().set_permissions(Permissions::from_mode(0o644))?;
        let timestamp = filetime::FileTime::from_unix_time(source_date_epoch, 0);
        filetime::set_file_handle_times(temporary.as_file(), Some(timestamp), Some(timestamp))?;
        temporary.persist_noclobber(&target).map_err(|error| Error::Install {
            target,
            source: error.error,
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    #[test]
    fn shared_archive_is_independent_from_the_verified_cache_inode() {
        let directory = tempfile::tempdir().unwrap();
        let cached = directory.path().join("cache/source.tar.zst");
        let shared = directory.path().join("build/sources");
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::write(&cached, b"verified archive").unwrap();
        let source = StoredPlain {
            name: "source.tar.zst".to_owned(),
            path: cached.clone(),
            hash: Hash(hex::encode(Sha256::digest(b"verified archive"))),
            was_cached: true,
        };
        fs::create_dir_all(&shared).unwrap();

        source.share(&shared, 1_700_000_000).unwrap();
        let build_visible = shared.join("source.tar.zst");
        assert_eq!(fs::read(&build_visible).unwrap(), b"verified archive");

        let cached_metadata = fs::metadata(&cached).unwrap();
        let shared_metadata = fs::metadata(&build_visible).unwrap();
        assert_ne!(
            (cached_metadata.dev(), cached_metadata.ino()),
            (shared_metadata.dev(), shared_metadata.ino())
        );
        assert_eq!(shared_metadata.mode() & 0o7777, 0o644);
        assert_eq!(shared_metadata.mtime(), 1_700_000_000);

        fs::write(&build_visible, b"mutated by build").unwrap();
        assert_eq!(fs::read(&cached).unwrap(), b"verified archive");
        assert_eq!(fs::read(&build_visible).unwrap(), b"mutated by build");
    }

    #[test]
    fn sharing_rehashes_copied_bytes_and_removes_failed_staging_file() {
        let directory = tempfile::tempdir().unwrap();
        let cached = directory.path().join("cache/source.tar.zst");
        let shared = directory.path().join("build/sources");
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::create_dir_all(&shared).unwrap();
        fs::write(&cached, b"cache bytes changed after the first verification").unwrap();
        let source = StoredPlain {
            name: "source.tar.zst".to_owned(),
            path: cached,
            hash: Hash(hex::encode(Sha256::digest(b"original locked archive"))),
            was_cached: true,
        };

        assert!(matches!(source.share(&shared, 0), Err(Error::HashMismatch { .. })));
        assert!(fs::read_dir(shared).unwrap().next().is_none());
    }

    #[test]
    fn sharing_never_replaces_a_destination_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let cached = directory.path().join("cache/source.tar.zst");
        let shared = directory.path().join("build/sources");
        let outside = directory.path().join("outside");
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(&cached, b"verified archive").unwrap();
        symlink(&outside, shared.join("source.tar.zst")).unwrap();
        let source = StoredPlain {
            name: "source.tar.zst".to_owned(),
            path: cached,
            hash: Hash(hex::encode(Sha256::digest(b"verified archive"))),
            was_cached: true,
        };

        assert!(matches!(source.share(&shared, 0), Err(Error::Install { .. })));
        assert!(
            fs::symlink_metadata(shared.join("source.tar.zst"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn archive_hash_parser_uses_the_canonical_source_digest_contract() {
        assert!("a".repeat(64).parse::<Hash>().is_ok());
        for invalid in ["a".repeat(63), "A".repeat(64), format!("{}g", "a".repeat(63))] {
            assert!(matches!(
                invalid.parse::<Hash>(),
                Err(ParseHashError::NotCanonical(value)) if value == invalid
            ));
        }
    }
}

/// Thin wrapper around String that represents a
/// hexadecimal SHA256 hash.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Hash(String);

impl FromStr for Hash {
    type Err = ParseHashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if !is_canonical_sha256(s) {
            return Err(ParseHashError::NotCanonical(s.to_owned()));
        }

        Ok(Self(s.to_owned()))
    }
}

impl TryFrom<String> for Hash {
    type Error = ParseHashError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !is_canonical_sha256(&value) {
            return Err(ParseHashError::NotCanonical(value));
        }
        Ok(Self(value))
    }
}

impl Deref for Hash {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.0.as_str()
    }
}

/// Reasons why [Hash] may be invalid.
#[derive(Debug, Error)]
pub enum ParseHashError {
    #[error("SHA-256 must be exactly 64 lowercase hexadecimal characters: {0}")]
    NotCanonical(String),
}

/// Possible errors returned by functions in this module.
#[derive(Debug, Error)]
pub enum Error {
    /// [Hash] is malformed.
    #[error("parse hash")]
    ParseHash(#[from] ParseHashError),
    /// Two hashes did not match.
    #[error("hash mismatch for {name}, expected {expected:?} got {:?}", got.0)]
    HashMismatch { name: String, expected: String, got: Hash },
    #[error("request")]
    /// A local or remote fetch failed.
    Request(#[from] request::Error),
    #[error("create private archive staging file in {parent:?}")]
    CreateStaging {
        parent: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("atomically install verified archive at {target:?}")]
    Install {
        target: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("io")]
    /// A generic I/O error occurred.
    Io(#[from] io::Error),
}

async fn fetch(url: Url, dest: &Path, pb: &ProgressBar) -> Result<Hash, Error> {
    pb.set_style(
        ProgressStyle::with_template(" {spinner} {wide_msg} {binary_bytes_per_sec:>.dim} ")
            .unwrap()
            .tick_chars("--=≡■≡=--"),
    );

    request::download_with_progress_and_sha256(url, dest, |progress| pb.inc(progress.delta))
        .await
        .map_err(Error::from)?
        .try_into()
        .map_err(Error::from)
}
