// SPDX-FileCopyrightText: 2026 AerynOS Developers

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

include!("git/error.rs");
include!("git/remote_transport.rs");

include!("git/tests.rs");
