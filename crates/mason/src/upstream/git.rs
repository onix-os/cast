// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    ffi::{CString, OsString},
    io,
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use forge::util;
use fs_err as fs;
use fs_err::os::unix::fs::OpenOptionsExt as _;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    sync::mpsc,
    time::{Instant, sleep},
};
use tui::{ProgressBar, ProgressStyle};
use url::Url;

mod materialization;

/// Production Git policy for source acquisition and materialization. These
/// ceilings are deliberately generous for large upstreams. Process streams
/// and individual files have hard ceilings; aggregate mirror/checkout usage is
/// sampled during mutation and mandatorily checked after Git exits.
const MASON_GIT_LIMITS: gitwrap::Limits = gitwrap::Limits {
    wall_timeout: Duration::from_secs(30 * 60),
    termination_timeout: Duration::from_secs(10),
    stdout_bytes: 64 * 1024 * 1024,
    stderr_bytes: 8 * 1024 * 1024,
    progress_segment_bytes: 64 * 1024,
    repository_bytes: 64 * 1024 * 1024 * 1024,
    repository_entries: 4_000_000,
    open_files: 4096,
    address_space_bytes: 16 * 1024 * 1024 * 1024,
    quota_poll_interval: Duration::from_secs(2),
};

const CACHE_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const CACHE_LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Upstream based on a Git repository.
#[derive(Clone, Debug)]
pub struct Git {
    /// URL of origin.
    pub url: Url,
    /// Revision to fetch, pinned to the full commit when a source lock exists.
    pub commit: String,
    /// Exact directory name used when sharing this source with the build.
    pub name: String,
    pub original_index: usize,
    /// Expected normalized checkout identity. Authored moving references have
    /// no value until explicit lock refresh materializes them.
    pub materialization_sha256: Option<String>,
}

impl Git {
    /// Returns the name of the upstream. It is implied from the URL.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Stores the upstream into the storage directory.
    /// If the upstream was already stored but does not include [Self::commit],
    /// it is updated contextually. If it does not exist, the Git repository is cloned.
    pub async fn store(&self, storage_dir: &Path, pb: &ProgressBar) -> Result<StoredGit, Error> {
        let _cache_lock = self.acquire_cache_lock(storage_dir, CacheLockMode::Exclusive).await?;
        let repo: gitwrap::Repository;
        let mut mutation = None;
        let mut cached = true;
        match self.stored_locked(storage_dir).await {
            Ok((stored, has_commit)) => {
                repo = stored.repo;
                if !has_commit {
                    cached = false;
                    mutation = Some(self.fetch_cached(storage_dir, &repo, pb).await?);
                }
            }
            Err(Error::Git(_) | Error::OriginMismatch { .. } | Error::IncompleteCache { .. }) => {
                cached = false;
                self.remove_locked(storage_dir)?;
                let stored_path = self.stored_path(storage_dir);
                if let Some(parent) = stored_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                repo = clone(&self.url, &stored_path, pb).await?;
            }
            Err(error) => return Err(error),
        }

        let resolved_hash = match repo.peel_commit(&self.commit).await {
            Ok(resolved_hash) => resolved_hash,
            Err(source) => {
                if mutation.is_some() {
                    self.remove_locked(storage_dir)?;
                }
                return Err(source.into());
            }
        };
        if let Err(source) = reject_gitlinks(&repo, &resolved_hash).await {
            if mutation.is_some() {
                self.remove_locked(storage_dir)?;
            }
            return Err(source);
        }
        if let Some(marker) = mutation
            && let Err(source) = marker.commit()
        {
            self.remove_locked(storage_dir)?;
            return Err(source.into());
        }

        Ok(StoredGit {
            name: self.name().to_owned(),
            was_cached: cached,
            repo,
            resolved_hash,
            original_index: self.original_index,
            materialization_sha256: self.materialization_sha256.clone(),
        })
    }

    /// Resolve an authored moving reference against the current remote state.
    ///
    /// Normal build storage may reuse a lock-pinned commit without contacting
    /// the network. Explicit lock refreshes must instead fetch an existing
    /// mirror so branches and tags can advance before they are pinned again.
    pub async fn resolve(&self, storage_dir: &Path, pb: &ProgressBar) -> Result<StoredGit, Error> {
        let _cache_lock = self.acquire_cache_lock(storage_dir, CacheLockMode::Exclusive).await?;
        let mut mutation = None;
        let repo = match self.stored_locked(storage_dir).await {
            Ok((stored, _)) => {
                mutation = Some(self.fetch_cached(storage_dir, &stored.repo, pb).await?);
                stored.repo
            }
            Err(Error::Git(_) | Error::OriginMismatch { .. } | Error::IncompleteCache { .. }) => {
                self.remove_locked(storage_dir)?;
                let stored_path = self.stored_path(storage_dir);
                if let Some(parent) = stored_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                clone(&self.url, &stored_path, pb).await?
            }
            Err(error) => return Err(error),
        };
        let resolved_hash = match repo.peel_commit(&self.commit).await {
            Ok(resolved_hash) => resolved_hash,
            Err(source) => {
                if mutation.is_some() {
                    self.remove_locked(storage_dir)?;
                }
                return Err(source.into());
            }
        };
        if let Err(source) = reject_gitlinks(&repo, &resolved_hash).await {
            if mutation.is_some() {
                self.remove_locked(storage_dir)?;
            }
            return Err(source);
        }
        if let Some(marker) = mutation
            && let Err(source) = marker.commit()
        {
            self.remove_locked(storage_dir)?;
            return Err(source.into());
        }

        Ok(StoredGit {
            name: self.name().to_owned(),
            was_cached: false,
            repo,
            resolved_hash,
            original_index: self.original_index,
            materialization_sha256: self.materialization_sha256.clone(),
        })
    }

    fn remove_locked(&self, storage_dir: &Path) -> Result<(), Error> {
        let dir = self.stored_path(storage_dir);
        let marker = self.mutation_marker_path(storage_dir);
        // Leave the marker in place unless removal of the potentially partial
        // mirror succeeds. A failed cleanup must remain ineligible for reuse.
        util::remove_dir_all(&dir)?;
        match fs::remove_file(&marker) {
            Ok(()) => sync_parent_directory(&marker)?,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(source.into()),
        }
        Ok(())
    }

    /// Returns the stored upstream if it exists.
    ///
    /// If successful, a tuple is returned containing the
    /// stored upstream and a boolean flag, indicating whether
    /// the stored Git repository contains [Self::commit].
    #[cfg(test)]
    pub async fn stored(&self, storage_dir: &Path) -> Result<(StoredGit, bool), Error> {
        let _cache_lock = self.acquire_cache_lock(storage_dir, CacheLockMode::Shared).await?;
        self.stored_locked(storage_dir).await
    }

    async fn stored_locked(&self, storage_dir: &Path) -> Result<(StoredGit, bool), Error> {
        let stored_path = self.stored_path(storage_dir);
        let marker = self.mutation_marker_path(storage_dir);
        match fs::symlink_metadata(&marker) {
            Ok(_) => return Err(Error::IncompleteCache { cache: stored_path }),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(source.into()),
        }
        let repo = match gitwrap::Repository::open_private_mirror_with_limits(&stored_path, &self.url, MASON_GIT_LIMITS)
            .await
        {
            Ok(repo) => repo,
            Err(source) if source.mirror_origin_mismatch() => {
                return Err(Error::OriginMismatch { cache: stored_path });
            }
            Err(source) => return Err(source.into()),
        };
        repo.secure_private_mirror()?;
        let has_ref = repo.has_commit(&self.commit).await?;
        let resolved_hash = repo.peel_commit(&self.commit).await?;
        Ok((
            StoredGit {
                name: self.name().to_owned(),
                was_cached: has_ref,
                repo,
                resolved_hash,
                original_index: self.original_index,
                materialization_sha256: self.materialization_sha256.clone(),
            },
            has_ref,
        ))
    }

    /// Returns a relative PathBuf where this Git repository
    /// should be stored within the storage directory.
    fn stored_path(&self, storage_dir: &Path) -> PathBuf {
        storage_dir.join("git").join(self.directory_name())
    }

    fn mutation_marker_path(&self, storage_dir: &Path) -> PathBuf {
        let stored = self.stored_path(storage_dir);
        let mut name = OsString::from(".");
        name.push(stored.file_name().expect("Git cache path has a file name"));
        name.push(".fetch-in-progress");
        stored.with_file_name(name)
    }

    fn cache_lock_path(&self, storage_dir: &Path) -> PathBuf {
        let stored = self.stored_path(storage_dir);
        let mut name = OsString::from(".");
        name.push(stored.file_name().expect("Git cache path has a file name"));
        name.push(".lock");
        stored.with_file_name(name)
    }

    async fn acquire_cache_lock(&self, storage_dir: &Path, mode: CacheLockMode) -> Result<CacheLock, Error> {
        let lock = self.open_cache_lock(storage_dir)?;
        let deadline = Instant::now() + CACHE_LOCK_WAIT_TIMEOUT;
        loop {
            match try_lock_cache_file(&lock.file, mode)? {
                true => {
                    lock.verify_name()?;
                    return Ok(CacheLock {
                        _file: lock.file,
                        _parent: lock.parent,
                    });
                }
                false if Instant::now() >= deadline => {
                    return Err(Error::CacheBusy {
                        cache: self.stored_path(storage_dir),
                    });
                }
                false => sleep(CACHE_LOCK_RETRY_INTERVAL).await,
            }
        }
    }

    fn open_cache_lock(&self, storage_dir: &Path) -> Result<OpenedCacheLock, Error> {
        let path = self.cache_lock_path(storage_dir);
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Git cache lock has no parent directory"))?;
        fs::create_dir_all(parent)?;
        let parent = open_directory(parent)?;
        parent.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        let name = path_file_name(&path, "Git cache lock")?;
        let file = openat_lock_file(&parent, &name)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Git cache lock is not a private regular file",
            )
            .into());
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.sync_all()?;
        parent.sync_all()?;
        let identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        Ok(OpenedCacheLock {
            file,
            parent,
            name,
            identity,
        })
    }

    fn begin_cache_mutation(&self, storage_dir: &Path) -> Result<CacheMutationMarker, Error> {
        let path = self.mutation_marker_path(storage_dir);
        write_cache_mutation_marker(&path, true).map_err(|source| {
            if source.kind() == io::ErrorKind::AlreadyExists {
                Error::IncompleteCache {
                    cache: self.stored_path(storage_dir),
                }
            } else {
                source.into()
            }
        })?;
        Ok(CacheMutationMarker { path })
    }

    async fn fetch_cached(
        &self,
        storage_dir: &Path,
        repo: &gitwrap::Repository,
        pb: &ProgressBar,
    ) -> Result<CacheMutationMarker, Error> {
        let marker = self.begin_cache_mutation(storage_dir)?;
        if let Err(source) = fetch(repo, pb).await {
            // Fetch mutates the cache in place. If its deadline, output, or
            // storage budget fails, discard it rather than trusting partial
            // state on a later build. The durable marker survives if cleanup
            // itself cannot complete.
            self.remove_locked(storage_dir)?;
            return Err(source.into());
        }
        Ok(marker)
    }

    /// Returns the name of the directory that should contain
    /// the Git repository.
    /// The readable prefix is deliberately cosmetic. The SHA-256 suffix binds
    /// the cache identity to every byte of the canonical URL, including its
    /// scheme, authority, port, path, query, and user information.
    fn directory_name(&self) -> PathBuf {
        const MAX_READABLE_BYTES: usize = 48;

        let basename = self
            .url
            .path_segments()
            .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
            .unwrap_or("repository");
        let basename = basename.strip_suffix(".git").unwrap_or(basename);
        let mut readable = basename
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                    character
                } else {
                    '-'
                }
            })
            .take(MAX_READABLE_BYTES)
            .collect::<String>();
        while readable.ends_with('-') || readable.ends_with('_') {
            readable.pop();
        }
        let first_safe = readable
            .find(|character: char| character.is_ascii_alphanumeric())
            .unwrap_or(readable.len());
        readable.drain(..first_safe);
        if readable.is_empty() {
            readable.push_str("repository");
        }

        let digest = Sha256::digest(self.url.as_str().as_bytes());
        format!("{readable}-{digest:x}").into()
    }
}

#[derive(Clone, Copy)]
enum CacheLockMode {
    #[cfg(test)]
    Shared,
    Exclusive,
}

fn try_lock_cache_file(file: &fs::File, mode: CacheLockMode) -> io::Result<bool> {
    let operation = match mode {
        #[cfg(test)]
        CacheLockMode::Shared => nix::libc::LOCK_SH | nix::libc::LOCK_NB,
        CacheLockMode::Exclusive => nix::libc::LOCK_EX | nix::libc::LOCK_NB,
    };
    let result = unsafe { nix::libc::flock(file.as_raw_fd(), operation) };
    if result == 0 {
        Ok(true)
    } else {
        let source = io::Error::last_os_error();
        let code = source.raw_os_error();
        if code == Some(nix::libc::EAGAIN) || code == Some(nix::libc::EWOULDBLOCK) {
            Ok(false)
        } else {
            Err(source)
        }
    }
}

struct CacheLock {
    _file: fs::File,
    _parent: fs::File,
}

struct OpenedCacheLock {
    file: fs::File,
    parent: fs::File,
    name: CString,
    identity: FileIdentity,
}

impl OpenedCacheLock {
    fn verify_name(&self) -> io::Result<()> {
        let metadata = self.file.metadata()?;
        let held_identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if metadata.is_file()
            && metadata.nlink() == 1
            && metadata.mode() & 0o7777 == 0o600
            && held_identity == self.identity
            && regular_identity_at(&self.parent, &self.name)? == Some(self.identity)
        {
            Ok(())
        } else {
            Err(io::Error::other(
                "Git cache lock inode, link count, or private mode changed before acquisition",
            ))
        }
    }
}

struct CacheMutationMarker {
    path: PathBuf,
}

impl CacheMutationMarker {
    fn commit(self) -> io::Result<()> {
        fs::remove_file(&self.path)?;
        if let Err(source) = sync_parent_directory(&self.path) {
            // Once unlink succeeds, a failed directory sync must not leave the
            // current process with an apparently reusable cache. Restore and
            // sync the poison marker before reporting the original failure.
            write_cache_mutation_marker(&self.path, false)?;
            return Err(source);
        }
        Ok(())
    }
}

fn write_cache_mutation_marker(path: &Path, create_new: bool) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options
        .write(true)
        .create(!create_new)
        .create_new(create_new)
        .truncate(!create_new)
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    let mut file = options.open(path)?;
    use std::io::Write as _;
    file.write_all(b"incomplete Git cache mutation\n")?;
    file.sync_all()?;
    sync_parent_directory(path)
}

fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Git cache marker has no parent directory"))?;
    fs::File::open(parent)?.sync_all()
}

/// Information available after [Git] is stored on disk.
pub struct StoredGit {
    /// Name of the upstream, as returned by [Git::name].
    pub name: String,
    /// Whether the stored Git repository was
    /// synchronized with [Git],
    /// that is, it existed and contained [Git::commit].
    pub was_cached: bool,
    pub resolved_hash: String,
    pub original_index: usize,
    pub materialization_sha256: Option<String>,
    pub repo: gitwrap::Repository,
}

impl StoredGit {
    /// Export one exact commit, remove Git administration data, normalize the
    /// build-visible tree, and return its canonical SHA-256 identity.
    pub(crate) async fn export_normalized(&self, dest_dir: &Path, source_date_epoch: i64) -> Result<String, Error> {
        self.export_normalized_with_root_access(dest_dir, source_date_epoch, false)
            .await
    }

    async fn export_normalized_with_root_access(
        &self,
        dest_dir: &Path,
        source_date_epoch: i64,
        descriptor_rooted: bool,
    ) -> Result<String, Error> {
        reject_gitlinks(&self.repo, &self.resolved_hash).await?;
        match fs::symlink_metadata(dest_dir) {
            Ok(_) => return Err(Error::DestinationExists(dest_dir.to_owned())),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(source.into()),
        }

        // Clone from our mirror to destdir
        let cloned = self.repo.clone_to(dest_dir).await?;

        // Cloning sets origin to the local mirror, but we want to use
        // the original remote as submodule resolving may depend on this
        let source_origin = self.repo.get_remote_url("origin").await?;
        cloned.set_remote_url("origin", &source_origin).await?;

        // Finally checkout the desired commit
        cloned.checkout(&self.resolved_hash).await?;

        // Git administration data contains checkout-time and host-specific
        // state and is not part of the locked commit tree.
        let removal = if descriptor_rooted {
            materialization::remove_git_administration_descriptor_path_bounded(dest_dir)
        } else {
            materialization::remove_git_administration_bounded(dest_dir)
        };
        removal.map_err(|source| Error::Materialization {
            root: dest_dir.to_owned(),
            source,
        })?;
        let digest = if descriptor_rooted {
            materialization::normalize_and_hash_descriptor_path(dest_dir, source_date_epoch)
        } else {
            materialization::normalize_and_hash(dest_dir, source_date_epoch)
        };
        digest.map_err(|source| Error::Materialization {
            root: dest_dir.to_owned(),
            source,
        })
    }

    async fn export_normalized_sealed(
        &self,
        dest_dir: &Path,
        source_date_epoch: i64,
    ) -> Result<materialization::SealedMaterialization, Error> {
        reject_gitlinks(&self.repo, &self.resolved_hash).await?;
        match fs::symlink_metadata(dest_dir) {
            Ok(_) => return Err(Error::DestinationExists(dest_dir.to_owned())),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(source.into()),
        }

        let cloned = self.repo.clone_to(dest_dir).await?;
        let source_origin = self.repo.get_remote_url("origin").await?;
        cloned.set_remote_url("origin", &source_origin).await?;
        cloned.checkout(&self.resolved_hash).await?;
        materialization::remove_git_administration_descriptor_path_bounded(dest_dir).map_err(|source| {
            Error::Materialization {
                root: dest_dir.to_owned(),
                source,
            }
        })?;
        materialization::normalize_and_seal_descriptor_path_with_limits(
            dest_dir,
            source_date_epoch,
            materialization::MaterializationLimits::default(),
        )
        .map_err(|source| Error::Materialization {
            root: dest_dir.to_owned(),
            source,
        })
    }

    /// Shares the exact Git repository in preparation of a frozen build and
    /// rejects any checkout whose normalized bytes differ from the source lock.
    pub async fn share(&self, dest_dir: &Path, source_date_epoch: i64) -> Result<(), Error> {
        let expected = self
            .materialization_sha256
            .as_deref()
            .ok_or_else(|| Error::MissingMaterializationDigest {
                index: self.original_index,
                commit: self.resolved_hash.clone(),
            })?;
        let parent = dest_dir
            .parent()
            .ok_or_else(|| Error::MissingDestinationParent(dest_dir.to_owned()))?;
        let staging = tempfile::Builder::new()
            .prefix(".cast-git-")
            .tempdir_in(parent)
            .map_err(|source| Error::CreateStaging {
                parent: parent.to_owned(),
                source,
            })?;
        let checkout = staging.path().join("checkout");
        let sealed = self.export_normalized_sealed(&checkout, source_date_epoch).await?;
        if sealed.digest() != expected {
            return Err(Error::MaterializationDigestMismatch {
                index: self.original_index,
                commit: self.resolved_hash.clone(),
                expected: expected.to_owned(),
                found: sealed.digest().to_owned(),
            });
        }
        let installed = PinnedInstall::install(&checkout, dest_dir).map_err(|source| Error::Install {
            source_path: checkout,
            destination: dest_dir.to_owned(),
            source,
        })?;
        if let Err(source) = sealed.verify_installed_descriptor_path(dest_dir) {
            return match installed.quarantine() {
                Ok(quarantine) => Err(Error::RejectedInstalledMaterialization {
                    destination: dest_dir.to_owned(),
                    quarantine,
                    source,
                }),
                Err(cleanup) => Err(Error::RejectedInstallCleanup {
                    destination: dest_dir.to_owned(),
                    verification: Box::new(source),
                    cleanup,
                }),
            };
        }

        Ok(())
    }
}

async fn reject_gitlinks(repo: &gitwrap::Repository, commit: &str) -> Result<(), Error> {
    if repo.contains_gitlinks(commit).await? {
        Err(Error::UnsupportedSubmodules {
            commit: commit.to_owned(),
        })
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: nix::libc::dev_t,
    inode: nix::libc::ino_t,
}

impl FileIdentity {
    fn from_file(file: &fs::File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "installed Git checkout is not a directory",
            ));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

/// Pins both the destination parent and the exact staged directory inode
/// across publication and post-install verification. Failure cleanup never
/// resolves the destination through its caller-visible ancestors again.
struct PinnedInstall {
    parent: fs::File,
    parent_path: PathBuf,
    parent_identity: FileIdentity,
    name: CString,
    identity: FileIdentity,
    _installed: fs::File,
}

impl PinnedInstall {
    fn install(source: &Path, destination: &Path) -> io::Result<Self> {
        let source_parent = open_parent_directory(source, "staged checkout")?;
        let source_name = path_file_name(source, "staged checkout")?;
        let installed = openat_directory(&source_parent, &source_name)?;
        let identity = FileIdentity::from_file(&installed)?;

        let parent_path = destination
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "final checkout has no parent"))?
            .to_owned();
        let parent = open_directory(&parent_path)?;
        let parent_identity = FileIdentity::from_file(&parent)?;
        let name = path_file_name(destination, "final checkout")?;
        renameat_noreplace(&source_parent, &source_name, &parent, &name)?;
        if identity_at(&parent, &name)? != Some(identity) {
            return Err(io::Error::other(
                "published Git checkout name does not identify the staged directory",
            ));
        }
        parent.sync_all()?;
        Ok(Self {
            parent,
            parent_path,
            parent_identity,
            name,
            identity,
            _installed: installed,
        })
    }

    fn quarantine(self) -> io::Result<PathBuf> {
        // A public-name replacement is not ours to move. This check is made
        // against the pinned parent, not a re-resolved ancestor path.
        if identity_at(&self.parent, &self.name)? != Some(self.identity) {
            return Err(io::Error::other(
                "refusing to quarantine a replacement Git checkout inode",
            ));
        }

        let (quarantine_name, quarantine) = create_quarantine_directory(&self.parent)?;
        let checkout = c"checkout";
        if let Err(source) = renameat_noreplace(&self.parent, &self.name, &quarantine, checkout) {
            drop(quarantine);
            return Err(cleanup_quarantine_error(&self.parent, &quarantine_name, source));
        }
        if identity_at(&quarantine, checkout)? != Some(self.identity) {
            return Err(io::Error::other(
                "quarantined Git checkout does not identify the installed directory",
            ));
        }
        quarantine.sync_all()?;
        self.parent.sync_all()?;
        let quarantine_name = std::ffi::OsStr::from_bytes(quarantine_name.as_bytes());
        let public_parent_matches = open_directory(&self.parent_path)
            .and_then(|parent| FileIdentity::from_file(&parent))
            .is_ok_and(|identity| identity == self.parent_identity);
        if public_parent_matches {
            Ok(self.parent_path.join(quarantine_name).join("checkout"))
        } else {
            Ok(PathBuf::from(format!(
                "<detached pinned Git quarantine from {}>",
                self.parent_path.display()
            ))
            .join(quarantine_name)
            .join("checkout"))
        }
    }
}

static QUARANTINE_NONCE: AtomicU64 = AtomicU64::new(0);

fn create_quarantine_directory(parent: &fs::File) -> io::Result<(CString, fs::File)> {
    for _ in 0..128 {
        let nonce = QUARANTINE_NONCE.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!(".cast-git-rejected-{}-{nonce}", std::process::id()))
            .expect("generated quarantine name contains no NUL");
        let result = unsafe { nix::libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
        if result == 0 {
            let directory = match openat_directory(parent, &name) {
                Ok(directory) => directory,
                Err(source) => return Err(cleanup_quarantine_error(parent, &name, source)),
            };
            if let Err(source) = directory.set_permissions(std::fs::Permissions::from_mode(0o700)) {
                drop(directory);
                return Err(cleanup_quarantine_error(parent, &name, source));
            }
            if let Err(source) = directory.sync_all().and_then(|()| parent.sync_all()) {
                drop(directory);
                return Err(cleanup_quarantine_error(parent, &name, source));
            }
            return Ok((name, directory));
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::AlreadyExists {
            return Err(source);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve a rejected Git checkout quarantine",
    ))
}

fn cleanup_quarantine_error(parent: &fs::File, name: &std::ffi::CStr, source: io::Error) -> io::Error {
    match remove_empty_quarantine(parent, name) {
        Ok(()) => source,
        Err(cleanup) => io::Error::new(
            source.kind(),
            format!("{source}; removing the incomplete Git quarantine also failed: {cleanup}"),
        ),
    }
}

fn remove_empty_quarantine(parent: &fs::File, name: &std::ffi::CStr) -> io::Result<()> {
    let result = unsafe { nix::libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), nix::libc::AT_REMOVEDIR) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    parent.sync_all()
}

fn identity_at(parent: &fs::File, name: &std::ffi::CStr) -> io::Result<Option<FileIdentity>> {
    let mut stat = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
    let result = unsafe {
        nix::libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            nix::libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == -1 {
        let source = io::Error::last_os_error();
        return if source.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(source)
        };
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & nix::libc::S_IFMT != nix::libc::S_IFDIR {
        return Ok(None);
    }
    Ok(Some(FileIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
    }))
}

fn regular_identity_at(parent: &fs::File, name: &std::ffi::CStr) -> io::Result<Option<FileIdentity>> {
    let mut stat = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
    let result = unsafe {
        nix::libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            nix::libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == -1 {
        let source = io::Error::last_os_error();
        return if source.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(source)
        };
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & nix::libc::S_IFMT != nix::libc::S_IFREG || stat.st_nlink != 1 {
        return Ok(None);
    }
    Ok(Some(FileIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
    }))
}

fn path_file_name(path: &Path, description: &'static str) -> io::Result<CString> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("{description} has no file name")))?;
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("{description} contains NUL")))
}

fn open_parent_directory(path: &Path, description: &'static str) -> io::Result<fs::File> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("{description} has no parent")))?;
    open_directory(parent)
}

fn open_directory(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(path)
}

fn openat_directory(parent: &fs::File, name: &std::ffi::CStr) -> io::Result<fs::File> {
    let descriptor = unsafe {
        nix::libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW,
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(fs::File::from_parts(
        descriptor.into(),
        Path::new("<descriptor-rooted Git checkout directory>"),
    ))
}

fn openat_lock_file(parent: &fs::File, name: &std::ffi::CStr) -> io::Result<fs::File> {
    let descriptor = unsafe {
        nix::libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            nix::libc::O_RDWR | nix::libc::O_CREAT | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0o600,
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(fs::File::from_parts(
        descriptor.into(),
        Path::new("<descriptor-rooted Git cache lock>"),
    ))
}

/// Atomically install a verified checkout without ever replacing or following
/// a destination that appeared after the source-root preflight.
#[cfg(test)]
fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    let source_parent = open_parent_directory(source, "staged checkout")?;
    let target_parent = open_parent_directory(target, "final checkout")?;
    let source_name = path_file_name(source, "staged checkout")?;
    let target_name = path_file_name(target, "final checkout")?;
    renameat_noreplace(&source_parent, &source_name, &target_parent, &target_name)
}

fn renameat_noreplace(
    source_parent: &fs::File,
    source_name: &std::ffi::CStr,
    target_parent: &fs::File,
    target_name: &std::ffi::CStr,
) -> io::Result<()> {
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            source_parent.as_raw_fd(),
            source_name.as_ptr(),
            target_parent.as_raw_fd(),
            target_name.as_ptr(),
            1_u32, // RENAME_NOREPLACE
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Possible errors returned by functions in this module.
#[derive(Debug, Error)]
pub enum Error {
    /// An error occurred while handling a Git repository.
    #[error("{0}")]
    Git(#[from] gitwrap::Error),
    /// A cache entry belongs to a different canonical source URL.
    #[error("cached Git mirror at {cache:?} does not belong to the requested source URL")]
    OriginMismatch { cache: PathBuf },
    /// A previous in-place fetch did not reach its durable commit point.
    #[error("cached Git mirror at {cache:?} has an incomplete fetch marker")]
    IncompleteCache { cache: PathBuf },
    /// Another process currently owns the cache mutation boundary.
    #[error("cached Git mirror at {cache:?} is busy in another process")]
    CacheBusy { cache: PathBuf },
    /// Submodules require their own explicit, locked source model.
    #[error("Git commit {commit} contains submodules, which are not supported as implicit sources")]
    UnsupportedSubmodules { commit: String },
    /// A frozen source has no expected normalized-tree identity.
    #[error("Git source {index} at commit {commit} has no locked materialization digest")]
    MissingMaterializationDigest { index: usize, commit: String },
    /// A caller attempted to export over an existing path of any type.
    #[error("refusing to export Git source over existing destination {0:?}")]
    DestinationExists(PathBuf),
    /// A build-visible checkout destination had no containing directory.
    #[error("Git checkout destination has no parent: {0:?}")]
    MissingDestinationParent(PathBuf),
    /// A private staging directory could not be created beside the final path.
    #[error("create private Git checkout staging directory in {parent:?}")]
    CreateStaging {
        parent: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The verified staging tree could not be installed atomically.
    #[error("atomically install verified Git checkout from {source_path:?} at {destination:?}")]
    Install {
        source_path: PathBuf,
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The normalized checkout differs from the bytes admitted by lock refresh.
    #[error("Git source {index} at commit {commit} materialized as {found}, but sources.lock.glu requires {expected}")]
    MaterializationDigestMismatch {
        index: usize,
        commit: String,
        expected: String,
        found: String,
    },
    /// Canonical tree normalization or hashing failed.
    #[error("normalize and hash Git materialization at {root:?}")]
    Materialization {
        root: PathBuf,
        #[source]
        source: materialization::Error,
    },
    /// Post-publication verification failed and the rejected inode was moved
    /// out of the build-visible destination without following it.
    #[error("rejected installed Git materialization at {destination:?}; quarantined at {quarantine:?}")]
    RejectedInstalledMaterialization {
        destination: PathBuf,
        quarantine: PathBuf,
        #[source]
        source: materialization::Error,
    },
    /// Verification failed and moving the rejected public name into a private
    /// no-replace quarantine also failed.
    #[error("failed to quarantine rejected Git materialization at {destination:?}: {cleanup}")]
    RejectedInstallCleanup {
        destination: PathBuf,
        #[source]
        verification: Box<materialization::Error>,
        cleanup: io::Error,
    },
    /// A generic I/O error occurred.
    #[error("{0}")]
    Io(#[from] io::Error),
}

async fn clone(url: &Url, path: &Path, pb: &ProgressBar) -> Result<gitwrap::Repository, gitwrap::Error> {
    let (progress, reporter) = progress_reporter(pb);

    let result = gitwrap::Repository::clone_mirror_progress_with_limits(path, url, MASON_GIT_LIMITS, progress).await;
    let _ = reporter.await;
    pb.finish_and_clear();

    result
}

async fn fetch(repo: &gitwrap::Repository, pb: &ProgressBar) -> Result<(), gitwrap::Error> {
    let (progress, reporter) = progress_reporter(pb);

    let result = repo.fetch_progress(progress).await;
    let _ = reporter.await;
    pb.finish_and_clear();

    result
}

fn progress_reporter(pb: &ProgressBar) -> (mpsc::Sender<gitwrap::FetchProgress>, tokio::task::JoinHandle<()>) {
    pb.set_length(100);
    pb.set_style(
        ProgressStyle::with_template(" {spinner} |{percent:>3}%| {wide_msg} {prefix:>.dim} ")
            .unwrap()
            .tick_chars("--=≡■≡=--"),
    );
    let (sender, mut receiver) = mpsc::channel::<gitwrap::FetchProgress>(64);
    let pb = pb.clone();
    let reporter = tokio::spawn(async move {
        while let Some(progress) = receiver.recv().await {
            pb.set_position(u64::from(progress.percent));
            pb.set_prefix(progress.speed);
        }
    });
    (sender, reporter)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, os::unix::fs::symlink, process::Command};

    use super::*;

    fn fixture_git(repository: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn source(url: Url) -> Git {
        Git {
            url,
            commit: "HEAD".to_owned(),
            name: "source".to_owned(),
            original_index: 0,
            materialization_sha256: None,
        }
    }

    fn create_repository(path: &Path, contents: &[u8]) -> String {
        fs::create_dir(path).unwrap();
        fixture_git(path, &["init", "--initial-branch=main"]);
        fixture_git(path, &["config", "user.name", "Cast Test"]);
        fixture_git(path, &["config", "user.email", "cast@example.invalid"]);
        fs::write(path.join("source.txt"), contents).unwrap();
        fixture_git(path, &["add", "source.txt"]);
        fixture_git(path, &["commit", "-m", "source"]);
        fixture_git(path, &["rev-parse", "HEAD"])
    }

    #[test]
    fn cache_identity_binds_the_complete_canonical_url() {
        let urls = [
            "https://alice:secret@example.invalid:8443/org/repo.git?transport=one#release",
            "http://alice:secret@example.invalid:8443/org/repo.git?transport=one#release",
            "https://bob:secret@example.invalid:8443/org/repo.git?transport=one#release",
            "https://alice:different@example.invalid:8443/org/repo.git?transport=one#release",
            "https://alice:secret@example.invalid:9443/org/repo.git?transport=one#release",
            "https://alice:secret@example.invalid:8443/other/repo.git?transport=one#release",
            "https://alice:secret@example.invalid:8443/org/repo.git?transport=two#release",
            "https://alice:secret@example.invalid:8443/org/repo.git?transport=one#other",
        ]
        .map(|url| Url::parse(url).unwrap());

        let names = urls
            .iter()
            .map(|url| {
                let name = source(url.clone()).directory_name();
                let name = name.to_str().unwrap();
                let expected_digest = format!("{:x}", Sha256::digest(url.as_str().as_bytes()));
                assert!(name.starts_with("repo-"));
                assert!(name.ends_with(&expected_digest));
                assert!(
                    name.bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                );
                assert!(!name.contains("alice"));
                assert!(!name.contains("secret"));
                name.to_owned()
            })
            .collect::<HashSet<_>>();

        assert_eq!(names.len(), urls.len());
    }

    #[test]
    fn cache_identity_never_uses_unsafe_url_path_bytes() {
        let url = Url::parse("https://example.invalid/a/%2E%2E/%2Fbad%5Cname%00.git?path=/tmp/escape").unwrap();
        let name = source(url).directory_name();
        let name = name.to_str().unwrap();

        assert_eq!(Path::new(name).components().count(), 1);
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        );
        assert!(!matches!(name, "." | ".."));
    }

    #[tokio::test]
    async fn mismatched_cache_origin_is_rejected_and_repaired_before_reuse() {
        let temporary = tempfile::tempdir().unwrap();
        let requested_path = temporary.path().join("requested");
        let wrong_path = temporary.path().join("wrong");
        let requested_commit = create_repository(&requested_path, b"requested source\n");
        create_repository(&wrong_path, b"wrong source\n");
        let requested_url = Url::from_directory_path(&requested_path).unwrap();
        let wrong_url = Url::from_directory_path(&wrong_path).unwrap();
        let requested = source(requested_url.clone());
        let storage = temporary.path().join("storage");
        let cached_path = requested.stored_path(&storage);
        fs::create_dir_all(cached_path.parent().unwrap()).unwrap();
        gitwrap::Repository::clone_mirror(&cached_path, &wrong_url)
            .await
            .unwrap();

        match requested.stored(&storage).await {
            Err(Error::OriginMismatch { cache }) => assert_eq!(cache, cached_path),
            Err(error) => panic!("unexpected cache error: {error}"),
            Ok(_) => panic!("a mirror for another origin was accepted"),
        }

        let stored = requested.store(&storage, &ProgressBar::new(100)).await.unwrap();
        assert_eq!(
            stored.repo.get_remote_url("origin").await.unwrap(),
            requested_url.as_str()
        );
        assert_eq!(stored.resolved_hash, requested_commit);
    }

    #[tokio::test]
    async fn failed_cache_fetch_is_purged_before_a_later_retry() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source");
        create_repository(&source_path, b"source\n");
        let requested = source(Url::from_directory_path(&source_path).unwrap());
        let storage = temporary.path().join("storage");
        requested.store(&storage, &ProgressBar::new(100)).await.unwrap();
        let cached_path = requested.stored_path(&storage);
        assert!(cached_path.is_dir());

        fs::remove_dir_all(&source_path).unwrap();
        assert!(requested.resolve(&storage, &ProgressBar::new(100)).await.is_err());
        assert!(
            !cached_path.exists(),
            "a failed in-place fetch must not leave a cache eligible for reuse"
        );
    }

    #[tokio::test]
    async fn same_url_sources_serialize_on_the_shared_cache() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source");
        let commit = create_repository(&source_path, b"source\n");
        let source_url = Url::from_directory_path(&source_path).unwrap();
        let first = source(source_url.clone());
        let mut second = source(source_url);
        second.name = "second-materialization".to_owned();
        second.original_index = 1;
        let storage = temporary.path().join("storage");
        let first_progress = ProgressBar::new(100);
        let second_progress = ProgressBar::new(100);

        let (first_stored, second_stored) = tokio::join!(
            first.store(&storage, &first_progress),
            second.store(&storage, &second_progress),
        );

        assert_eq!(first_stored.unwrap().resolved_hash, commit);
        assert_eq!(second_stored.unwrap().resolved_hash, commit);
        assert_eq!(first.stored_path(&storage), second.stored_path(&storage));
    }

    #[test]
    fn cache_lock_rejects_parent_symlinks_and_detects_lock_inode_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let requested = source(Url::parse("https://example.invalid/source.git").unwrap());
        let storage = temporary.path().join("storage");
        let outside = temporary.path().join("outside");
        fs::create_dir(&storage).unwrap();
        fs::create_dir(&outside).unwrap();
        symlink(&outside, storage.join("git")).unwrap();
        assert!(requested.open_cache_lock(&storage).is_err());
        assert!(fs::read_dir(&outside).unwrap().next().is_none());

        fs::remove_file(storage.join("git")).unwrap();
        let opened = requested.open_cache_lock(&storage).unwrap();
        let lock_path = requested.cache_lock_path(&storage);

        fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let error = opened.verify_name().unwrap_err();
        assert!(error.to_string().contains("private mode"));
        fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        opened.verify_name().unwrap();

        let displaced = lock_path.with_extension("displaced");
        fs::rename(&lock_path, &displaced).unwrap();
        fs::write(&lock_path, b"replacement").unwrap();
        let error = opened.verify_name().unwrap_err();
        assert!(error.to_string().contains("inode"));
        assert_eq!(fs::read(&lock_path).unwrap(), b"replacement");
    }

    #[tokio::test]
    async fn live_cache_owner_is_waited_for_and_interrupted_marker_is_repaired() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source");
        let commit = create_repository(&source_path, b"source\n");
        let requested = source(Url::from_directory_path(&source_path).unwrap());
        let storage = temporary.path().join("storage");
        requested.store(&storage, &ProgressBar::new(100)).await.unwrap();
        let live_lock = tokio::time::timeout(
            Duration::from_secs(2),
            requested.acquire_cache_lock(&storage, CacheLockMode::Exclusive),
        )
        .await
        .expect("the completed store must release its cache lock")
        .unwrap();
        let marker = requested.begin_cache_mutation(&storage).unwrap();
        let marker_path = marker.path.clone();

        let waiting_progress = ProgressBar::new(100);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), requested.store(&storage, &waiting_progress),)
                .await
                .is_err(),
            "a concurrent cache caller must wait instead of poisoning its peer through CacheBusy",
        );
        assert!(
            requested.stored_path(&storage).is_dir(),
            "a concurrent caller must not delete a mirror beneath the lock owner"
        );
        drop(live_lock); // model the mutation owner exiting unexpectedly
        drop(marker);

        assert!(matches!(
            requested.stored(&storage).await,
            Err(Error::IncompleteCache { .. })
        ));
        assert!(requested.stored_path(&storage).is_dir());
        assert!(marker_path.is_file());

        // Acquiring the exclusive lock proves that no cooperating mutation
        // owner is still live. Repair discards the marked mirror; it never
        // reuses potentially partial state.
        let repaired = requested.store(&storage, &ProgressBar::new(100)).await.unwrap();
        assert_eq!(repaired.resolved_hash, commit);
        assert!(!marker_path.exists());
    }

    #[test]
    fn verified_checkout_install_never_replaces_a_destination_symlink() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("staged");
        let destination = temporary.path().join("destination");
        let outside = temporary.path().join("outside");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("source.txt"), b"verified").unwrap();
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &destination).unwrap();

        let error = rename_noreplace(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(source.join("source.txt").is_file());
        assert!(fs::symlink_metadata(&destination).unwrap().file_type().is_symlink());
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn rejected_install_quarantine_refuses_to_move_a_replacement_inode() {
        let temporary = tempfile::tempdir().unwrap();
        let staged = temporary.path().join("staged");
        let destination = temporary.path().join("destination");
        let displaced = temporary.path().join("displaced-verified");
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("source.txt"), b"verified").unwrap();
        let installed = PinnedInstall::install(&staged, &destination).unwrap();

        fs::rename(&destination, &displaced).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("attacker.txt"), b"replacement").unwrap();
        let error = installed.quarantine().unwrap_err();

        assert!(error.to_string().contains("replacement Git checkout inode"));
        assert_eq!(fs::read(destination.join("attacker.txt")).unwrap(), b"replacement");
        assert_eq!(fs::read(displaced.join("source.txt")).unwrap(), b"verified");
        assert!(fs::read_dir(temporary.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .as_bytes()
                .starts_with(b".cast-git-rejected-")
        }));
    }

    #[test]
    fn quarantine_cleanup_removes_empty_entries_and_reports_cleanup_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = open_directory(temporary.path()).unwrap();

        let (empty_name, empty) = create_quarantine_directory(&parent).unwrap();
        drop(empty);
        let source = io::Error::new(io::ErrorKind::Interrupted, "primary quarantine failure");
        let error = cleanup_quarantine_error(&parent, &empty_name, source);
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(error.to_string(), "primary quarantine failure");
        assert_eq!(identity_at(&parent, &empty_name).unwrap(), None);

        let (nonempty_name, nonempty) = create_quarantine_directory(&parent).unwrap();
        let public_name = std::ffi::OsStr::from_bytes(nonempty_name.as_bytes());
        fs::write(temporary.path().join(public_name).join("retained"), b"data").unwrap();
        drop(nonempty);
        let source = io::Error::new(io::ErrorKind::Interrupted, "primary quarantine failure");
        let error = cleanup_quarantine_error(&parent, &nonempty_name, source);
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(
            error
                .to_string()
                .contains("removing the incomplete Git quarantine also failed")
        );
        assert!(temporary.path().join(public_name).join("retained").is_file());
    }

    #[test]
    fn rejected_install_quarantine_stays_with_the_pinned_parent_after_path_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("parent");
        let moved_parent = temporary.path().join("moved-parent");
        let staged = temporary.path().join("staged");
        fs::create_dir(&parent).unwrap();
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("source.txt"), b"verified").unwrap();
        let destination = parent.join("destination");
        let installed = PinnedInstall::install(&staged, &destination).unwrap();

        fs::rename(&parent, &moved_parent).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::create_dir(parent.join("destination")).unwrap();
        fs::write(parent.join("destination/attacker.txt"), b"replacement").unwrap();
        let reported = installed.quarantine().unwrap();

        assert_eq!(
            fs::read(parent.join("destination/attacker.txt")).unwrap(),
            b"replacement"
        );
        assert!(
            reported.to_string_lossy().contains("detached pinned Git quarantine"),
            "a replaced public parent must not produce a misleading live path: {reported:?}"
        );
        let quarantine = fs::read_dir(&moved_parent)
            .unwrap()
            .find_map(|entry| {
                let entry = entry.unwrap();
                entry
                    .file_name()
                    .as_bytes()
                    .starts_with(b".cast-git-rejected-")
                    .then(|| entry.path())
            })
            .expect("rejected checkout remains beneath the pinned original parent");
        assert_eq!(fs::read(quarantine.join("checkout/source.txt")).unwrap(), b"verified");
    }

    #[tokio::test]
    async fn failed_materialization_verification_leaves_no_checkout_or_staging_tree() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source");
        let commit = create_repository(&source_path, b"locked source\n");
        let source_url = Url::from_directory_path(&source_path).unwrap();
        let mirror_path = temporary.path().join("mirror.git");
        let repo = gitwrap::Repository::clone_mirror(&mirror_path, &source_url)
            .await
            .unwrap();
        let stored = StoredGit {
            name: "source".to_owned(),
            was_cached: false,
            resolved_hash: commit,
            original_index: 0,
            materialization_sha256: Some("0".repeat(64)),
            repo,
        };
        let share_root = temporary.path().join("share");
        fs::create_dir(&share_root).unwrap();

        assert!(matches!(
            stored.share(&share_root.join("source"), 0).await,
            Err(Error::MaterializationDigestMismatch { .. })
        ));
        assert!(fs::read_dir(share_root).unwrap().next().is_none());
    }

    #[test]
    fn exported_git_tree_removes_only_git_administration_state() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join(".git");
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/.git"), b"gitdir: ../.git/modules/nested\n").unwrap();
        fs::write(root.join("regular"), b"regular").unwrap();
        symlink("regular", root.join("link")).unwrap();
        fs::write(root.join(".git-marker"), b"ordinary committed name").unwrap();

        materialization::remove_git_administration_bounded(&root).unwrap();

        assert!(root.is_dir());
        assert!(!root.join(".git").exists());
        assert!(!root.join("nested/.git").exists());
        assert!(
            fs::symlink_metadata(root.join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(root.join(".git-marker").is_file());
    }
}
