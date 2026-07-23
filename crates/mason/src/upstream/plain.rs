// SPDX-FileCopyrightText: 2026 AerynOS Developers

use std::{
    fs::Permissions,
    io::{self, Read, Write},
    ops::Deref,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use forge::{request, util};
use fs_err as fs;
use fs_err::os::unix::fs::OpenOptionsExt as _;
use sha2::{Digest, Sha256};
use stone_recipe::spec::is_canonical_sha256;
use thiserror::Error;
use tui::{ProgressBar, ProgressStyle};
use url::Url;

const GIB: u64 = 1024 * 1024 * 1024;

/// Package source archives are substantially more constrained than Forge's
/// general artifact downloads. This policy applies to authoring, resolution,
/// frozen builds, cached verification, and build-visible sharing.
pub(crate) const ARCHIVE_DOWNLOAD_LIMITS: request::DownloadLimits =
    request::DownloadLimits::new(2 * GIB, Duration::from_secs(20 * 60));

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
    /// no write operation takes place. A missing archive is downloaded into a
    /// private file and admitted by exact copy. Any mismatched or unsafe
    /// existing cache state fails closed and is never replaced.
    pub async fn store(&self, storage_dir: &Path, pb: &ProgressBar) -> Result<StoredPlain, Error> {
        use fs_err::tokio as fs;

        match self.stored(storage_dir) {
            Ok(stored) => return Ok(stored),
            Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }

        let path = self.stored_path(storage_dir);
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "archive cache path has no parent"))?;
        fs::create_dir_all(parent).await?;
        let temporary = tempfile::Builder::new()
            .prefix(".cast-download-input-")
            .tempfile_in(parent)
            .map_err(|source| Error::CreateStaging {
                parent: parent.to_owned(),
                source,
            })?
            .into_temp_path();
        fetch(self.url.clone(), &temporary, &self.hash, self.name(), pb).await?;
        let mut stored = self.admit_downloaded(storage_dir, &temporary)?;
        stored.was_cached = false;
        Ok(stored)
    }

    /// Returns an already-stored source archive.
    /// An error is instead returned if the source archive is
    /// not found in the storage directory, or its hash doesn't match
    /// [Self::hash].
    pub fn stored(&self, storage_dir: &Path) -> Result<StoredPlain, Error> {
        self.stored_with_max_bytes(storage_dir, ARCHIVE_DOWNLOAD_LIMITS.max_bytes)
    }

    fn stored_with_max_bytes(&self, storage_dir: &Path, max_bytes: u64) -> Result<StoredPlain, Error> {
        let path = self.stored_path(storage_dir);

        let mut file = open_regular_archive(&path, self.name())?;
        reject_file_size(&file, self.name(), max_bytes)?;
        let mut sink = io::sink();
        let hash = copy_and_hash_bounded(&mut file, &mut sink, self.name(), max_bytes)?;
        if hash != self.hash {
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
            was_cached: true,
        })
    }

    /// Admit one tracked offline fixture through the same content-addressed
    /// cache used by production HTTPS downloads.
    ///
    /// This boundary exists only in test builds or the narrowly feature-gated
    /// harness-free fixture target. It never changes the source URL or cache
    /// key and publishes only bytes which match the lock's exact SHA-256.
    /// Production binaries therefore retain the HTTPS-only source policy
    /// instead of gaining a local-file recipe escape hatch.
    #[cfg(any(test, feature = "delegated-fixture-test-support"))]
    pub(crate) fn import_fixture(&self, storage_dir: &Path, fixture: &Path) -> Result<StoredPlain, Error> {
        self.import_fixture_with_max_bytes(storage_dir, fixture, ARCHIVE_DOWNLOAD_LIMITS.max_bytes)
    }

    #[cfg(any(test, feature = "delegated-fixture-test-support"))]
    fn import_fixture_with_max_bytes(
        &self,
        storage_dir: &Path,
        fixture: &Path,
        max_bytes: u64,
    ) -> Result<StoredPlain, Error> {
        self.admit_downloaded_with_max_bytes(storage_dir, fixture, max_bytes)
    }

    /// Copy one freshly downloaded archive into the content-addressed cache
    /// without aliasing its inode or replacing any pre-existing cache entry.
    pub(crate) fn admit_downloaded(&self, storage_dir: &Path, source: &Path) -> Result<StoredPlain, Error> {
        self.admit_downloaded_with_max_bytes(storage_dir, source, ARCHIVE_DOWNLOAD_LIMITS.max_bytes)
    }

    fn admit_downloaded_with_max_bytes(
        &self,
        storage_dir: &Path,
        source_path: &Path,
        max_bytes: u64,
    ) -> Result<StoredPlain, Error> {
        match self.stored_with_max_bytes(storage_dir, max_bytes) {
            Ok(stored) => return Ok(stored),
            Err(Error::Io(source)) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(source),
        }

        let target = self.stored_path(storage_dir);
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "archive cache path has no parent"))?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, Permissions::from_mode(0o700))?;

        let mut source = open_regular_archive(source_path, self.name())?;
        reject_file_size(&source, self.name(), max_bytes)?;
        let mut staging = tempfile::Builder::new()
            .prefix(".cast-fixture-import-")
            .tempfile_in(parent)
            .map_err(|source| Error::CreateStaging {
                parent: parent.to_owned(),
                source,
            })?;
        let found = copy_and_hash_bounded(&mut source, staging.as_file_mut(), self.name(), max_bytes)?;
        if found != self.hash {
            return Err(Error::HashMismatch {
                name: self.name().to_owned(),
                expected: self.hash.to_string(),
                got: found,
            });
        }

        staging.as_file().set_permissions(Permissions::from_mode(0o644))?;
        staging.as_file().sync_all()?;
        if let Err(error) = staging.persist_noclobber(&target) {
            if error.error.kind() == io::ErrorKind::AlreadyExists {
                fs::File::open(parent)?.sync_all()?;
                return self.stored_with_max_bytes(storage_dir, max_bytes);
            }
            return Err(Error::Install {
                target,
                source: error.error,
            });
        }
        fs::File::open(parent)?.sync_all()?;

        self.stored_with_max_bytes(storage_dir, max_bytes)
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
        self.share_with_max_bytes(dest_dir, source_date_epoch, ARCHIVE_DOWNLOAD_LIMITS.max_bytes)
    }

    fn share_with_max_bytes(&self, dest_dir: &Path, source_date_epoch: i64, max_bytes: u64) -> Result<(), Error> {
        let target = dest_dir.join(self.name.clone());
        let mut source = open_regular_archive(&self.path, &self.name)?;
        reject_file_size(&source, &self.name, max_bytes)?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".cast-archive-")
            .tempfile_in(dest_dir)
            .map_err(|source| Error::CreateStaging {
                parent: dest_dir.to_owned(),
                source,
            })?;
        let found = copy_and_hash_bounded(&mut source, temporary.as_file_mut(), &self.name, max_bytes)?;
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

fn open_regular_archive(path: &Path, name: &str) -> Result<fs::File, Error> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(Error::NotRegular { name: name.to_owned() });
    }
    if metadata.nlink() != 1 {
        return Err(Error::LinkCount {
            name: name.to_owned(),
            links: metadata.nlink(),
        });
    }
    Ok(file)
}

fn reject_file_size(file: &fs::File, name: &str, max_bytes: u64) -> Result<(), Error> {
    if file.metadata()?.len() > max_bytes {
        Err(Error::TooLarge {
            name: name.to_owned(),
            limit: max_bytes,
        })
    } else {
        Ok(())
    }
}

fn copy_and_hash_bounded<R: Read, W: Write>(
    source: &mut R,
    destination: &mut W,
    name: &str,
    max_bytes: u64,
) -> Result<Hash, Error> {
    let mut hasher = Sha256::new();
    let mut completed = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        // Probe one byte beyond the allowance so growth after the metadata
        // preflight cannot bypass the limit.
        let remaining = max_bytes.saturating_sub(completed);
        let read_capacity = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let read = source.read(&mut buffer[..read_capacity])?;
        if read == 0 {
            break;
        }
        if read as u64 > remaining {
            return Err(Error::TooLarge {
                name: name.to_owned(),
                limit: max_bytes,
            });
        }
        destination.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        completed += read as u64;
    }

    Ok(Hash(hex::encode(hasher.finalize())))
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
    fn fixture_import_copies_exact_locked_bytes_into_the_normal_cache_namespace() {
        let directory = tempfile::tempdir().unwrap();
        let fixture = directory.path().join("fixture.tar");
        let storage = directory.path().join("storage");
        fs::write(&fixture, b"locked offline fixture").unwrap();
        let plain = Plain {
            url: Url::parse("https://fixtures.invalid/source.tar").unwrap(),
            hash: Hash(hex::encode(Sha256::digest(b"locked offline fixture"))),
            rename: Some("source.tar".to_owned()),
        };

        let stored = plain.import_fixture(&storage, &fixture).unwrap();

        assert!(stored.was_cached);
        assert_eq!(stored.path, plain.stored_path(&storage));
        assert_eq!(fs::read(&stored.path).unwrap(), b"locked offline fixture");
        let fixture_metadata = fs::metadata(fixture).unwrap();
        let cached_metadata = fs::metadata(&stored.path).unwrap();
        assert_ne!(
            (fixture_metadata.dev(), fixture_metadata.ino()),
            (cached_metadata.dev(), cached_metadata.ino())
        );
        assert_eq!(cached_metadata.mode() & 0o7777, 0o644);
        assert_eq!(cached_metadata.nlink(), 1);
    }

    #[test]
    fn downloaded_admission_copies_bytes_without_aliasing_the_download_inode() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("download.tar");
        let storage = directory.path().join("storage");
        fs::write(&source, b"downloaded bytes").unwrap();
        let plain = Plain {
            url: Url::parse("https://example.invalid/source.tar").unwrap(),
            hash: Hash(hex::encode(Sha256::digest(b"downloaded bytes"))),
            rename: Some("source.tar".to_owned()),
        };

        let stored = plain.admit_downloaded(&storage, &source).unwrap();

        assert_eq!(fs::read(&stored.path).unwrap(), b"downloaded bytes");
        let source_metadata = fs::metadata(source).unwrap();
        let cache_metadata = fs::metadata(stored.path).unwrap();
        assert_ne!(
            (source_metadata.dev(), source_metadata.ino()),
            (cache_metadata.dev(), cache_metadata.ino())
        );
        assert_eq!(cache_metadata.nlink(), 1);
        assert_eq!(cache_metadata.mode() & 0o7777, 0o644);
    }

    #[test]
    fn concurrent_identical_downloads_adopt_one_exact_no_clobber_cache_entry() {
        use std::sync::{Arc, Barrier};

        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.tar");
        let second = directory.path().join("second.tar");
        let storage = directory.path().join("storage");
        fs::write(&first, b"identical bytes").unwrap();
        fs::write(&second, b"identical bytes").unwrap();
        let plain = Plain {
            url: Url::parse("https://example.invalid/source.tar").unwrap(),
            hash: Hash(hex::encode(Sha256::digest(b"identical bytes"))),
            rename: Some("source.tar".to_owned()),
        };
        let barrier = Arc::new(Barrier::new(2));

        let handles = [first, second].map(|source| {
            let storage = storage.clone();
            let plain = plain.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                plain.admit_downloaded(&storage, &source)
            })
        });
        let stored = handles.map(|handle| handle.join().unwrap().unwrap());

        assert_eq!(stored[0].path, stored[1].path);
        assert_eq!(fs::read(&stored[0].path).unwrap(), b"identical bytes");
        assert_eq!(fs::metadata(&stored[0].path).unwrap().nlink(), 1);
    }

    #[test]
    fn fixture_import_is_bounded_and_hash_checked_before_publication() {
        let directory = tempfile::tempdir().unwrap();
        let storage = directory.path().join("storage");
        let exact_fixture = directory.path().join("exact.tar");
        fs::write(&exact_fixture, b"1234").unwrap();
        let exact = Plain {
            url: Url::parse("https://fixtures.invalid/exact.tar").unwrap(),
            hash: Hash(hex::encode(Sha256::digest(b"1234"))),
            rename: None,
        };
        assert!(exact.import_fixture_with_max_bytes(&storage, &exact_fixture, 4).is_ok());

        let oversized_fixture = directory.path().join("oversized.tar");
        fs::write(&oversized_fixture, b"12345").unwrap();
        let oversized = Plain {
            url: Url::parse("https://fixtures.invalid/oversized.tar").unwrap(),
            hash: Hash(hex::encode(Sha256::digest(b"12345"))),
            rename: None,
        };
        assert!(matches!(
            oversized.import_fixture_with_max_bytes(&storage, &oversized_fixture, 4),
            Err(Error::TooLarge { limit: 4, .. })
        ));
        assert!(!oversized.stored_path(&storage).exists());

        let mismatched_fixture = directory.path().join("mismatched.tar");
        fs::write(&mismatched_fixture, b"not admitted").unwrap();
        let mismatched = Plain {
            url: Url::parse("https://fixtures.invalid/mismatched.tar").unwrap(),
            hash: Hash(hex::encode(Sha256::digest(b"expected"))),
            rename: None,
        };
        assert!(matches!(
            mismatched.import_fixture(&storage, &mismatched_fixture),
            Err(Error::HashMismatch { .. })
        ));
        assert!(!mismatched.stored_path(&storage).exists());
        let mismatch_parent = mismatched.stored_path(&storage).parent().unwrap().to_owned();
        assert!(fs::read_dir(mismatch_parent).unwrap().next().is_none());
    }

    #[test]
    fn fixture_import_rejects_aliases_and_never_replaces_existing_cache_state() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let storage = directory.path().join("storage");
        let original = directory.path().join("original.tar");
        let hardlink = directory.path().join("hardlink.tar");
        fs::write(&original, b"locked bytes").unwrap();
        fs::hard_link(&original, &hardlink).unwrap();
        let plain = Plain {
            url: Url::parse("https://fixtures.invalid/source.tar").unwrap(),
            hash: Hash(hex::encode(Sha256::digest(b"locked bytes"))),
            rename: None,
        };
        assert!(matches!(
            plain.import_fixture(&storage, &hardlink),
            Err(Error::LinkCount { links: 2, .. })
        ));
        assert!(!plain.stored_path(&storage).exists());

        fs::remove_file(hardlink).unwrap();
        let symlink_fixture = directory.path().join("symlink.tar");
        symlink(&original, &symlink_fixture).unwrap();
        assert!(matches!(
            plain.import_fixture(&storage, &symlink_fixture),
            Err(Error::Io(_))
        ));
        assert!(!plain.stored_path(&storage).exists());

        let target = plain.stored_path(&storage);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"preexisting corrupt state").unwrap();
        assert!(matches!(
            plain.import_fixture(&storage, &original),
            Err(Error::HashMismatch { .. })
        ));
        assert_eq!(fs::read(target).unwrap(), b"preexisting corrupt state");
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
    fn cached_archive_limit_accepts_n_and_rejects_n_plus_one() {
        let directory = tempfile::tempdir().unwrap();
        let storage = directory.path().join("cache");
        let exact = Plain {
            url: Url::parse("https://example.invalid/exact.tar.zst").unwrap(),
            hash: Hash(hex::encode(Sha256::digest(b"1234"))),
            rename: None,
        };
        let exact_path = exact.stored_path(&storage);
        fs::create_dir_all(exact_path.parent().unwrap()).unwrap();
        fs::write(&exact_path, b"1234").unwrap();
        assert!(exact.stored_with_max_bytes(&storage, 4).is_ok());

        let oversized = Plain {
            url: Url::parse("https://example.invalid/oversized.tar.zst").unwrap(),
            hash: Hash(hex::encode(Sha256::digest(b"12345"))),
            rename: None,
        };
        let oversized_path = oversized.stored_path(&storage);
        fs::create_dir_all(oversized_path.parent().unwrap()).unwrap();
        fs::write(&oversized_path, b"12345").unwrap();
        assert!(matches!(
            oversized.stored_with_max_bytes(&storage, 4),
            Err(Error::TooLarge { limit: 4, .. })
        ));
    }

    #[test]
    fn production_store_never_refetches_over_mismatched_cache_state() {
        let directory = tempfile::tempdir().unwrap();
        let storage = directory.path().join("cache");
        let plain = Plain {
            url: Url::parse("https://network-must-not-run.invalid/source.tar").unwrap(),
            hash: Hash(hex::encode(Sha256::digest(b"expected bytes"))),
            rename: Some("source.tar".to_owned()),
        };
        let target = plain.stored_path(&storage);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"foreign cache state").unwrap();

        let error = match forge::runtime::block_on(plain.store(&storage, &ProgressBar::hidden())) {
            Ok(_) => panic!("mismatched cache state must fail before fetching"),
            Err(error) => error,
        };

        assert!(matches!(error, Error::HashMismatch { .. }));
        assert_eq!(fs::read(target).unwrap(), b"foreign cache state");
    }

    #[test]
    fn cached_archive_stream_limit_independently_probes_n_plus_one() {
        let mut exact = io::Cursor::new(b"1234".to_vec());
        let mut sink = io::sink();
        assert!(copy_and_hash_bounded(&mut exact, &mut sink, "source", 4).is_ok());

        let mut oversized = io::Cursor::new(b"12345".to_vec());
        assert!(matches!(
            copy_and_hash_bounded(&mut oversized, &mut sink, "source", 4),
            Err(Error::TooLarge { limit: 4, .. })
        ));
    }

    #[test]
    fn sharing_n_plus_one_archive_removes_private_staging() {
        let directory = tempfile::tempdir().unwrap();
        let cached = directory.path().join("cache/source.tar.zst");
        let shared = directory.path().join("build/sources");
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::create_dir_all(&shared).unwrap();
        fs::write(&cached, b"12345").unwrap();
        let source = StoredPlain {
            name: "source.tar.zst".to_owned(),
            path: cached,
            hash: Hash(hex::encode(Sha256::digest(b"12345"))),
            was_cached: true,
        };

        assert!(matches!(
            source.share_with_max_bytes(&shared, 0, 4),
            Err(Error::TooLarge { limit: 4, .. })
        ));
        assert!(fs::read_dir(shared).unwrap().next().is_none());
    }

    #[test]
    fn cached_archive_symlink_is_never_followed() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let storage = directory.path().join("cache");
        let outside = directory.path().join("outside.tar.zst");
        fs::write(&outside, b"locked bytes").unwrap();
        let plain = Plain {
            url: Url::parse("https://example.invalid/source.tar.zst").unwrap(),
            hash: Hash(hex::encode(Sha256::digest(b"locked bytes"))),
            rename: None,
        };
        let cached = plain.stored_path(&storage);
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        symlink(&outside, &cached).unwrap();

        assert!(matches!(plain.stored(&storage), Err(Error::Io(_))));
        assert_eq!(fs::read(&outside).unwrap(), b"locked bytes");
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
    /// A source archive exceeded the package-source resource policy.
    #[error("archive {name:?} exceeds byte limit of {limit}")]
    TooLarge { name: String, limit: u64 },
    /// The cache path did not name an ordinary archive file.
    #[error("archive {name:?} is not a regular file")]
    NotRegular { name: String },
    /// A cache or fixture inode had aliases outside the admitted path.
    #[error("archive {name:?} has {links} hard links; expected exactly one")]
    LinkCount { name: String, links: u64 },
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

async fn fetch(url: Url, dest: &Path, expected: &Hash, name: &str, pb: &ProgressBar) -> Result<(), Error> {
    pb.set_style(
        ProgressStyle::with_template(" {spinner} {wide_msg} {binary_bytes_per_sec:>.dim} ")
            .unwrap()
            .tick_chars("--=≡■≡=--"),
    );

    match request::download_with_progress_and_expected_sha256_and_limits(
        url,
        dest,
        expected,
        ARCHIVE_DOWNLOAD_LIMITS,
        |progress| pb.inc(progress.delta),
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(request::Error::HashMismatch { expected, actual }) => Err(Error::HashMismatch {
            name: name.to_owned(),
            expected,
            got: Hash(actual),
        }),
        Err(request::Error::TooLarge { limit }) => Err(Error::TooLarge {
            name: name.to_owned(),
            limit,
        }),
        Err(source) => Err(Error::Request(source)),
    }
}
