// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    ffi::{CString, OsStr, OsString},
    io,
    os::{fd::AsRawFd as _, unix::ffi::OsStrExt},
    path::{Path, PathBuf},
    time::Duration,
};

use forge::util;
use fs_err as fs;
use fs_err::os::unix::fs::OpenOptionsExt as _;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::{Instant, sleep};
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
        let mut cached = true;
        match self.stored_locked(storage_dir).await {
            Ok((stored, has_commit)) => {
                repo = stored.repo;
                if !has_commit {
                    cached = false;
                    self.fetch_cached(storage_dir, &repo, pb).await?;
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

        let resolved_hash = repo.peel_commit(&self.commit).await?;
        reject_gitlinks(&repo, &resolved_hash).await?;

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
        let repo = match self.stored_locked(storage_dir).await {
            Ok((stored, _)) => {
                self.fetch_cached(storage_dir, &stored.repo, pb).await?;
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
        let resolved_hash = repo.peel_commit(&self.commit).await?;
        reject_gitlinks(&repo, &resolved_hash).await?;

        Ok(StoredGit {
            name: self.name().to_owned(),
            was_cached: false,
            repo,
            resolved_hash,
            original_index: self.original_index,
            materialization_sha256: self.materialization_sha256.clone(),
        })
    }

    /// Unconditionally removes the directory, within the storage
    /// directory, that would store the Git repository.
    /// If the directory does not exist, this function returns
    /// successfully (it is idempotent).
    ///
    /// Careful: this function does not validate the content
    /// of the directory! Resources will be deleted even if they
    /// do not belong to a Git repository.
    pub fn remove(&self, storage_dir: &Path) -> Result<(), Error> {
        let _cache_lock = self.try_acquire_cache_lock(storage_dir, CacheLockMode::Exclusive)?;
        self.remove_locked(storage_dir)
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
        let repo = gitwrap::Repository::open_bare_with_limits(&stored_path, MASON_GIT_LIMITS).await?;
        let origin = repo.get_remote_url("origin").await?;
        if origin != self.url.as_str() {
            return Err(Error::OriginMismatch { cache: stored_path });
        }
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
        let file = self.open_cache_lock(storage_dir)?;
        let deadline = Instant::now() + CACHE_LOCK_WAIT_TIMEOUT;
        loop {
            match try_lock_cache_file(&file, mode)? {
                true => return Ok(CacheLock { _file: file }),
                false if Instant::now() >= deadline => {
                    return Err(Error::CacheBusy {
                        cache: self.stored_path(storage_dir),
                    });
                }
                false => sleep(CACHE_LOCK_RETRY_INTERVAL).await,
            }
        }
    }

    fn try_acquire_cache_lock(&self, storage_dir: &Path, mode: CacheLockMode) -> Result<CacheLock, Error> {
        let file = self.open_cache_lock(storage_dir)?;
        if try_lock_cache_file(&file, mode)? {
            Ok(CacheLock { _file: file })
        } else {
            Err(Error::CacheBusy {
                cache: self.stored_path(storage_dir),
            })
        }
    }

    fn open_cache_lock(&self, storage_dir: &Path) -> Result<fs::File, Error> {
        let path = self.cache_lock_path(storage_dir);
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Git cache lock has no parent directory"))?;
        fs::create_dir_all(parent)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
            .open(&path)?;
        Ok(file)
    }

    fn begin_cache_mutation(&self, storage_dir: &Path) -> Result<CacheMutationMarker, Error> {
        let path = self.mutation_marker_path(storage_dir);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| {
                if source.kind() == io::ErrorKind::AlreadyExists {
                    Error::IncompleteCache {
                        cache: self.stored_path(storage_dir),
                    }
                } else {
                    source.into()
                }
            })?;
        use std::io::Write as _;
        file.write_all(b"incomplete Git cache mutation\n")?;
        file.sync_all()?;
        sync_parent_directory(&path)?;
        Ok(CacheMutationMarker { path })
    }

    async fn fetch_cached(
        &self,
        storage_dir: &Path,
        repo: &gitwrap::Repository,
        pb: &ProgressBar,
    ) -> Result<(), Error> {
        let marker = self.begin_cache_mutation(storage_dir)?;
        if let Err(source) = fetch(repo, pb).await {
            // Fetch mutates the cache in place. If its deadline, output, or
            // storage budget fails, discard it rather than trusting partial
            // state on a later build. The durable marker survives if cleanup
            // itself cannot complete.
            self.remove_locked(storage_dir)?;
            return Err(source.into());
        }
        if let Err(source) = marker.commit() {
            self.remove_locked(storage_dir)?;
            return Err(source.into());
        }
        Ok(())
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
    Shared,
    Exclusive,
}

fn try_lock_cache_file(file: &fs::File, mode: CacheLockMode) -> io::Result<bool> {
    let operation = match mode {
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
}

struct CacheMutationMarker {
    path: PathBuf,
}

impl CacheMutationMarker {
    fn commit(self) -> io::Result<()> {
        fs::remove_file(&self.path)?;
        sync_parent_directory(&self.path)
    }
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
        remove_git_administration(dest_dir)?;
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
        let found = self
            .export_normalized_with_root_access(&checkout, source_date_epoch, true)
            .await?;
        if found != expected {
            return Err(Error::MaterializationDigestMismatch {
                index: self.original_index,
                commit: self.resolved_hash.clone(),
                expected: expected.to_owned(),
                found,
            });
        }
        rename_noreplace(&checkout, dest_dir).map_err(|source| Error::Install {
            source_path: checkout,
            destination: dest_dir.to_owned(),
            source,
        })?;

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

fn remove_git_administration(root: &Path) -> Result<(), Error> {
    let entries = walkdir::WalkDir::new(root)
        // The export directory itself may legitimately be named `.git` by an
        // authored clone_dir. Only administration entries *inside* the export
        // are removable.
        .min_depth(1)
        .contents_first(true)
        .follow_links(false)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(walk_error)?;
    for entry in entries {
        if entry.file_name() != OsStr::new(".git") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn walk_error(error: walkdir::Error) -> io::Error {
    let message = error.to_string();
    error.into_io_error().unwrap_or_else(|| io::Error::other(message))
}

/// Atomically install a verified checkout without ever replacing or following
/// a destination that appeared after the source-root preflight.
fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "staged checkout path contains NUL"))?;
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "final checkout path contains NUL"))?;
    // nix exposes renameat2 only on some libc targets. Cast supports musl,
    // so use the Linux syscall directly with RENAME_NOREPLACE.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            nix::libc::AT_FDCWD,
            source.as_ptr(),
            nix::libc::AT_FDCWD,
            target.as_ptr(),
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
    /// A generic I/O error occurred.
    #[error("{0}")]
    Io(#[from] io::Error),
}

async fn clone(url: &Url, path: &Path, pb: &ProgressBar) -> Result<gitwrap::Repository, gitwrap::Error> {
    let cb = set_progress_bar_style(pb);

    let result = gitwrap::Repository::clone_mirror_progress_with_limits(path, url, MASON_GIT_LIMITS, cb).await;
    pb.finish_and_clear();

    result
}

async fn fetch(repo: &gitwrap::Repository, pb: &ProgressBar) -> Result<(), gitwrap::Error> {
    let cb = set_progress_bar_style(pb);

    let result = repo.fetch_progress(cb).await;
    pb.finish_and_clear();

    result
}

fn set_progress_bar_style(pb: &ProgressBar) -> impl Fn(gitwrap::FetchProgress) {
    pb.set_length(100);
    pb.set_style(
        ProgressStyle::with_template(" {spinner} |{percent:>3}%| {wide_msg} {prefix:>.dim} ")
            .unwrap()
            .tick_chars("--=≡■≡=--"),
    );

    |prog| {
        pb.set_position(prog.percent as u64);
        pb.set_prefix(prog.speed);
    }
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

        remove_git_administration(&root).unwrap();

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
