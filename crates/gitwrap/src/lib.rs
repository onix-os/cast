// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Git repository manipulation utilities based
//! on the `git` executable.
//!
//! For any operation, `git` is called under the hood:
//! make sure it is available in your `$PATH`, otherwise
//! [Error] will be returned.
//!
//! Even though we are aware that calling executables is brittle API,
//! neither libgit2 nor gitoxide had all operations available in this
//! module implemented.

use std::{
    collections::BTreeMap,
    env,
    ffi::{CString, OsStr},
    io as std_io,
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{self, Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use fs_err as fs;
use fs_err::os::unix::fs::OpenOptionsExt as _;
use tokio::{
    io::{self, AsyncReadExt},
    process,
    sync::mpsc,
    time::{sleep, sleep_until, timeout, Instant},
};
use url::Url;

pub mod error;
pub use self::error::Error;
use error::{Constraint, InnerError};

/// Finite resource ceilings applied to one Git subprocess and to repositories
/// opened or produced by that subprocess. Aggregate repository accounting is
/// sampled and post-validated; only the per-file `RLIMIT_FSIZE` backstop is an
/// instantaneous kernel-enforced storage ceiling.
///
/// The defaults are intentionally generous, but never infinite. Production
/// callers should still construct this value explicitly so their policy is
/// visible at the trust boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Total wall-clock time for a single Git subprocess, including pipe
    /// draining and non-blocking progress parsing. Public progress delivery is
    /// lossy under channel backpressure and never awaits receiver code.
    pub wall_timeout: Duration,
    /// Time allowed to kill the private process group, reap Git, and observe
    /// all transport descendants disappear.
    pub termination_timeout: Duration,
    /// Maximum bytes captured from standard output.
    pub stdout_bytes: usize,
    /// Maximum bytes consumed from standard error in either ordinary or
    /// progress-reporting mode.
    pub stderr_bytes: usize,
    /// Maximum bytes in one carriage-return/newline-delimited progress record.
    pub progress_segment_bytes: usize,
    /// Maximum observed and final logical or allocated bytes in a mirror or
    /// checkout. This is not a hard instantaneous aggregate filesystem quota:
    /// data created and removed between scans can temporarily exceed it.
    pub repository_bytes: u64,
    /// Maximum filesystem entries in a mirror or checkout.
    pub repository_entries: u64,
    /// Maximum file descriptors inherited or opened by Git and transport
    /// helpers, capped by the launcher's lower hard limit.
    pub open_files: u64,
    /// Maximum virtual address space for Git and transport helpers, capped by
    /// the launcher's lower inherited limit.
    pub address_space_bytes: u64,
    /// Frequency of sampled repository quota checks while Git is mutating a tree.
    /// Scans are cooperative and deadline-checked between entries; like other
    /// local filesystem operations, one kernel filesystem call cannot be
    /// preempted in userspace.
    pub quota_poll_interval: Duration,
}

impl Limits {
    /// Finite defaults for callers which have not supplied a narrower policy.
    pub const DEFAULT: Self = Self {
        wall_timeout: Duration::from_secs(30 * 60),
        termination_timeout: Duration::from_secs(10),
        stdout_bytes: 64 * 1024 * 1024,
        stderr_bytes: 8 * 1024 * 1024,
        progress_segment_bytes: 64 * 1024,
        repository_bytes: 64 * 1024 * 1024 * 1024,
        repository_entries: 4_000_000,
        open_files: 4096,
        address_space_bytes: 16 * 1024 * 1024 * 1024,
        // Aggregate scans are intentionally much slower than progress polling:
        // rescanning a multi-million-entry repository every few milliseconds
        // would itself become a denial of service. RLIMIT_FSIZE remains the
        // immediate OS backstop and a mandatory post-scan closes the interval.
        quota_poll_interval: Duration::from_secs(2),
    };

    fn validate(self) -> Result<Self, Error> {
        if self.quota_poll_interval.is_zero()
            || self.termination_timeout.is_zero()
            || self.open_files == 0
            || self.address_space_bytes == 0
            || Instant::now().checked_add(self.wall_timeout).is_none()
            || Instant::now().checked_add(self.termination_timeout).is_none()
            || Instant::now().checked_add(self.quota_poll_interval).is_none()
        {
            Err(InnerError::InvalidLimits.into())
        } else {
            Ok(self)
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// An uninitialized repository, useful for unit tests.
pub fn null_repository() -> Repository {
    Repository {
        path: PathBuf::new(),
        limits: Limits::DEFAULT,
        identity: None,
        mirror: None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RepositoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectFormat {
    Sha1,
    Sha256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MirrorIdentity {
    origin: Url,
    object_format: ObjectFormat,
}

impl RepositoryIdentity {
    fn from_directory(directory: &fs::File) -> Result<Self, Error> {
        let metadata = directory.metadata().map_err(InnerError::from)?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    fn verify(self, directory: &fs::File) -> Result<(), Error> {
        if Self::from_directory(directory)? == self {
            Ok(())
        } else {
            Err(InnerError::RepositoryRootChanged.into())
        }
    }
}

/// A Git repository.
#[derive(Debug)]
pub struct Repository {
    path: PathBuf,
    limits: Limits,
    /// Rejects replacement between operations without retaining one descriptor
    /// per cached repository for the lifetime of this object.
    identity: Option<RepositoryIdentity>,
    /// Cache-owned mirrors retain the origin and object format admitted before
    /// their local configuration was reduced to the canonical safe subset.
    /// Fetches can therefore repair hostile local config without trusting it.
    mirror: Option<MirrorIdentity>,
}

impl Repository {
    /// Opens a local bare Git repository.
    /// If the Git repository at `path` is not bare,
    /// an [Error] containing [Constraint::NotBare] is returned.
    pub async fn open_bare(path: &Path) -> Result<Self, Error> {
        Self::open_bare_with_limits(path, Limits::DEFAULT).await
    }

    /// Opens a local bare Git repository under an explicit resource policy.
    pub async fn open_bare_with_limits(path: &Path, limits: Limits) -> Result<Self, Error> {
        let limits = limits.validate()?;
        let path = path::absolute(path).map_err(InnerError::from)?;
        let root = open_repository_directory(&path)?;
        verify_repository_path_identity(&path, &root)?;
        verify_repository_usage_directory(&root, limits)?;
        let output = run_git_in_directory(
            &[OsStr::new("repo"), OsStr::new("info"), OsStr::new("layout.bare")],
            limits,
            &root,
            None,
            None::<fn(FetchProgress)>,
        )
        .await?;
        if !output.stdout.starts_with(b"layout.bare=true") {
            return Err(InnerError::Constraint(Constraint::NotBare))?;
        }
        verify_repository_path_identity(&path, &root)?;
        verify_repository_usage_directory(&root, limits)?;
        let identity = RepositoryIdentity::from_directory(&root)?;
        Ok(Self {
            path,
            limits,
            identity: Some(identity),
            mirror: None,
        })
    }

    /// Opens a cache-owned mirror for one exact origin. Unlike [`Self::open_bare`],
    /// this boundary may restrict permissions and replace local Git config.
    /// The direct `remote.origin.url`, bare flag, and object format are read
    /// with includes disabled before the config is rewritten, so a cache for a
    /// different origin is never silently repointed and accepted.
    pub async fn open_private_mirror_with_limits(path: &Path, origin: &Url, limits: Limits) -> Result<Self, Error> {
        let limits = limits.validate()?;
        validate_transport_url(origin)?;
        let path = path::absolute(path).map_err(InnerError::from)?;
        let root = open_repository_directory(&path)?;
        verify_repository_path_identity(&path, &root)?;
        verify_repository_usage_directory(&root, limits)?;
        secure_mirror_permissions(&root)?;
        let object_format = inspect_private_mirror_config(&root, origin, limits).await?;
        write_canonical_mirror_config(&root, origin, object_format)?;
        verify_bare_repository(&root, limits).await?;
        verify_repository_path_identity(&path, &root)?;
        verify_repository_usage_directory(&root, limits)?;
        secure_mirror_permissions(&root)?;
        verify_canonical_mirror_config(&root, origin, object_format)?;
        let identity = RepositoryIdentity::from_directory(&root)?;
        Ok(Self {
            path,
            limits,
            identity: Some(identity),
            mirror: Some(MirrorIdentity {
                origin: origin.clone(),
                object_format,
            }),
        })
    }

    /// Clones a local or remote Git repository as bare into `path`.
    /// The clone is performed with Git's `--mirror` flag.
    pub async fn clone_mirror(path: &Path, url: &Url) -> Result<Self, Error> {
        Self::clone_mirror_with_limits(path, url, Limits::DEFAULT).await
    }

    /// Clones a mirror under an explicit deadline/output/storage policy.
    pub async fn clone_mirror_with_limits(path: &Path, url: &Url, limits: Limits) -> Result<Self, Error> {
        clone_mirror_impl(path, url, limits, None::<fn(FetchProgress)>).await
    }

    /// Clones a local or remote Git repository as bare into `path`.
    /// The clone is performed with Git's `--mirror` flag.
    /// Progress records are offered to a bounded Tokio channel without
    /// waiting. A full or closed channel drops records rather than delaying
    /// stderr draining, quota enforcement, or the subprocess wall deadline.
    pub async fn clone_mirror_progress(
        path: &Path,
        url: &Url,
        progress: mpsc::Sender<FetchProgress>,
    ) -> Result<Self, Error> {
        Self::clone_mirror_progress_with_limits(path, url, Limits::DEFAULT, progress).await
    }

    /// Clones a progress-reporting mirror under an explicit resource policy.
    /// Delivery is non-blocking and deliberately lossy under backpressure.
    pub async fn clone_mirror_progress_with_limits(
        path: &Path,
        url: &Url,
        limits: Limits,
        progress: mpsc::Sender<FetchProgress>,
    ) -> Result<Self, Error> {
        clone_mirror_impl(path, url, limits, Some(progress_callback(progress))).await
    }

    /// Whether this repository has a commit identified by its hash.
    pub async fn has_commit(&self, commit: &str) -> Result<bool, Error> {
        validate_revision_argument(commit)?;
        let root = self.transaction_root()?;
        self.verify_usage(&root)?;
        let output = run_git_in_directory(
            &[
                OsStr::new("cat-file"),
                OsStr::new("-t"),
                OsStr::new("--"),
                OsStr::new(commit),
            ],
            self.limits,
            &root,
            None,
            None::<fn(FetchProgress)>,
        )
        .await?;
        self.verify_identity(&root)?;
        Ok(output.stdout.starts_with(b"commit"))
    }

    /// Returns the hash of the commit. If a commit hash is passed,
    /// the output is equal to `commit`. If a Git reference is passed, the
    /// reference is peeled through annotated tags into the commit object.
    pub async fn peel_commit(&self, commit: &str) -> Result<String, Error> {
        validate_revision_argument(commit)?;
        let commit = format!("{commit}^{{commit}}");
        let root = self.transaction_root()?;
        self.verify_usage(&root)?;
        let output = run_git_in_directory(
            &[
                OsStr::new("rev-parse"),
                OsStr::new("--verify"),
                OsStr::new("--end-of-options"),
                OsStr::new(&commit),
            ],
            self.limits,
            &root,
            None,
            None::<fn(FetchProgress)>,
        )
        .await?;
        let object_id = str::from_utf8(output.stdout.trim_ascii_end()).unwrap_or("");
        if !matches!(object_id.len(), 40 | 64)
            || !object_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(InnerError::Run { code: None })?;
        }
        self.verify_identity(&root)?;
        Ok(object_id.to_owned())
    }

    /// Returns the remote URL for the provided `remote`
    pub async fn get_remote_url(&self, remote: &str) -> Result<String, Error> {
        validate_remote_argument(remote)?;
        let root = self.transaction_root()?;
        self.verify_usage(&root)?;
        let output = run_git_in_directory(
            &[
                OsStr::new("remote"),
                OsStr::new("get-url"),
                OsStr::new("--"),
                OsStr::new(remote),
            ],
            self.limits,
            &root,
            None,
            None::<fn(FetchProgress)>,
        )
        .await?;
        self.verify_identity(&root)?;
        Ok(str::from_utf8(output.stdout.trim_ascii_end()).unwrap_or("").to_owned())
    }

    /// Sets the remote URL for the provided `remote` to `url`
    pub async fn set_remote_url(&self, remote: &str, url: &str) -> Result<(), Error> {
        validate_remote_argument(remote)?;
        validate_value_argument(url, "remote URL")?;
        let root = self.transaction_root()?;
        self.verify_usage(&root)?;
        run_git_in_directory(
            &[
                OsStr::new("remote"),
                OsStr::new("set-url"),
                OsStr::new("--"),
                OsStr::new(remote),
                OsStr::new(url),
            ],
            self.limits,
            &root,
            Some(MonitoredRepository::directory(&root)?),
            None::<fn(FetchProgress)>,
        )
        .await?;
        self.verify_usage(&root)?;
        Ok(())
    }

    /// Checkout the provided `rev` (branch or commit).
    ///
    /// A failed checkout is preserved rather than deleting a repository this
    /// library did not create. Callers which own disposable staging state must
    /// discard it before retrying; Mason does so through its private checkout
    /// staging directory.
    pub async fn checkout(&self, rev: &str) -> Result<(), Error> {
        validate_revision_argument(rev)?;
        let root = self.transaction_root()?;
        self.verify_usage(&root)?;
        run_git_in_directory(
            &[
                OsStr::new("checkout"),
                OsStr::new("--detach"),
                OsStr::new("--force"),
                OsStr::new("--no-recurse-submodules"),
                OsStr::new(rev),
            ],
            self.limits,
            &root,
            Some(MonitoredRepository::directory(&root)?),
            None::<fn(FetchProgress)>,
        )
        .await?;
        self.verify_usage(&root)?;
        Ok(())
    }

    /// Equivalent to `git fetch`.
    /// Progress records are offered to a bounded channel without waiting.
    /// Records may be dropped when the receiver cannot keep up.
    ///
    /// Fetch mutates the repository. On failure, cache-owning callers must
    /// discard the repository before reuse. Gitwrap never implicitly deletes
    /// an arbitrary path supplied through [`Self::open_bare`].
    pub async fn fetch_progress(&self, progress: mpsc::Sender<FetchProgress>) -> Result<(), Error> {
        let root = self.transaction_root()?;
        self.verify_usage(&root)?;
        if let Some(mirror) = &self.mirror {
            validate_transport_url(&mirror.origin)?;
            write_canonical_mirror_config(&root, &mirror.origin, mirror.object_format)?;
            secure_mirror_permissions(&root)?;
            verify_canonical_mirror_config(&root, &mirror.origin, mirror.object_format)?;
        } else {
            let origin = self.get_remote_url("origin").await?;
            let origin = Url::parse(&origin).map_err(|_| InnerError::InvalidRemoteUrl)?;
            validate_transport_url(&origin)?;
        }
        run_git_in_directory(
            &[
                OsStr::new("fetch"),
                OsStr::new("--no-recurse-submodules"),
                OsStr::new("--progress"),
            ],
            self.limits,
            &root,
            Some(MonitoredRepository::directory(&root)?),
            Some(progress_callback(progress)),
        )
        .await?;
        self.verify_usage(&root)?;
        Ok(())
    }

    /// Clone the current [`Repository`] to the provided `path` without
    /// checking out its default branch, and return the cloned to
    /// [`Repository`].
    pub async fn clone_to(&self, path: &Path) -> Result<Self, Error> {
        let source_root = self.transaction_root()?;
        self.verify_usage(&source_root)?;
        let path = path::absolute(path).map_err(InnerError::from)?;
        let root = clone_to_staged(&source_root, &self.path, &path, self.limits).await?;
        if let Err(error) = self.verify_identity(&source_root) {
            // Only delete the installed checkout if its public name still
            // refers to the inode we created. A concurrent replacement is not
            // caller-owned staging and must never become cleanup collateral.
            if verify_repository_path_identity(&path, &root).is_ok() {
                if let Err(source) = remove_path(&path) {
                    return Err(InnerError::Cleanup(source).into());
                }
            }
            return Err(error);
        }
        verify_repository_path_identity(&path, &root)?;
        let identity = RepositoryIdentity::from_directory(&root)?;

        Ok(Self {
            path,
            limits: self.limits,
            identity: Some(identity),
            mirror: None,
        })
    }

    /// Whether `rev` contains Gitlink entries whose contents would require
    /// an additional, independently fetched submodule source graph.
    pub async fn contains_gitlinks(&self, rev: &str) -> Result<bool, Error> {
        validate_revision_argument(rev)?;
        let root = self.transaction_root()?;
        self.verify_usage(&root)?;
        let output = run_git_in_directory(
            &[
                OsStr::new("ls-tree"),
                OsStr::new("-r"),
                OsStr::new("--full-tree"),
                OsStr::new("--format=%(objectmode)"),
                OsStr::new("--"),
                OsStr::new(rev),
            ],
            self.limits,
            &root,
            None,
            None::<fn(FetchProgress)>,
        )
        .await?;
        self.verify_identity(&root)?;
        Ok(output.stdout.split(|byte| *byte == b'\n').any(|line| line == b"160000"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Resource policy inherited by all commands and clones from this
    /// repository.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Restrict a cache-owned mirror and its credential-bearing configuration
    /// to the current user. This is intentionally explicit: [`Self::open_bare`]
    /// also serves caller-owned repositories and must not silently chmod them.
    pub fn secure_private_mirror(&self) -> Result<(), Error> {
        let root = self.transaction_root()?;
        secure_mirror_permissions(&root)?;
        self.verify_identity(&root)
    }

    fn verify_usage(&self, root: &fs::File) -> Result<(), Error> {
        self.verify_identity(root)?;
        verify_repository_usage_directory(root, self.limits).map(|_| ())
    }

    fn transaction_root(&self) -> Result<fs::File, Error> {
        let root = open_repository_directory(&self.path)?;
        self.identity.ok_or(InnerError::InvalidRepositoryRoot)?.verify(&root)?;
        Ok(root)
    }

    fn verify_identity(&self, root: &fs::File) -> Result<(), Error> {
        verify_repository_path_identity(&self.path, root)?;
        self.identity.ok_or(InnerError::InvalidRepositoryRoot)?.verify(root)
    }
}

/// One advisory record sent while Git reports network progress.
#[derive(Debug)]
pub struct FetchProgress {
    /// Completion percentage.
    pub percent: u8,
    /// Download speed in formatted human units per second
    pub speed: String,
}

fn progress_callback(progress: mpsc::Sender<FetchProgress>) -> impl Fn(FetchProgress) {
    move |record| {
        // Progress is advisory. Never let user-controlled receiver scheduling
        // become part of the process supervisor's completion boundary.
        let _ = progress.try_send(record);
    }
}

const MAX_GIT_IDENTIFIER_BYTES: usize = 4096;
const MAX_GIT_VALUE_BYTES: usize = 64 * 1024;

fn validate_revision_argument(value: &str) -> Result<(), Error> {
    validate_non_option_argument(value, "revision")
}

fn validate_remote_argument(value: &str) -> Result<(), Error> {
    validate_non_option_argument(value, "remote name")
}

fn validate_non_option_argument(value: &str, argument: &'static str) -> Result<(), Error> {
    if value.is_empty() || value.starts_with('-') || value.len() > MAX_GIT_IDENTIFIER_BYTES {
        Err(InnerError::InvalidArgument { argument }.into())
    } else {
        Ok(())
    }
}

fn validate_value_argument(value: &str, argument: &'static str) -> Result<(), Error> {
    if value.is_empty() || value.len() > MAX_GIT_VALUE_BYTES {
        Err(InnerError::InvalidArgument { argument }.into())
    } else {
        Ok(())
    }
}

/// Reject unknown schemes before Git can dispatch a `git-remote-*` helper from
/// PATH. Every accepted scheme is handled by Git itself or its explicitly
/// constrained SSH transport.
fn validate_transport_url(url: &Url) -> Result<(), Error> {
    validate_value_argument(url.as_str(), "transport URL")?;
    match url.scheme() {
        "file" | "https" | "ssh" => Ok(()),
        scheme => Err(InnerError::UnsupportedTransportScheme {
            scheme: scheme.to_owned(),
        }
        .into()),
    }
}

async fn clone_mirror_impl<F>(path: &Path, url: &Url, limits: Limits, callback: Option<F>) -> Result<Repository, Error>
where
    F: Fn(FetchProgress),
{
    let limits = limits.validate()?;
    validate_transport_url(url)?;
    let path = path::absolute(path).map_err(InnerError::from)?;
    ensure_destination_absent(&path)?;
    let parent = path.parent().ok_or_else(|| {
        InnerError::Io(std_io::Error::new(
            std_io::ErrorKind::InvalidInput,
            "Git clone destination has no parent",
        ))
    })?;
    let staging = tempfile::Builder::new()
        .prefix(".gitwrap-mirror-")
        .tempdir_in(parent)
        .map_err(InnerError::from)?;
    let staged_path = staging.path().join("repository.git");
    let progress = callback.is_some();
    let result = run_git_monitored(
        [
            OsStr::new("clone"),
            OsStr::new("--mirror"),
            OsStr::new("--no-hardlinks"),
            OsStr::new("--no-recurse-submodules"),
            if progress {
                OsStr::new("--progress")
            } else {
                OsStr::new("--no-progress")
            },
            OsStr::new(url.as_str()),
            staged_path.as_os_str(),
        ],
        limits,
        &staged_path,
        callback,
    )
    .await;
    if let Err(error) = result {
        return close_staging_after_error(staging, error);
    }
    if let Err(error) = verify_repository_usage(&staged_path, limits) {
        return close_staging_after_error(staging, error);
    }
    // Pin the exact staged inode before exposing its name in the caller's
    // directory. Opening the final path after rename would allow a concurrent
    // replacement to make us return a handle for a repository we did not
    // validate.
    let root = match open_repository_directory(&staged_path) {
        Ok(root) => root,
        Err(error) => return close_staging_after_error(staging, error),
    };
    if let Err(error) = secure_mirror_permissions(&root) {
        return close_staging_after_error(staging, error);
    }
    let object_format = match inspect_private_mirror_config(&root, url, limits).await {
        Ok(object_format) => object_format,
        Err(error) => return close_staging_after_error(staging, error),
    };
    if let Err(error) = write_canonical_mirror_config(&root, url, object_format) {
        return close_staging_after_error(staging, error);
    }
    let identity = match RepositoryIdentity::from_directory(&root) {
        Ok(identity) => identity,
        Err(error) => return close_staging_after_error(staging, error),
    };
    if let Err(error) = rename_noreplace(&staged_path, &path) {
        return close_staging_after_error(staging, error);
    }
    if let Err(error) = verify_repository_path_identity(&path, &root) {
        return close_staging_and_remove_install(staging, &path, &root, error);
    }
    if let Err(error) =
        secure_mirror_permissions(&root).and_then(|()| verify_canonical_mirror_config(&root, url, object_format))
    {
        return close_staging_and_remove_install(staging, &path, &root, error);
    }
    if let Err(source) = staging.close() {
        let cleanup = remove_path(&path).err().unwrap_or(source);
        return Err(InnerError::Cleanup(cleanup).into());
    }

    Ok(Repository {
        path,
        limits,
        identity: Some(identity),
        mirror: Some(MirrorIdentity {
            origin: url.clone(),
            object_format,
        }),
    })
}

async fn clone_to_staged(
    source: &fs::File,
    source_path: &Path,
    path: &Path,
    limits: Limits,
) -> Result<fs::File, Error> {
    ensure_destination_absent(path)?;
    let parent = path.parent().ok_or_else(|| {
        InnerError::Io(std_io::Error::new(
            std_io::ErrorKind::InvalidInput,
            "Git clone destination has no parent",
        ))
    })?;
    let staging = tempfile::Builder::new()
        .prefix(".gitwrap-checkout-")
        .tempdir_in(parent)
        .map_err(InnerError::from)?;
    let staged_path = staging.path().join("checkout");
    let result = run_git_in_directory(
        [
            OsStr::new("clone"),
            OsStr::new("--no-checkout"),
            OsStr::new("--no-hardlinks"),
            OsStr::new("--no-recurse-submodules"),
            OsStr::new("."),
            staged_path.as_os_str(),
        ],
        limits,
        source,
        Some(MonitoredRepository::Path(staged_path.clone())),
        None::<fn(FetchProgress)>,
    )
    .await;
    if let Err(error) = result {
        return close_staging_after_error(staging, error);
    }
    let reset_origin = run_git(
        [
            OsStr::new("-C"),
            staged_path.as_os_str(),
            OsStr::new("remote"),
            OsStr::new("set-url"),
            OsStr::new("origin"),
            source_path.as_os_str(),
        ],
        limits,
    )
    .await;
    if let Err(error) = reset_origin {
        return close_staging_after_error(staging, error);
    }
    if let Err(error) = verify_repository_usage(&staged_path, limits) {
        return close_staging_after_error(staging, error);
    }
    let root = match open_repository_directory(&staged_path) {
        Ok(root) => root,
        Err(error) => return close_staging_after_error(staging, error),
    };
    if let Err(error) = rename_noreplace(&staged_path, path) {
        return close_staging_after_error(staging, error);
    }
    if let Err(error) = verify_repository_path_identity(path, &root) {
        return close_staging_and_remove_install(staging, path, &root, error);
    }
    if let Err(source) = staging.close() {
        let cleanup = remove_path(path).err().unwrap_or(source);
        return Err(InnerError::Cleanup(cleanup).into());
    }
    Ok(root)
}

fn close_staging_after_error<T>(staging: tempfile::TempDir, error: Error) -> Result<T, Error> {
    match staging.close() {
        Ok(()) => Err(error),
        Err(source) => Err(InnerError::Cleanup(source).into()),
    }
}

fn close_staging_and_remove_install<T>(
    staging: tempfile::TempDir,
    installed: &Path,
    installed_root: &fs::File,
    error: Error,
) -> Result<T, Error> {
    let remove_error = if verify_repository_path_identity(installed, installed_root).is_ok() {
        remove_path(installed).err()
    } else {
        None
    };
    let staging_error = staging.close().err();
    if let Some(source) = remove_error.or(staging_error) {
        Err(InnerError::Cleanup(source).into())
    } else {
        Err(error)
    }
}

fn ensure_destination_absent(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(InnerError::DestinationExists.into()),
        Err(error) if error.kind() == std_io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(InnerError::Io(error).into()),
    }
}

/// Atomically install a private clone without replacing a destination that
/// appeared after preflight.
fn rename_noreplace(source: &Path, target: &Path) -> Result<(), Error> {
    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        InnerError::Io(std_io::Error::new(
            std_io::ErrorKind::InvalidInput,
            "staged Git path contains NUL",
        ))
    })?;
    let target = CString::new(target.as_os_str().as_bytes()).map_err(|_| {
        InnerError::Io(std_io::Error::new(
            std_io::ErrorKind::InvalidInput,
            "final Git path contains NUL",
        ))
    })?;
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
    if result == 0 {
        Ok(())
    } else {
        let error = std_io::Error::last_os_error();
        if error.kind() == std_io::ErrorKind::AlreadyExists {
            Err(InnerError::DestinationExists.into())
        } else {
            Err(InnerError::Io(error).into())
        }
    }
}

/// Runs Git with bounded pipes and a finite process-group deadline.
async fn run_git<I, S>(args: I, limits: Limits) -> Result<std::process::Output, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = git_command(limits);
    command.args(args);
    run_command(command, limits, None, None::<fn(FetchProgress)>).await
}

async fn run_git_monitored<I, S, F>(
    args: I,
    limits: Limits,
    repository: &Path,
    callback: Option<F>,
) -> Result<std::process::Output, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
    F: Fn(FetchProgress),
{
    let mut command = git_command(limits);
    command.args(args);
    run_command(
        command,
        limits,
        Some(MonitoredRepository::Path(repository.to_owned())),
        callback,
    )
    .await
}

async fn run_git_in_directory<I, S, F>(
    args: I,
    limits: Limits,
    directory: &fs::File,
    monitored_repository: Option<MonitoredRepository>,
    callback: Option<F>,
) -> Result<std::process::Output, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
    F: Fn(FetchProgress),
{
    let mut command = git_command(limits);
    command.args(args);
    set_command_directory(&mut command, directory);
    run_command(command, limits, monitored_repository, callback).await
}

enum MonitoredRepository {
    Path(PathBuf),
    Directory(fs::File),
}

impl MonitoredRepository {
    fn directory(directory: &fs::File) -> Result<Self, Error> {
        Ok(Self::Directory(directory.try_clone().map_err(InnerError::from)?))
    }

    fn scanner(&self, limits: Limits) -> Result<RepositoryUsageScanner, Error> {
        match self {
            Self::Path(path) => RepositoryUsageScanner::new(path, limits, ScanMode::Live),
            Self::Directory(directory) => RepositoryUsageScanner::from_directory(
                directory.try_clone().map_err(InnerError::from)?,
                limits,
                ScanMode::Live,
            ),
        }
    }

    fn verify(&self, limits: Limits) -> Result<RepositoryUsage, Error> {
        match self {
            Self::Path(path) => verify_repository_usage_or_absent_for_creation(path, limits),
            Self::Directory(directory) => verify_repository_usage_directory(directory, limits),
        }
    }
}

async fn run_command<F>(
    mut command: process::Command,
    limits: Limits,
    monitored_repository: Option<MonitoredRepository>,
    callback: Option<F>,
) -> Result<std::process::Output, Error>
where
    F: Fn(FetchProgress),
{
    let limits = limits.validate()?;
    if let Some(repository) = monitored_repository.as_ref() {
        repository.verify(limits)?;
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let started = Instant::now();
    let deadline = started
        .checked_add(limits.wall_timeout)
        .ok_or(InnerError::InvalidLimits)?;
    let mut child = command.spawn().map_err(InnerError::from)?;
    let process_group = child
        .id()
        .ok_or_else(|| InnerError::Io(std_io::Error::other("spawned Git process has no process identifier")))?
        as i32;
    let mut process_group_guard = ProcessGroupGuard::new(process_group);
    let stdout = child.stdout.take().expect("piped Git stdout");
    let stderr = child.stderr.take().expect("piped Git stderr");
    let mut stdout_reader = Box::pin(read_bounded(stdout, "stdout", limits.stdout_bytes));
    let mut stderr_reader = Box::pin(read_stderr(stderr, limits, callback));
    // Run quota scans in this supervisor instead of detaching `spawn_blocking`
    // work which could accumulate after repeated cancellations. Each bounded
    // scan slice returns to `select!`, allowing stdout/stderr draining and
    // child-status handling to make progress between filesystem operations.
    let mut quota_tick = Box::pin(sleep(limits.quota_poll_interval));
    let mut quota_scanner = None;
    let mut status = None;
    let mut stdout = None;
    let mut stderr_done = false;
    let mut boundary_terminated = false;

    loop {
        if status.is_some() && stdout.is_some() && stderr_done {
            break;
        }
        tokio::select! {
            result = &mut stdout_reader, if stdout.is_none() => match result {
                Ok(bytes) => stdout = Some(bytes),
                Err(error) => return abort_with(&mut child, &mut process_group_guard, status.is_some(), boundary_terminated, limits, error).await,
            },
            result = &mut stderr_reader, if !stderr_done => match result {
                Ok(()) => stderr_done = true,
                Err(error) => return abort_with(&mut child, &mut process_group_guard, status.is_some(), boundary_terminated, limits, error).await,
            },
            result = child.wait(), if status.is_none() => match result {
                Ok(found) => {
                    status = Some(found);
                    // Git is the process-group leader. Once it exits, no
                    // transport/helper is allowed to keep the boundary or a
                    // captured pipe alive until the outer wall deadline.
                    terminate_boundary(
                        &mut child,
                        process_group,
                        true,
                        limits.termination_timeout,
                    )
                    .await?;
                    boundary_terminated = true;
                    process_group_guard.disarm();
                }
                Err(source) => {
                    let error = InnerError::Io(source).into();
                    return abort_with(&mut child, &mut process_group_guard, false, false, limits, error).await;
                }
            },
            () = &mut quota_tick, if monitored_repository.is_some() && status.is_none() => {
                let repository = monitored_repository.as_ref().expect("guarded repository root");
                if quota_scanner.is_none() {
                    match repository.scanner(limits) {
                        Ok(scanner) => quota_scanner = Some(scanner),
                        Err(error) => {
                            return abort_with(&mut child, &mut process_group_guard, status.is_some(), boundary_terminated, limits, error).await;
                        }
                    }
                }
                let complete = match quota_scanner
                    .as_mut()
                    .expect("initialized quota scanner")
                    .advance(512, Some(deadline))
                {
                    Ok(complete) => complete,
                    Err(error) => {
                        return abort_with(&mut child, &mut process_group_guard, status.is_some(), boundary_terminated, limits, error).await;
                    }
                };
                if complete {
                    quota_scanner = None;
                    quota_tick.as_mut().reset(Instant::now() + limits.quota_poll_interval);
                } else {
                    quota_tick.as_mut().reset(Instant::now());
                }
            },
            () = sleep_until(deadline) => {
                let error = InnerError::Timeout { timeout: limits.wall_timeout }.into();
                return abort_with(&mut child, &mut process_group_guard, status.is_some(), boundary_terminated, limits, error).await;
            }
        }
    }

    // Git transports belong to the same private process group. Kill any
    // descendant which survived the direct process even on a nominally
    // successful exit, then prove the group disappeared.
    if !boundary_terminated {
        terminate_boundary(&mut child, process_group, true, limits.termination_timeout).await?;
        process_group_guard.disarm();
    }
    let status = status.expect("completed Git status");
    if status.success() {
        Ok(std::process::Output {
            status,
            stdout: stdout.expect("completed Git stdout"),
            // Diagnostics are consumed under a byte ceiling but never exposed:
            // transports may repeat credential-bearing URLs.
            stderr: Vec::new(),
        })
    } else {
        Err(InnerError::Run { code: status.code() }.into())
    }
}

/// Cancellation safety for callers which drop the async operation before its
/// internal deadline resolves. Normal error paths additionally await direct
/// child reaping and group disappearance.
struct ProcessGroupGuard {
    process_group: i32,
    armed: bool,
}

impl ProcessGroupGuard {
    fn new(process_group: i32) -> Self {
        Self {
            process_group,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if self.armed {
            unsafe {
                nix::libc::kill(-self.process_group, nix::libc::SIGKILL);
            }
        }
    }
}

async fn abort_with<T>(
    child: &mut process::Child,
    process_group: &mut ProcessGroupGuard,
    already_reaped: bool,
    boundary_terminated: bool,
    limits: Limits,
    error: Error,
) -> Result<T, Error> {
    if !boundary_terminated {
        terminate_boundary(
            child,
            process_group.process_group,
            already_reaped,
            limits.termination_timeout,
        )
        .await?;
        process_group.disarm();
    }
    Err(error)
}

async fn terminate_boundary(
    child: &mut process::Child,
    process_group: i32,
    already_reaped: bool,
    termination_timeout: Duration,
) -> Result<(), Error> {
    signal_process_group(process_group, nix::libc::SIGKILL)?;
    let deadline = Instant::now()
        .checked_add(termination_timeout)
        .ok_or(InnerError::InvalidLimits)?;
    if !already_reaped {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match timeout(remaining, child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(source)) => return Err(InnerError::Io(source).into()),
            Err(_) => {
                return Err(InnerError::BoundaryTermination {
                    timeout: termination_timeout,
                }
                .into())
            }
        }
    }

    loop {
        if !process_group_exists(process_group)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(InnerError::BoundaryTermination {
                timeout: termination_timeout,
            }
            .into());
        }
        sleep(Duration::from_millis(5)).await;
    }
}

fn signal_process_group(process_group: i32, signal: i32) -> Result<(), Error> {
    let result = unsafe { nix::libc::kill(-process_group, signal) };
    if result == 0 {
        Ok(())
    } else {
        let error = std_io::Error::last_os_error();
        if error.raw_os_error() == Some(nix::libc::ESRCH) {
            Ok(())
        } else {
            Err(InnerError::Io(error).into())
        }
    }
}

fn process_group_exists(process_group: i32) -> Result<bool, Error> {
    let result = unsafe { nix::libc::kill(-process_group, 0) };
    if result == 0 {
        Ok(true)
    } else {
        let error = std_io::Error::last_os_error();
        if error.raw_os_error() == Some(nix::libc::ESRCH) {
            Ok(false)
        } else {
            Err(InnerError::Io(error).into())
        }
    }
}

async fn read_bounded<R>(mut reader: R, stream: &'static str, limit: usize) -> Result<Vec<u8>, Error>
where
    R: io::AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader.read(&mut chunk).await.map_err(InnerError::from)?;
        if count == 0 {
            return Ok(bytes);
        }
        if count > limit.saturating_sub(bytes.len()) {
            return Err(InnerError::OutputLimit { stream, limit }.into());
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

async fn read_stderr<R, F>(reader: R, limits: Limits, callback: Option<F>) -> Result<(), Error>
where
    R: io::AsyncRead + Unpin,
    F: Fn(FetchProgress),
{
    if let Some(callback) = callback {
        ProgressParser::new(reader, limits.stderr_bytes, limits.progress_segment_bytes)
            .parse(callback)
            .await
    } else {
        read_bounded(reader, "stderr", limits.stderr_bytes).await.map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RepositoryUsage {
    logical_bytes: u64,
    allocated_bytes: u64,
    entries: u64,
}

fn open_repository_directory(path: &Path) -> Result<fs::File, Error> {
    match open_directory(path) {
        Ok(directory) => Ok(directory),
        Err(source)
            if source.kind() == std_io::ErrorKind::NotADirectory || source.raw_os_error() == Some(nix::libc::ELOOP) =>
        {
            Err(InnerError::InvalidRepositoryRoot.into())
        }
        Err(source) => Err(InnerError::Io(source).into()),
    }
}

async fn verify_bare_repository(root: &fs::File, limits: Limits) -> Result<(), Error> {
    let output = run_git_in_directory(
        &[OsStr::new("repo"), OsStr::new("info"), OsStr::new("layout.bare")],
        limits,
        root,
        None,
        None::<fn(FetchProgress)>,
    )
    .await?;
    if output.stdout.starts_with(b"layout.bare=true") {
        Ok(())
    } else {
        Err(InnerError::Constraint(Constraint::NotBare).into())
    }
}

/// Admit only the direct local values needed to preserve a mirror. Includes
/// are disabled explicitly and every multi-valued field must have exactly one
/// normalized result. No URL rewriting is consulted when comparing origins.
async fn inspect_private_mirror_config(
    root: &fs::File,
    expected_origin: &Url,
    limits: Limits,
) -> Result<ObjectFormat, Error> {
    let bare = run_git_in_directory(
        &[
            OsStr::new("config"),
            OsStr::new("--local"),
            OsStr::new("--no-includes"),
            OsStr::new("--type=bool"),
            OsStr::new("--get-all"),
            OsStr::new("--"),
            OsStr::new("core.bare"),
        ],
        limits,
        root,
        None,
        None::<fn(FetchProgress)>,
    )
    .await?;
    if bare.stdout != b"true\n" {
        return Err(InnerError::InvalidMirrorConfiguration.into());
    }

    let origin = run_git_in_directory(
        &[
            OsStr::new("config"),
            OsStr::new("--local"),
            OsStr::new("--no-includes"),
            OsStr::new("--get-all"),
            OsStr::new("--"),
            OsStr::new("remote.origin.url"),
        ],
        limits,
        root,
        None,
        None::<fn(FetchProgress)>,
    )
    .await?;
    let expected_origin_line = format!("{}\n", expected_origin.as_str());
    if origin.stdout != expected_origin_line.as_bytes() {
        return Err(InnerError::MirrorOriginMismatch.into());
    }

    let object_format = run_git_in_directory(
        &[
            OsStr::new("config"),
            OsStr::new("--local"),
            OsStr::new("--no-includes"),
            OsStr::new("--default"),
            OsStr::new("sha1"),
            OsStr::new("--get"),
            OsStr::new("--"),
            OsStr::new("extensions.objectformat"),
        ],
        limits,
        root,
        None,
        None::<fn(FetchProgress)>,
    )
    .await?;
    match object_format.stdout.as_slice() {
        b"sha1\n" => Ok(ObjectFormat::Sha1),
        b"sha256\n" => Ok(ObjectFormat::Sha256),
        _ => Err(InnerError::InvalidMirrorConfiguration.into()),
    }
}

fn canonical_mirror_config(origin: &Url, object_format: ObjectFormat) -> Result<Vec<u8>, Error> {
    validate_transport_url(origin)?;
    let repository_format = match object_format {
        ObjectFormat::Sha1 => 0,
        ObjectFormat::Sha256 => 1,
    };
    let mut config =
        format!("[core]\n\trepositoryformatversion = {repository_format}\n\tfilemode = true\n\tbare = true\n")
            .into_bytes();
    if object_format == ObjectFormat::Sha256 {
        config.extend_from_slice(b"[extensions]\n\tobjectformat = sha256\n");
    }
    config.extend_from_slice(b"[remote \"origin\"]\n\turl = \"");
    for byte in origin.as_str().bytes() {
        match byte {
            b'\\' => config.extend_from_slice(b"\\\\"),
            b'\"' => config.extend_from_slice(b"\\\""),
            b'\n' => config.extend_from_slice(b"\\n"),
            b'\t' => config.extend_from_slice(b"\\t"),
            0..=31 | 127 => return Err(InnerError::InvalidRemoteUrl.into()),
            _ => config.push(byte),
        }
    }
    config.extend_from_slice(b"\"\n\tfetch = +refs/*:refs/*\n\tmirror = true\n");
    Ok(config)
}

static MIRROR_CONFIG_NONCE: AtomicU64 = AtomicU64::new(0);

/// Replace local config through the pinned repository descriptor. The old
/// config remains in place until the complete owner-only replacement has been
/// written and synced, and the root directory is synced after rename.
fn write_canonical_mirror_config(root: &fs::File, origin: &Url, object_format: ObjectFormat) -> Result<(), Error> {
    let expected = canonical_mirror_config(origin, object_format)?;
    let mut temporary = None;
    for _ in 0..128 {
        let nonce = MIRROR_CONFIG_NONCE.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!(".gitwrap-config-{}-{nonce}", std::process::id()))
            .expect("generated Git config name contains no NUL");
        match createat_private_file(root, &name) {
            Ok(file) => {
                temporary = Some((name, file));
                break;
            }
            Err(source) if source.kind() == std_io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(InnerError::Io(source).into()),
        }
    }
    let (temporary_name, mut temporary_file) = temporary.ok_or_else(|| {
        InnerError::Io(std_io::Error::new(
            std_io::ErrorKind::AlreadyExists,
            "could not reserve a private Git config replacement",
        ))
    })?;

    let result = (|| -> Result<(), Error> {
        use std::io::Write as _;

        temporary_file.write_all(&expected).map_err(InnerError::from)?;
        temporary_file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(InnerError::from)?;
        temporary_file.sync_all().map_err(InnerError::from)?;
        let current = openat_root_file(root, c"config", nix::libc::O_RDONLY)?;
        if !current.metadata().map_err(InnerError::from)?.is_file() {
            return Err(InnerError::InvalidMirrorConfiguration.into());
        }
        let renamed = unsafe {
            nix::libc::renameat(
                root.as_raw_fd(),
                temporary_name.as_ptr(),
                root.as_raw_fd(),
                c"config".as_ptr(),
            )
        };
        if renamed == -1 {
            return Err(InnerError::Io(std_io::Error::last_os_error()).into());
        }
        root.sync_all().map_err(InnerError::from)?;
        verify_canonical_mirror_config(root, origin, object_format)
    })();

    if result.is_err() {
        unsafe {
            nix::libc::unlinkat(root.as_raw_fd(), temporary_name.as_ptr(), 0);
        }
    }
    result
}

fn verify_canonical_mirror_config(root: &fs::File, origin: &Url, object_format: ObjectFormat) -> Result<(), Error> {
    use std::io::Read as _;

    let expected = canonical_mirror_config(origin, object_format)?;
    let root_mode = root.metadata().map_err(InnerError::from)?.mode() & 0o777;
    let mut config = openat_root_file(root, c"config", nix::libc::O_RDONLY)?;
    let metadata = config.metadata().map_err(InnerError::from)?;
    if root_mode != 0o700 || !metadata.is_file() || metadata.mode() & 0o777 != 0o600 {
        return Err(InnerError::InvalidMirrorConfiguration.into());
    }
    let mut found = Vec::with_capacity(expected.len());
    config
        .by_ref()
        .take(u64::try_from(expected.len()).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut found)
        .map_err(InnerError::from)?;
    if found == expected {
        Ok(())
    } else {
        Err(InnerError::InvalidMirrorConfiguration.into())
    }
}

fn secure_mirror_permissions(root: &fs::File) -> Result<(), Error> {
    root.set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(InnerError::from)?;
    let config = openat_root_file(root, c"config", nix::libc::O_RDONLY)?;
    let config_metadata = config.metadata().map_err(InnerError::from)?;
    if !config_metadata.is_file() {
        return Err(InnerError::InvalidRepositoryRoot.into());
    }
    config
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(InnerError::from)?;
    config.sync_all().map_err(InnerError::from)?;
    root.sync_all().map_err(InnerError::from)?;
    let root_mode = root.metadata().map_err(InnerError::from)?.mode() & 0o777;
    let config_mode = config.metadata().map_err(InnerError::from)?.mode() & 0o777;
    if root_mode != 0o700 || config_mode != 0o600 {
        return Err(InnerError::InvalidRepositoryRoot.into());
    }
    Ok(())
}

fn openat_root_file(root: &fs::File, name: &std::ffi::CStr, access: i32) -> Result<fs::File, Error> {
    let descriptor = unsafe {
        nix::libc::openat(
            root.as_raw_fd(),
            name.as_ptr(),
            access | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        )
    };
    if descriptor == -1 {
        return Err(InnerError::Io(std_io::Error::last_os_error()).into());
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(fs::File::from_parts(
        descriptor.into(),
        Path::new("<descriptor-rooted Git file>"),
    ))
}

fn createat_private_file(root: &fs::File, name: &std::ffi::CStr) -> std_io::Result<fs::File> {
    let descriptor = unsafe {
        nix::libc::openat(
            root.as_raw_fd(),
            name.as_ptr(),
            nix::libc::O_WRONLY | nix::libc::O_CREAT | nix::libc::O_EXCL | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0o600,
        )
    };
    if descriptor == -1 {
        return Err(std_io::Error::last_os_error());
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(fs::File::from_parts(
        descriptor.into(),
        Path::new("<descriptor-rooted private Git file>"),
    ))
}

fn verify_repository_path_identity(path: &Path, directory: &fs::File) -> Result<(), Error> {
    let path_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source)
            if source.kind() == std_io::ErrorKind::NotFound
                || source.kind() == std_io::ErrorKind::NotADirectory
                || source.raw_os_error() == Some(nix::libc::ELOOP) =>
        {
            return Err(InnerError::RepositoryRootChanged.into());
        }
        Err(source) => return Err(InnerError::Io(source).into()),
    };
    let opened_metadata = directory.metadata().map_err(InnerError::from)?;
    if !path_metadata.is_dir()
        || path_metadata.file_type().is_symlink()
        || path_metadata.dev() != opened_metadata.dev()
        || path_metadata.ino() != opened_metadata.ino()
    {
        Err(InnerError::RepositoryRootChanged.into())
    } else {
        Ok(())
    }
}

fn verify_repository_usage(path: &Path, limits: Limits) -> Result<RepositoryUsage, Error> {
    let deadline = Instant::now()
        .checked_add(limits.wall_timeout)
        .ok_or(InnerError::InvalidLimits)?;
    verify_repository_usage_before(path, limits, deadline)
}

/// Creation monitors start before Git creates their staging root. Absence is
/// accepted only at this explicit pre-spawn boundary; every mandatory scan of
/// an expected root uses strict mode and rejects disappearance.
fn verify_repository_usage_or_absent_for_creation(path: &Path, limits: Limits) -> Result<RepositoryUsage, Error> {
    match open_directory(path) {
        Ok(directory) => verify_repository_usage_directory(&directory, limits),
        Err(source) if source.kind() == std_io::ErrorKind::NotFound => Ok(RepositoryUsage::default()),
        Err(source)
            if source.kind() == std_io::ErrorKind::NotADirectory || source.raw_os_error() == Some(nix::libc::ELOOP) =>
        {
            Err(InnerError::InvalidRepositoryRoot.into())
        }
        Err(source) => Err(InnerError::Io(source).into()),
    }
}

fn verify_repository_usage_directory(directory: &fs::File, limits: Limits) -> Result<RepositoryUsage, Error> {
    let deadline = Instant::now()
        .checked_add(limits.wall_timeout)
        .ok_or(InnerError::InvalidLimits)?;
    verify_two_repository_snapshots(|| scan_repository_directory_strict(directory, limits, deadline))
}

fn verify_repository_usage_before(path: &Path, limits: Limits, deadline: Instant) -> Result<RepositoryUsage, Error> {
    verify_two_repository_snapshots(|| scan_repository_path_strict(path, limits, deadline))
}

fn scan_repository_directory_strict(
    directory: &fs::File,
    limits: Limits,
    deadline: Instant,
) -> Result<RepositorySnapshot, Error> {
    let mut scanner = RepositoryUsageScanner::from_directory(
        directory.try_clone().map_err(InnerError::from)?,
        limits,
        ScanMode::Strict,
    )?;
    while !scanner.advance(8192, Some(deadline))? {}
    Ok(scanner.snapshot)
}

fn scan_repository_path_strict(path: &Path, limits: Limits, deadline: Instant) -> Result<RepositorySnapshot, Error> {
    let mut scanner = RepositoryUsageScanner::new(path, limits, ScanMode::Strict)?;
    while !scanner.advance(8192, Some(deadline))? {}
    Ok(scanner.snapshot)
}

fn verify_two_repository_snapshots(
    mut scan: impl FnMut() -> Result<RepositorySnapshot, Error>,
) -> Result<RepositoryUsage, Error> {
    let first = scan()?;
    let second = scan()?;
    if first == second {
        Ok(second.usage)
    } else {
        Err(InnerError::RepositoryChangedDuringScan.into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScanMode {
    Live,
    Strict,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RepositorySnapshot {
    usage: RepositoryUsage,
    entries: BTreeMap<Vec<u8>, SnapshotMetadata>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SnapshotMetadata {
    device: nix::libc::dev_t,
    inode: nix::libc::ino_t,
    mode: nix::libc::mode_t,
    links: nix::libc::nlink_t,
    logical_bytes: nix::libc::off_t,
    allocated_blocks: nix::libc::blkcnt_t,
    modified_seconds: nix::libc::time_t,
    modified_nanoseconds: nix::libc::c_long,
    changed_seconds: nix::libc::time_t,
    changed_nanoseconds: nix::libc::c_long,
}

impl SnapshotMetadata {
    fn from_stat(stat: &nix::libc::stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            links: stat.st_nlink,
            logical_bytes: stat.st_size,
            allocated_blocks: stat.st_blocks,
            modified_seconds: stat.st_mtime,
            modified_nanoseconds: stat.st_mtime_nsec,
            changed_seconds: stat.st_ctime,
            changed_nanoseconds: stat.st_ctime_nsec,
        }
    }

    fn usage(self) -> RepositoryUsage {
        let logical_bytes = u64::try_from(self.logical_bytes).unwrap_or(u64::MAX);
        let blocks = u64::try_from(self.allocated_blocks).unwrap_or(u64::MAX);
        RepositoryUsage {
            logical_bytes,
            allocated_bytes: blocks.saturating_mul(512),
            entries: 1,
        }
    }

    fn is_directory(self) -> bool {
        self.mode & nix::libc::S_IFMT == nix::libc::S_IFDIR
    }

    fn same_inode(self, other: Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.mode & nix::libc::S_IFMT == other.mode & nix::libc::S_IFMT
    }
}

struct RepositoryUsageScanner {
    limits: Limits,
    mode: ScanMode,
    snapshot: RepositorySnapshot,
    snapshot_bytes: u64,
    snapshot_limit: u64,
    directory_limit: usize,
    directories: Vec<DirectoryCursor>,
}

const SNAPSHOT_ENTRY_OVERHEAD: u64 = 128;

impl RepositoryUsageScanner {
    fn new(path: &Path, limits: Limits, mode: ScanMode) -> Result<Self, Error> {
        let directory_limit = scanner_directory_limit(limits)?;
        let snapshot_limit = repository_snapshot_limit(limits);
        let root = match DirectoryCursor::open(path) {
            Ok(root) => root,
            Err(source) if source.kind() == std_io::ErrorKind::NotFound && mode == ScanMode::Live => {
                return Ok(Self {
                    limits,
                    mode,
                    snapshot: RepositorySnapshot::default(),
                    snapshot_bytes: 0,
                    snapshot_limit,
                    directory_limit,
                    directories: Vec::new(),
                });
            }
            Err(source) if source.kind() == std_io::ErrorKind::NotFound => {
                return Err(InnerError::RepositoryChangedDuringScan.into());
            }
            Err(source)
                if source.kind() == std_io::ErrorKind::NotADirectory
                    || source.raw_os_error() == Some(nix::libc::ELOOP) =>
            {
                return Err(InnerError::InvalidRepositoryRoot.into());
            }
            Err(source) => return Err(InnerError::Io(source).into()),
        };
        Self::from_root(root, limits, mode, directory_limit, snapshot_limit)
    }

    fn from_directory(directory: fs::File, limits: Limits, mode: ScanMode) -> Result<Self, Error> {
        let directory_limit = scanner_directory_limit(limits)?;
        let snapshot_limit = repository_snapshot_limit(limits);
        let root = DirectoryCursor::from_directory(directory).map_err(InnerError::from)?;
        Self::from_root(root, limits, mode, directory_limit, snapshot_limit)
    }

    fn from_root(
        root: DirectoryCursor,
        limits: Limits,
        mode: ScanMode,
        directory_limit: usize,
        snapshot_limit: u64,
    ) -> Result<Self, Error> {
        let metadata = metadata_for_descriptor(&root.directory).map_err(InnerError::from)?;
        let mut usage = metadata.usage();
        usage.entries = 0;
        enforce_repository_usage(usage, limits)?;
        let mut scanner = Self {
            limits,
            mode,
            snapshot: RepositorySnapshot {
                usage,
                entries: BTreeMap::new(),
            },
            snapshot_bytes: 0,
            snapshot_limit,
            directory_limit,
            directories: vec![root],
        };
        scanner.record_snapshot(Vec::new(), metadata)?;
        Ok(scanner)
    }

    fn record_snapshot(&mut self, relative: Vec<u8>, metadata: SnapshotMetadata) -> Result<(), Error> {
        if self.mode == ScanMode::Live {
            return Ok(());
        }

        let path_bytes = u64::try_from(relative.len()).unwrap_or(u64::MAX);
        let next = self
            .snapshot_bytes
            .saturating_add(SNAPSHOT_ENTRY_OVERHEAD)
            .saturating_add(path_bytes);
        if next > self.snapshot_limit {
            return Err(InnerError::RepositorySnapshotMemory {
                limit: self.snapshot_limit,
            }
            .into());
        }
        if self.snapshot.entries.insert(relative, metadata).is_some() {
            return Err(InnerError::RepositoryChangedDuringScan.into());
        }
        self.snapshot_bytes = next;
        Ok(())
    }

    /// Process at most `entry_budget` entries. Returning to the async select
    /// between slices prevents quota accounting from starving bounded pipe
    /// readers while retaining one descriptor-rooted traversal.
    fn advance(&mut self, entry_budget: usize, deadline: Option<Instant>) -> Result<bool, Error> {
        let mut processed = 0_usize;
        while !self.directories.is_empty() {
            enforce_scan_deadline(deadline, self.limits)?;
            if processed == entry_budget {
                return Ok(false);
            }
            let next = self
                .directories
                .last_mut()
                .expect("non-empty descriptor stack")
                .entries
                .next();
            let Some(entry) = next else {
                self.directories.pop();
                continue;
            };
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) if self.mode == ScanMode::Live && transient_directory_replacement(&source) => continue,
                Err(source) if transient_directory_replacement(&source) => {
                    return Err(InnerError::RepositoryChangedDuringScan.into());
                }
                Err(source) => return Err(InnerError::Io(source).into()),
            };
            processed += 1;
            let name = entry.file_name();
            let name = CString::new(name.as_bytes()).map_err(|_| {
                InnerError::Io(std_io::Error::new(
                    std_io::ErrorKind::InvalidData,
                    "Git repository entry contains NUL",
                ))
            })?;
            let (metadata, relative) = {
                let parent = self.directories.last().expect("current descriptor cursor");
                let Some(metadata) = scan_metadata_at(&parent.directory, &name, self.mode)? else {
                    continue;
                };
                let relative = if self.mode == ScanMode::Live {
                    Vec::new()
                } else {
                    let remaining = self
                        .snapshot_limit
                        .saturating_sub(self.snapshot_bytes)
                        .saturating_sub(SNAPSHOT_ENTRY_OVERHEAD);
                    parent.child_relative(name.as_bytes(), remaining, self.snapshot_limit)?
                };
                (metadata, relative)
            };
            self.record_snapshot(relative.clone(), metadata)?;
            let entry_usage = metadata.usage();
            self.snapshot.usage.entries = self.snapshot.usage.entries.saturating_add(1);
            self.snapshot.usage.logical_bytes = self
                .snapshot
                .usage
                .logical_bytes
                .saturating_add(entry_usage.logical_bytes);
            self.snapshot.usage.allocated_bytes = self
                .snapshot
                .usage
                .allocated_bytes
                .saturating_add(entry_usage.allocated_bytes);
            enforce_repository_usage(self.snapshot.usage, self.limits)?;
            if metadata.is_directory() {
                if self.directories.len() >= self.directory_limit {
                    return Err(InnerError::RepositoryDepth {
                        limit: self.directory_limit,
                    }
                    .into());
                }
                let opened = {
                    let parent = self.directories.last().expect("current descriptor cursor");
                    DirectoryCursor::open_child(parent, &name, relative)
                };
                match opened {
                    Ok(directory) => {
                        let opened_metadata =
                            metadata_for_descriptor(&directory.directory).map_err(InnerError::from)?;
                        if !metadata.same_inode(opened_metadata) {
                            if self.mode == ScanMode::Live {
                                continue;
                            }
                            return Err(InnerError::RepositoryChangedDuringScan.into());
                        }
                        self.directories.push(directory);
                    }
                    Err(source) if self.mode == ScanMode::Live && transient_directory_replacement(&source) => {}
                    Err(source) if transient_directory_replacement(&source) => {
                        return Err(InnerError::RepositoryChangedDuringScan.into());
                    }
                    Err(source) => return Err(InnerError::Io(source).into()),
                }
            }
        }
        enforce_scan_deadline(deadline, self.limits)?;
        Ok(true)
    }
}

fn scanner_directory_limit(limits: Limits) -> Result<usize, Error> {
    // Each cursor owns the pinned directory plus the independent descriptor
    // held by ReadDir. Leave headroom for the runtime, pipes, cache locks, and
    // unrelated work in this process instead of consuming the ambient
    // RLIMIT_NOFILE all the way to EMFILE.
    const PARENT_FD_RESERVE: u64 = 32;
    const FDS_PER_CURSOR: u64 = 2;

    let mut inherited = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { nix::libc::getrlimit(nix::libc::RLIMIT_NOFILE, &mut inherited) } == -1 {
        return Err(InnerError::Io(std_io::Error::last_os_error()).into());
    }
    // RLIMIT_NOFILE is process-wide. Basing the traversal only on its numeric
    // ceiling is unsafe when other repositories or runtime services already
    // hold descriptors: a deeply nested tree could consume the nominal budget
    // and hit EMFILE before the scanner's depth check. Count the live parent
    // descriptors as part of the reservation. `/proc/self/fd` is already the
    // Linux descriptor-root used by DirectoryCursor below, and its temporary
    // enumeration descriptor makes this count conservatively high.
    let open_descriptors = parent_open_descriptor_count()?;
    let descriptor_ceiling = limits.open_files.min(rlim_to_u64(inherited.rlim_cur));
    let cursors = scanner_cursor_capacity(descriptor_ceiling, open_descriptors, PARENT_FD_RESERVE, FDS_PER_CURSOR);
    if cursors == 0 {
        return Err(InnerError::RepositoryDepth { limit: 0 }.into());
    }
    Ok(usize::try_from(cursors).unwrap_or(usize::MAX))
}

fn repository_snapshot_limit(limits: Limits) -> u64 {
    const MAX_SNAPSHOT_BYTES: u64 = 512 * 1024 * 1024;

    (limits.address_space_bytes / 4).min(MAX_SNAPSHOT_BYTES)
}

fn scanner_cursor_capacity(ceiling: u64, open: u64, reserve: u64, descriptors_per_cursor: u64) -> u64 {
    ceiling.saturating_sub(open).saturating_sub(reserve) / descriptors_per_cursor
}

fn parent_open_descriptor_count() -> Result<u64, Error> {
    let mut count = 0_u64;
    for entry in fs::read_dir("/proc/self/fd").map_err(InnerError::from)? {
        entry.map_err(InnerError::from)?;
        count = count.saturating_add(1);
    }
    Ok(count)
}

struct DirectoryCursor {
    /// Keeps the inode pinned while `/proc/self/fd/<n>` is used to open an
    /// enumeration cursor. Descendants are opened with O_NOFOLLOW, so replacing
    /// a name with a symlink cannot redirect accounting outside this root.
    directory: fs::File,
    entries: fs::ReadDir,
    relative: Vec<u8>,
}

impl DirectoryCursor {
    fn open(path: &Path) -> std_io::Result<Self> {
        Self::from_directory(open_directory(path)?)
    }

    fn from_directory(directory: fs::File) -> std_io::Result<Self> {
        Self::from_directory_relative(directory, Vec::new())
    }

    fn open_child(parent: &Self, name: &std::ffi::CStr, relative: Vec<u8>) -> std_io::Result<Self> {
        Self::from_directory_relative(openat_directory(&parent.directory, name)?, relative)
    }

    fn from_directory_relative(directory: fs::File, relative: Vec<u8>) -> std_io::Result<Self> {
        let descriptor_path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        let entries = fs::read_dir(descriptor_path)?;
        Ok(Self {
            directory,
            entries,
            relative,
        })
    }

    fn child_relative(&self, name: &[u8], remaining: u64, snapshot_limit: u64) -> Result<Vec<u8>, Error> {
        let separator = usize::from(!self.relative.is_empty());
        let length = self
            .relative
            .len()
            .checked_add(separator)
            .and_then(|length| length.checked_add(name.len()))
            .ok_or(InnerError::RepositorySnapshotMemory { limit: snapshot_limit })?;
        if u64::try_from(length).unwrap_or(u64::MAX) > remaining {
            return Err(InnerError::RepositorySnapshotMemory { limit: snapshot_limit }.into());
        }
        let mut relative = Vec::new();
        relative
            .try_reserve_exact(length)
            .map_err(|_| InnerError::RepositorySnapshotMemory { limit: snapshot_limit })?;
        relative.extend_from_slice(&self.relative);
        if separator == 1 {
            relative.push(b'/');
        }
        relative.extend_from_slice(name);
        Ok(relative)
    }
}

fn metadata_for_descriptor(file: &fs::File) -> std_io::Result<SnapshotMetadata> {
    let mut stat = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
    let result = unsafe { nix::libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) };
    if result == -1 {
        Err(std_io::Error::last_os_error())
    } else {
        let stat = unsafe { stat.assume_init() };
        Ok(SnapshotMetadata::from_stat(&stat))
    }
}

fn metadata_at(directory: &fs::File, name: &std::ffi::CStr) -> std_io::Result<SnapshotMetadata> {
    let mut stat = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
    let result = unsafe {
        nix::libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            nix::libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == -1 {
        Err(std_io::Error::last_os_error())
    } else {
        let stat = unsafe { stat.assume_init() };
        Ok(SnapshotMetadata::from_stat(&stat))
    }
}

fn scan_metadata_at(
    directory: &fs::File,
    name: &std::ffi::CStr,
    mode: ScanMode,
) -> Result<Option<SnapshotMetadata>, Error> {
    match metadata_at(directory, name) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(source) if mode == ScanMode::Live && transient_directory_replacement(&source) => Ok(None),
        Err(source) if transient_directory_replacement(&source) => Err(InnerError::RepositoryChangedDuringScan.into()),
        Err(source) => Err(InnerError::Io(source).into()),
    }
}

fn openat_directory(directory: &fs::File, name: &std::ffi::CStr) -> std_io::Result<fs::File> {
    let descriptor = unsafe {
        nix::libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW,
        )
    };
    if descriptor == -1 {
        return Err(std_io::Error::last_os_error());
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(fs::File::from_parts(
        descriptor.into(),
        Path::new("<descriptor-rooted Git directory>"),
    ))
}

fn open_directory(path: &Path) -> std_io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(path)
}

fn transient_directory_replacement(source: &std_io::Error) -> bool {
    source.kind() == std_io::ErrorKind::NotFound
        || source.kind() == std_io::ErrorKind::NotADirectory
        || source.raw_os_error() == Some(nix::libc::ELOOP)
}

fn enforce_scan_deadline(deadline: Option<Instant>, limits: Limits) -> Result<(), Error> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(InnerError::Timeout {
            timeout: limits.wall_timeout,
        }
        .into())
    } else {
        Ok(())
    }
}

fn enforce_repository_usage(usage: RepositoryUsage, limits: Limits) -> Result<(), Error> {
    if usage.entries > limits.repository_entries {
        return Err(InnerError::RepositoryEntries {
            limit: limits.repository_entries,
        }
        .into());
    }
    let observed = usage.logical_bytes.max(usage.allocated_bytes);
    if observed > limits.repository_bytes {
        return Err(InnerError::RepositoryBytes {
            observed,
            limit: limits.repository_bytes,
        }
        .into());
    }
    Ok(())
}

fn remove_path(path: &Path) -> std_io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == std_io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Construct Git with a deliberately small, stable process environment.
///
/// Source transport may change whether a fetch succeeds, but it must not
/// activate user/system configuration, credential helpers, hooks, filters, or
/// locale-dependent checkout behavior that can change locked source bytes.
fn git_command(limits: Limits) -> process::Command {
    let path = env::var_os("PATH");
    let mut command = process::Command::new("git");
    command.env_clear();
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command
        .env("HOME", "/nonexistent")
        .env("XDG_CONFIG_HOME", "/nonexistent")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        // Ignore SSH configuration capable of launching ProxyCommand or local
        // commands. `ssh` itself is resolved from the same trusted PATH as Git;
        // unknown Git remote-helper transports are rejected before spawn.
        .env(
            "GIT_SSH_COMMAND",
            "ssh -F /dev/null -oBatchMode=yes -oPermitLocalCommand=no -oProxyCommand=none",
        )
        .env("GIT_SSH_VARIANT", "ssh")
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.symlinks=true",
            "-c",
            "credential.helper=",
            "-c",
            "credential.useHttpPath=true",
            "-c",
            "fetch.recurseSubmodules=false",
            "-c",
            "http.cookieFile=",
            "-c",
            "http.extraHeader=",
            "-c",
            "http.proxy=",
            "-c",
            "http.sslVerify=true",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
            "-c",
            "protocol.http.allow=never",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "protocol.ssh.allow=always",
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "remote.origin.proxy=",
            "-c",
            "remote.origin.uploadpack=git-upload-pack",
            "-c",
            "submodule.recurse=false",
        ]);
    constrain_process(&mut command, limits);
    command
}

fn set_command_directory(command: &mut process::Command, directory: &fs::File) {
    let descriptor = directory.as_raw_fd();
    // The descriptor itself remains close-on-exec. fchdir pins the child cwd to
    // the already-validated inode before Git starts, so a concurrent rename or
    // symlink replacement of the caller-visible path cannot redirect it.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if nix::libc::fchdir(descriptor) == -1 {
                Err(std_io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

fn constrain_process(command: &mut process::Command, limits: Limits) {
    // The process group contains Git plus transport helpers such as ssh. The
    // per-file RLIMIT_FSIZE is an OS-enforced backstop complementing the
    // monitored aggregate repository quota.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if nix::libc::setpgid(0, 0) == -1 {
                return Err(std_io::Error::last_os_error());
            }
            let core = nix::libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if nix::libc::setrlimit(nix::libc::RLIMIT_CORE, &core) == -1 {
                return Err(std_io::Error::last_os_error());
            }

            let mut inherited_nofile = nix::libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if nix::libc::getrlimit(nix::libc::RLIMIT_NOFILE, &mut inherited_nofile) == -1 {
                return Err(std_io::Error::last_os_error());
            }
            let requested_nofile = rlim_from_u64(limits.open_files);
            let nofile_max = inherited_nofile.rlim_max.min(requested_nofile);
            let nofile = nix::libc::rlimit {
                rlim_cur: inherited_nofile.rlim_cur.min(nofile_max),
                rlim_max: nofile_max,
            };
            if nix::libc::setrlimit(nix::libc::RLIMIT_NOFILE, &nofile) == -1 {
                return Err(std_io::Error::last_os_error());
            }

            let mut inherited_address_space = nix::libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if nix::libc::getrlimit(nix::libc::RLIMIT_AS, &mut inherited_address_space) == -1 {
                return Err(std_io::Error::last_os_error());
            }
            let address_space_max = inherited_address_space
                .rlim_max
                .min(rlim_from_u64(limits.address_space_bytes));
            let address_space = nix::libc::rlimit {
                rlim_cur: inherited_address_space.rlim_cur.min(address_space_max),
                rlim_max: address_space_max,
            };
            if nix::libc::setrlimit(nix::libc::RLIMIT_AS, &address_space) == -1 {
                return Err(std_io::Error::last_os_error());
            }

            let mut current = nix::libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if nix::libc::getrlimit(nix::libc::RLIMIT_FSIZE, &mut current) == -1 {
                return Err(std_io::Error::last_os_error());
            }
            let requested = rlim_from_u64(limits.repository_bytes);
            current.rlim_cur = current.rlim_cur.min(requested);
            current.rlim_max = current.rlim_max.min(requested);
            if nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &current) == -1 {
                return Err(std_io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(target_pointer_width = "64")]
fn rlim_from_u64(value: u64) -> nix::libc::rlim_t {
    value
}

#[cfg(not(target_pointer_width = "64"))]
fn rlim_from_u64(value: u64) -> nix::libc::rlim_t {
    nix::libc::rlim_t::try_from(value).unwrap_or(nix::libc::rlim_t::MAX)
}

#[cfg(target_pointer_width = "64")]
fn rlim_to_u64(value: nix::libc::rlim_t) -> u64 {
    value
}

#[cfg(not(target_pointer_width = "64"))]
fn rlim_to_u64(value: nix::libc::rlim_t) -> u64 {
    u64::from(value)
}

struct ProgressParser<R: io::AsyncRead> {
    reader: R,
    total_limit: usize,
    segment_limit: usize,
}

impl<R: io::AsyncRead + Unpin> ProgressParser<R> {
    const PREFIX: &[u8] = b"Receiving objects:";

    pub fn new(stderr: R, total_limit: usize, segment_limit: usize) -> Self {
        Self {
            reader: stderr,
            total_limit,
            segment_limit,
        }
    }

    // We're parsing lines like:
    // "Receiving objects:  26% (163045/627093), 52.57 MiB | 34.99 MiB/s"
    // And we want the percentage and the speed, which are conveniently
    // the first and the last tokens of the line.

    pub async fn parse(mut self, callback: impl Fn(FetchProgress)) -> Result<(), Error> {
        let mut total = 0_usize;
        let mut segment = Vec::with_capacity(self.segment_limit.min(1024));
        let mut chunk = [0_u8; 8192];
        loop {
            let count = self.reader.read(&mut chunk).await.map_err(InnerError::from)?;
            if count == 0 {
                Self::report_segment(&segment, &callback);
                return Ok(());
            }
            if count > self.total_limit.saturating_sub(total) {
                return Err(InnerError::OutputLimit {
                    stream: "stderr",
                    limit: self.total_limit,
                }
                .into());
            }
            total += count;
            for byte in &chunk[..count] {
                if matches!(*byte, b'\r' | b'\n') {
                    Self::report_segment(&segment, &callback);
                    segment.clear();
                } else if segment.len() == self.segment_limit {
                    return Err(InnerError::ProgressSegmentLimit {
                        limit: self.segment_limit,
                    }
                    .into());
                } else {
                    segment.push(*byte);
                }
            }
        }
    }

    fn report_segment(segment: &[u8], callback: &impl Fn(FetchProgress)) {
        if !segment.starts_with(Self::PREFIX) {
            return;
        }
        let line = str::from_utf8(&segment[Self::PREFIX.len()..]).unwrap_or("");
        if let Some(progress) = Self::parse_progress(line) {
            callback(progress);
        }
    }

    fn parse_progress(line: &str) -> Option<FetchProgress> {
        let mut tokens = line.split_ascii_whitespace();

        let percent = tokens.next()?;
        let unit_per_sec = tokens.next_back()?;
        let speed = tokens.next_back()?;

        if !unit_per_sec.ends_with("/s") {
            return None;
        }

        Some(FetchProgress {
            percent: percent.strip_suffix('%')?.parse().ok()?,
            speed: format!("{speed} {unit_per_sec}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        path::Path,
        process::{Command, Stdio},
        time::Duration,
    };

    use tokio::io::AsyncWriteExt;

    use super::*;

    fn test_limits() -> Limits {
        Limits {
            wall_timeout: Duration::from_secs(2),
            termination_timeout: Duration::from_secs(2),
            stdout_bytes: 1024 * 1024,
            stderr_bytes: 1024 * 1024,
            progress_segment_bytes: 4096,
            repository_bytes: 64 * 1024 * 1024,
            repository_entries: 100_000,
            open_files: 256,
            address_space_bytes: 512 * 1024 * 1024,
            quota_poll_interval: Duration::from_millis(5),
        }
    }

    fn contained_test_command(script: &str, limits: Limits) -> process::Command {
        let mut command = process::Command::new("/bin/sh");
        command.arg("-c").arg(script).env_clear();
        constrain_process(&mut command, limits);
        command
    }

    fn process_exists(pid: i32) -> bool {
        let result = unsafe { nix::libc::kill(pid, 0) };
        result == 0 || std_io::Error::last_os_error().raw_os_error() != Some(nix::libc::ESRCH)
    }

    #[tokio::test]
    async fn stdout_limit_accepts_exact_n_and_rejects_n_plus_one() {
        let (mut exact_writer, exact_reader) = io::duplex(16);
        exact_writer.write_all(b"1234").await.unwrap();
        drop(exact_writer);
        assert_eq!(read_bounded(exact_reader, "stdout", 4).await.unwrap(), b"1234");

        let (mut oversized_writer, oversized_reader) = io::duplex(16);
        oversized_writer.write_all(b"12345").await.unwrap();
        drop(oversized_writer);
        let error = read_bounded(oversized_reader, "stdout", 4).await.unwrap_err();
        assert!(error.limit_exceeded());
        assert!(error.to_string().contains("4-byte output limit"));
    }

    #[tokio::test]
    async fn progress_segment_limit_accepts_exact_n_and_rejects_n_plus_one() {
        let (mut exact_writer, exact_reader) = io::duplex(16);
        exact_writer.write_all(b"1234\r").await.unwrap();
        drop(exact_writer);
        ProgressParser::new(exact_reader, 5, 4).parse(|_| {}).await.unwrap();

        let (mut oversized_writer, oversized_reader) = io::duplex(16);
        oversized_writer.write_all(b"12345\r").await.unwrap();
        drop(oversized_writer);
        let error = ProgressParser::new(oversized_reader, 6, 4)
            .parse(|_| {})
            .await
            .unwrap_err();
        assert!(error.limit_exceeded());
        assert!(error.to_string().contains("progress record"));
    }

    #[tokio::test]
    async fn public_progress_backpressure_never_blocks_stderr_supervision() {
        let (progress, mut receiver) = mpsc::channel(1);
        progress
            .try_send(FetchProgress {
                percent: 0,
                speed: "queued".to_owned(),
            })
            .unwrap();
        let (mut writer, reader) = io::duplex(256);
        writer
            .write_all(
                b"Receiving objects:  25% (1/4), 1.00 MiB | 1.00 MiB/s\rReceiving objects:  50% (2/4), 2.00 MiB | 2.00 MiB/s\r",
            )
            .await
            .unwrap();
        drop(writer);

        timeout(
            Duration::from_millis(100),
            ProgressParser::new(reader, 1024, 256).parse(progress_callback(progress)),
        )
        .await
        .expect("a full progress channel must never stall parsing")
        .unwrap();
        assert_eq!(receiver.recv().await.unwrap().speed, "queued");
        assert!(receiver.try_recv().is_err(), "backpressured updates are dropped");
    }

    #[test]
    fn repository_limits_accept_exact_n_and_reject_n_plus_one() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("one"), vec![0_u8; 4096]).unwrap();

        let generous = test_limits();
        let usage = verify_repository_usage(&root, generous).unwrap();
        let exact_bytes = usage.logical_bytes.max(usage.allocated_bytes);
        let exact = Limits {
            repository_bytes: exact_bytes,
            repository_entries: 1,
            ..generous
        };
        assert_eq!(verify_repository_usage(&root, exact).unwrap(), usage);

        fs::write(root.join("two"), b"x").unwrap();
        let error = verify_repository_usage(&root, exact).unwrap_err();
        assert!(error.limit_exceeded());
        assert!(error.to_string().contains("filesystem entries"));

        fs::remove_file(root.join("two")).unwrap();
        fs::write(root.join("one"), vec![0_u8; 4097]).unwrap();
        let error = verify_repository_usage(&root, exact).unwrap_err();
        assert!(error.limit_exceeded());
        assert!(error.to_string().contains("bytes"));
    }

    #[test]
    fn strict_entry_quota_rejects_n_plus_one_without_sampling_slack() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("one"), b"").unwrap();
        let mut limits = test_limits();
        limits.repository_entries = 1;
        let one = verify_repository_usage(temporary.path(), limits).unwrap();
        assert_eq!(one.entries, 1);

        fs::write(temporary.path().join("two"), b"").unwrap();
        let error = verify_repository_usage(temporary.path(), limits).unwrap_err();
        assert!(error.limit_exceeded());
        assert!(error.to_string().contains("filesystem entries"));
    }

    #[test]
    fn live_scan_may_retry_a_vanished_name_but_strict_scan_fails_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = open_directory(temporary.path()).unwrap();
        let vanished = CString::new("vanished").unwrap();

        assert!(scan_metadata_at(&root, &vanished, ScanMode::Live).unwrap().is_none());
        let error = scan_metadata_at(&root, &vanished, ScanMode::Strict).unwrap_err();
        assert!(error.limit_exceeded());
        assert!(error.to_string().contains("changed during strict quota"));
    }

    #[test]
    fn live_scan_allows_initial_absence_without_building_a_strict_inventory() {
        let temporary = tempfile::tempdir().unwrap();
        let missing = temporary.path().join("not-created-yet");
        let mut limits = test_limits();
        limits.address_space_bytes = 1;

        let absent = RepositoryUsageScanner::new(&missing, limits, ScanMode::Live).unwrap();
        assert!(absent.directories.is_empty());
        let error = match RepositoryUsageScanner::new(&missing, limits, ScanMode::Strict) {
            Err(error) => error,
            Ok(_) => panic!("strict scan accepted a missing mandatory root"),
        };
        assert!(error.limit_exceeded());

        let root = temporary.path().join("created");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("entry"), b"contents").unwrap();
        let mut scanner = RepositoryUsageScanner::new(&root, limits, ScanMode::Live).unwrap();
        while !scanner.advance(1, None).unwrap() {}
        assert_eq!(scanner.snapshot.usage.entries, 1);
        assert!(scanner.snapshot.entries.is_empty());
        assert_eq!(scanner.snapshot_bytes, 0);
    }

    #[test]
    fn strict_relative_path_allocation_is_prechecked_against_snapshot_budget() {
        let temporary = tempfile::tempdir().unwrap();
        let cursor = DirectoryCursor::open(temporary.path()).unwrap();
        let error = cursor.child_relative(b"four", 3, 100).unwrap_err();
        assert!(error.limit_exceeded());
        assert!(error.to_string().contains("100-byte memory budget"));
    }

    #[test]
    fn strict_two_snapshot_verification_rejects_same_name_inode_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        fs::create_dir(&root).unwrap();
        let entry = root.join("entry");
        let old_entry = temporary.path().join("old-entry");
        fs::write(&entry, b"same-size").unwrap();
        let limits = test_limits();
        let deadline = Instant::now() + limits.wall_timeout;
        let mut pass = 0_u8;

        let error = verify_two_repository_snapshots(|| {
            if pass == 1 {
                fs::rename(&entry, &old_entry).unwrap();
                fs::write(&entry, b"same-size").unwrap();
            }
            pass += 1;
            scan_repository_path_strict(&root, limits, deadline)
        })
        .unwrap_err();
        assert!(error.limit_exceeded());
        assert!(error.to_string().contains("changed during strict quota"));
    }

    #[test]
    fn descriptor_rooted_quota_scan_never_follows_nested_or_root_symlinks() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("large"), vec![0_u8; 1024 * 1024]).unwrap();
        symlink(&outside, root.join("link")).unwrap();

        let usage = verify_repository_usage(&root, test_limits()).unwrap();
        assert!(usage.logical_bytes < 1024 * 1024);
        let root_link = temporary.path().join("repository-link");
        symlink(&root, &root_link).unwrap();
        let error = verify_repository_usage(&root_link, test_limits()).unwrap_err();
        assert!(error.to_string().contains("not an ordinary directory"));
    }

    #[test]
    fn quota_scan_rejects_nesting_before_exhausting_parent_descriptors() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        let mut limits = test_limits();
        limits.open_files = 34;

        let error = verify_repository_usage(&root, limits).unwrap_err();
        assert!(error.limit_exceeded());
        assert!(error.to_string().contains("descriptor budget"));
    }

    #[test]
    fn quota_scan_rejects_a_budget_too_small_for_one_cursor() {
        let temporary = tempfile::tempdir().unwrap();
        let mut limits = test_limits();
        limits.open_files = 33;

        let error = verify_repository_usage(temporary.path(), limits).unwrap_err();
        assert!(error.limit_exceeded());
        assert!(error.to_string().contains("0-directory descriptor budget"));
    }

    #[test]
    fn quota_scanner_reserves_descriptors_already_open_in_the_parent() {
        assert_eq!(scanner_cursor_capacity(256, 100, 32, 2), 62);
        assert_eq!(scanner_cursor_capacity(132, 100, 32, 2), 0);
        assert_eq!(scanner_cursor_capacity(131, 100, 32, 2), 0);
    }

    #[tokio::test]
    async fn repository_rejects_a_replaced_public_path_while_root_is_pinned() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        fixture_git(&source, &["init", "--initial-branch=main"]);
        fixture_git(&source, &["config", "user.name", "Gitwrap Test"]);
        fixture_git(&source, &["config", "user.email", "gitwrap@example.invalid"]);
        fs::write(source.join("source.txt"), b"source\n").unwrap();
        fixture_git(&source, &["add", "source.txt"]);
        fixture_git(&source, &["commit", "-m", "source"]);

        let destination = temporary.path().join("mirror.git");
        let mut limits = test_limits();
        limits.repository_bytes = 1024 * 1024;
        let source_url = Url::from_directory_path(&source).unwrap();
        let repository = Repository::clone_mirror_with_limits(&destination, &source_url, limits)
            .await
            .unwrap();

        let moved = temporary.path().join("moved-mirror.git");
        fs::rename(&destination, &moved).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("replacement"), vec![0_u8; 2 * 1024 * 1024]).unwrap();
        assert!(verify_repository_usage(&destination, limits).is_err());

        let error = repository.get_remote_url("origin").await.unwrap_err();
        assert!(error.to_string().contains("no longer names"));
        assert_eq!(
            fixture_git(&moved, &["remote", "get-url", "origin"]),
            source_url.as_str(),
        );
    }

    #[test]
    fn quota_scan_uses_the_subprocess_absolute_deadline() {
        let temporary = tempfile::tempdir().unwrap();
        let error = verify_repository_usage_before(temporary.path(), test_limits(), Instant::now()).unwrap_err();
        assert!(error.timed_out());
    }

    #[tokio::test]
    async fn oversized_cached_mirror_is_rejected_before_git_is_started() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("cached.git");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("untrusted"), vec![0_u8; 8192]).unwrap();
        let usage = verify_repository_usage(&root, test_limits()).unwrap();
        let mut limits = test_limits();
        limits.repository_bytes = usage.logical_bytes.max(usage.allocated_bytes) - 1;

        let error = Repository::open_bare_with_limits(&root, limits).await.unwrap_err();
        assert!(error.limit_exceeded());
        assert!(!error.run_failed(), "Git should not inspect an oversized cache");
        assert!(root.join("untrusted").is_file());
    }

    #[tokio::test]
    async fn remote_url_mutation_is_rejected_when_it_crosses_repository_quota() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        fs::create_dir(&root).unwrap();
        fixture_git(&root, &["init", "--initial-branch=main"]);
        fixture_git(&root, &["remote", "add", "origin", "https://example.invalid/a"]);
        let mut limits = test_limits();
        let usage = verify_repository_usage(&root, limits).unwrap();
        limits.repository_bytes = usage.logical_bytes.max(usage.allocated_bytes);
        let repository = Repository {
            path: root.clone(),
            limits,
            identity: Some(RepositoryIdentity::from_directory(&open_repository_directory(&root).unwrap()).unwrap()),
            mirror: None,
        };
        let large_url = format!("https://example.invalid/{}", "x".repeat(8192));

        let error = repository.set_remote_url("origin", &large_url).await.unwrap_err();
        assert!(error.limit_exceeded(), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn failed_public_fetch_never_deletes_a_caller_owned_repository() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("caller-owned");
        fs::create_dir(&root).unwrap();
        fixture_git(&root, &["init", "--initial-branch=main"]);
        let missing_remote = Url::from_file_path(temporary.path().join("missing-remote"))
            .unwrap()
            .to_string();
        fixture_git(&root, &["remote", "add", "origin", &missing_remote]);
        let repository = Repository {
            path: root.clone(),
            limits: test_limits(),
            identity: Some(RepositoryIdentity::from_directory(&open_repository_directory(&root).unwrap()).unwrap()),
            mirror: None,
        };

        let (progress, _receiver) = mpsc::channel(1);
        let error = repository.fetch_progress(progress).await.unwrap_err();
        assert!(error.run_failed());
        assert!(root.join(".git").is_dir());
    }

    #[tokio::test]
    async fn timeout_kills_and_reaps_the_complete_process_group() {
        let temporary = tempfile::tempdir().unwrap();
        let pid_file = temporary.path().join("descendant.pid");
        let mut limits = test_limits();
        limits.wall_timeout = Duration::from_millis(250);
        let mut command = contained_test_command("sleep 30 & echo $! > \"$PID_FILE\"; wait", limits);
        command.env("PID_FILE", &pid_file);

        let error = run_command(command, limits, None, None::<fn(FetchProgress)>)
            .await
            .unwrap_err();
        assert!(error.timed_out(), "unexpected error: {error}");
        let descendant: i32 = fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        assert!(!process_exists(descendant), "descendant {descendant} survived timeout");
    }

    #[tokio::test]
    async fn successful_parent_cannot_leave_a_background_pipe_holder() {
        let temporary = tempfile::tempdir().unwrap();
        let pid_file = temporary.path().join("descendant.pid");
        let mut limits = test_limits();
        limits.wall_timeout = Duration::from_secs(5);
        let mut command = contained_test_command("sleep 30 & echo $! > \"$PID_FILE\"", limits);
        command.env("PID_FILE", &pid_file);

        let started = Instant::now();
        let output = run_command(command, limits, None, None::<fn(FetchProgress)>)
            .await
            .unwrap();
        assert!(output.status.success());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "background pipe holder consumed the outer wall deadline"
        );
        let descendant: i32 = fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        assert!(
            !process_exists(descendant),
            "descendant {descendant} survived successful parent exit"
        );
    }

    #[tokio::test]
    async fn output_limit_kills_descendants_and_never_exposes_stderr_secrets() {
        let temporary = tempfile::tempdir().unwrap();
        let pid_file = temporary.path().join("descendant.pid");
        let mut limits = test_limits();
        limits.stdout_bytes = 4;
        let mut command = contained_test_command("sleep 30 & echo $! > \"$PID_FILE\"; printf 12345; wait", limits);
        command.env("PID_FILE", &pid_file);
        let error = run_command(command, limits, None, None::<fn(FetchProgress)>)
            .await
            .unwrap_err();
        assert!(error.limit_exceeded(), "unexpected error: {error}");
        let descendant: i32 = fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        assert!(
            !process_exists(descendant),
            "descendant {descendant} survived output rejection"
        );

        let secret = "https://alice:secret@example.invalid/repository.git";
        let command = contained_test_command(&format!("echo '{secret}' >&2; exit 7"), test_limits());
        let error = run_command(command, test_limits(), None, None::<fn(FetchProgress)>)
            .await
            .unwrap_err();
        assert!(error.run_failed());
        assert!(!error.to_string().contains("alice"));
        assert!(!error.to_string().contains("secret"));
    }

    #[tokio::test]
    async fn child_boundary_disables_core_dumps_and_caps_open_files() {
        let mut limits = test_limits();
        limits.open_files = 64;
        let mut inherited = nix::libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { nix::libc::getrlimit(nix::libc::RLIMIT_NOFILE, &mut inherited) },
            0
        );
        let expected_open_files = inherited.rlim_cur.min(64);
        let mut inherited_address_space = nix::libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { nix::libc::getrlimit(nix::libc::RLIMIT_AS, &mut inherited_address_space) },
            0
        );
        let expected_address_space = inherited_address_space
            .rlim_cur
            .min(rlim_from_u64(limits.address_space_bytes))
            / 1024;
        let command = contained_test_command(
            "printf '%s %s %s' \"$(ulimit -c)\" \"$(ulimit -n)\" \"$(ulimit -v)\"",
            limits,
        );

        let output = run_command(command, limits, None, None::<fn(FetchProgress)>)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("0 {expected_open_files} {expected_address_space}")
        );
    }

    #[tokio::test]
    async fn incremental_quota_scan_does_not_starve_a_full_stdout_pipe() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        fs::create_dir(&repository).unwrap();
        for index in 0..2048 {
            fs::write(repository.join(format!("entry-{index}")), b"").unwrap();
        }
        let mut limits = test_limits();
        limits.stdout_bytes = 512 * 1024;
        limits.quota_poll_interval = Duration::from_millis(1);
        let command = contained_test_command("dd if=/dev/zero bs=65536 count=4 2>/dev/null; sleep 0.05", limits);

        let output = run_command(
            command,
            limits,
            Some(MonitoredRepository::Path(repository)),
            None::<fn(FetchProgress)>,
        )
        .await
        .unwrap();
        assert_eq!(output.stdout.len(), 4 * 65536);
    }

    #[tokio::test]
    async fn oversized_clone_is_rejected_without_final_or_staging_state() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        fixture_git(&source, &["init", "--initial-branch=main"]);
        fixture_git(&source, &["config", "user.name", "Gitwrap Test"]);
        fixture_git(&source, &["config", "user.email", "gitwrap@example.invalid"]);
        fs::write(source.join("large"), vec![0_u8; 32 * 1024]).unwrap();
        fixture_git(&source, &["add", "large"]);
        fixture_git(&source, &["commit", "-m", "large"]);

        let destination = temporary.path().join("mirror.git");
        let mut limits = test_limits();
        limits.repository_bytes = 4096;
        let error =
            Repository::clone_mirror_with_limits(&destination, &Url::from_directory_path(&source).unwrap(), limits)
                .await
                .unwrap_err();
        assert!(
            error.limit_exceeded() || error.run_failed(),
            "unexpected error: {error}"
        );
        assert!(!destination.exists());
        assert!(fs::read_dir(temporary.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".gitwrap-mirror-")));
    }

    #[tokio::test]
    async fn published_mirror_and_credential_config_are_owner_private() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        fixture_git(&source, &["init", "--initial-branch=main"]);
        fixture_git(&source, &["config", "user.name", "Gitwrap Test"]);
        fixture_git(&source, &["config", "user.email", "gitwrap@example.invalid"]);
        fs::write(source.join("source.txt"), b"source\n").unwrap();
        fixture_git(&source, &["add", "source.txt"]);
        fixture_git(&source, &["commit", "-m", "source"]);

        let destination = temporary.path().join("mirror.git");
        Repository::clone_mirror_with_limits(&destination, &Url::from_directory_path(&source).unwrap(), test_limits())
            .await
            .unwrap();

        assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(destination.join("config")).unwrap().mode() & 0o777, 0o600);
    }

    #[tokio::test]
    async fn private_mirror_strips_hostile_local_config_before_open_and_fetch() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        fixture_git(&source, &["init", "--initial-branch=main"]);
        fixture_git(&source, &["config", "user.name", "Gitwrap Test"]);
        fixture_git(&source, &["config", "user.email", "gitwrap@example.invalid"]);
        fs::write(source.join("source.txt"), b"source\n").unwrap();
        fixture_git(&source, &["add", "source.txt"]);
        fixture_git(&source, &["commit", "-m", "source"]);

        let origin = Url::from_directory_path(&source).unwrap();
        let destination = temporary.path().join("mirror.git");
        Repository::clone_mirror_with_limits(&destination, &origin, test_limits())
            .await
            .unwrap();
        let canonical = canonical_mirror_config(&origin, ObjectFormat::Sha1).unwrap();
        let included = temporary.path().join("included-config");
        let sentinel = temporary.path().join("credential-helper-ran");
        fs::write(
            &included,
            format!(
                "[credential]\n\thelper = !touch {}\n[url \"custom-helper://attacker/\"]\n\tinsteadOf = {}\n",
                sentinel.display(),
                origin.as_str()
            ),
        )
        .unwrap();
        let mut hostile = canonical.clone();
        hostile.extend_from_slice(
            format!(
                "[include]\n\tpath = {}\n[credential]\n\thelper = !touch {}\n[core]\n\tsshCommand = touch {}\n",
                included.display(),
                sentinel.display(),
                sentinel.display()
            )
            .as_bytes(),
        );
        fs::write(destination.join("config"), &hostile).unwrap();
        fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(destination.join("config"), std::fs::Permissions::from_mode(0o644)).unwrap();

        let repository = Repository::open_private_mirror_with_limits(&destination, &origin, test_limits())
            .await
            .unwrap();
        assert_eq!(fs::read(destination.join("config")).unwrap(), canonical);
        assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(destination.join("config")).unwrap().mode() & 0o777, 0o600);
        assert!(!sentinel.exists());

        fs::write(destination.join("config"), &hostile).unwrap();
        let (progress, _receiver) = mpsc::channel(1);
        repository.fetch_progress(progress).await.unwrap();
        assert_eq!(fs::read(destination.join("config")).unwrap(), canonical);
        assert!(!sentinel.exists());
    }

    #[tokio::test]
    async fn private_mirror_origin_is_checked_before_config_is_rewritten() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let wrong_source = temporary.path().join("wrong-source");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&wrong_source).unwrap();
        fixture_git(&source, &["init", "--bare"]);
        fixture_git(&wrong_source, &["init", "--bare"]);
        let origin = Url::from_directory_path(&source).unwrap();
        let wrong_origin = Url::from_directory_path(&wrong_source).unwrap();
        let destination = temporary.path().join("mirror.git");
        Repository::clone_mirror_with_limits(&destination, &origin, test_limits())
            .await
            .unwrap();
        fixture_git(&destination, &["remote", "set-url", "origin", wrong_origin.as_str()]);

        let error = Repository::open_private_mirror_with_limits(&destination, &origin, test_limits())
            .await
            .unwrap_err();
        assert!(error.mirror_origin_mismatch());
        assert_eq!(
            fixture_git(&destination, &["remote", "get-url", "origin"]),
            wrong_origin.as_str()
        );
    }

    #[tokio::test]
    async fn unknown_remote_helper_schemes_and_option_like_arguments_are_rejected_before_spawn() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("mirror.git");
        let error = Repository::clone_mirror_with_limits(
            &destination,
            &Url::parse("custom-helper://example.invalid/repository").unwrap(),
            test_limits(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("not allowed"));
        assert!(!destination.exists());

        let error = Repository::clone_mirror_with_limits(
            &destination,
            &Url::parse("http://example.invalid/repository").unwrap(),
            test_limits(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("not allowed"));
        assert!(!destination.exists());

        let error = Repository::clone_mirror_with_limits(
            &destination,
            &Url::parse("git://example.invalid/repository").unwrap(),
            test_limits(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("not allowed"));
        assert!(!destination.exists());

        let repository = null_repository();
        assert!(repository.has_commit("--batch").await.is_err());
        assert!(repository.get_remote_url("--all").await.is_err());
        assert!(repository.set_remote_url("--add", "value").await.is_err());
        assert!(repository.checkout("--orphan").await.is_err());
        assert!(repository.contains_gitlinks("--long").await.is_err());
    }

    #[tokio::test]
    async fn sha256_object_format_commit_ids_are_accepted() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        fixture_git(&source, &["init", "--object-format=sha256", "--initial-branch=main"]);
        fixture_git(&source, &["config", "user.name", "Gitwrap Test"]);
        fixture_git(&source, &["config", "user.email", "gitwrap@example.invalid"]);
        fs::write(source.join("source.txt"), b"source\n").unwrap();
        fixture_git(&source, &["add", "source.txt"]);
        fixture_git(&source, &["commit", "-m", "source"]);

        let destination = temporary.path().join("mirror.git");
        let repository = Repository::clone_mirror_with_limits(
            &destination,
            &Url::from_directory_path(&source).unwrap(),
            test_limits(),
        )
        .await
        .unwrap();
        let commit = repository.peel_commit("HEAD").await.unwrap();
        assert_eq!(commit.len(), 64);
        assert!(commit.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
    }

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

    fn fixture_git_with_input(repository: &Path, arguments: &[&str], input: &[u8]) -> String {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.as_mut().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[tokio::test]
    async fn clone_to_skips_an_uncheckoutable_default_head() {
        let temporary = tempfile::tempdir().unwrap();
        let repository_path = temporary.path().join("repository");
        fs::create_dir(&repository_path).unwrap();
        fixture_git(&repository_path, &["init", "--initial-branch=main"]);
        fixture_git(&repository_path, &["config", "user.name", "Gitwrap Test"]);
        fixture_git(&repository_path, &["config", "user.email", "gitwrap@example.invalid"]);

        fs::write(repository_path.join("source.txt"), b"locked source\n").unwrap();
        fixture_git(&repository_path, &["add", "source.txt"]);
        fixture_git(&repository_path, &["commit", "-m", "locked source"]);
        let locked_commit = fixture_git(&repository_path, &["rev-parse", "HEAD"]);

        fs::write(repository_path.join("invalid-head"), b"invalid default head\n").unwrap();
        let invalid_blob = fixture_git(&repository_path, &["hash-object", "-w", "invalid-head"]);
        let invalid_tree_entry = format!("100644 blob {invalid_blob}\t.git\n");
        let invalid_tree = fixture_git_with_input(&repository_path, &["mktree"], invalid_tree_entry.as_bytes());
        let invalid_head = fixture_git(
            &repository_path,
            &[
                "commit-tree",
                &invalid_tree,
                "-p",
                &locked_commit,
                "-m",
                "uncheckoutable default head",
            ],
        );
        fixture_git(&repository_path, &["update-ref", "refs/heads/main", &invalid_head]);

        let ordinary_clone = temporary.path().join("ordinary-clone");
        let ordinary_result = run_git(
            [
                OsStr::new("clone"),
                OsStr::new("--no-hardlinks"),
                OsStr::new("--no-recurse-submodules"),
                repository_path.as_os_str(),
                ordinary_clone.as_os_str(),
            ],
            Limits::DEFAULT,
        )
        .await;
        assert!(
            ordinary_result.is_err(),
            "the fixture's default HEAD must be uncheckoutable"
        );

        let repository = Repository {
            path: repository_path.clone(),
            limits: Limits::DEFAULT,
            identity: Some(
                RepositoryIdentity::from_directory(&open_repository_directory(&repository_path).unwrap()).unwrap(),
            ),
            mirror: None,
        };
        let clone_path = temporary.path().join("locked-clone");
        let cloned = repository.clone_to(&clone_path).await.unwrap();
        assert_eq!(
            fixture_git(cloned.path(), &["rev-parse", "HEAD"]),
            invalid_head,
            "the clone must retain the unrelated default HEAD"
        );

        cloned.checkout(&locked_commit).await.unwrap();
        assert_eq!(fs::read(clone_path.join("source.txt")).unwrap(), b"locked source\n");
        assert_eq!(fixture_git(cloned.path(), &["rev-parse", "HEAD"]), locked_commit);
    }

    #[tokio::test]
    async fn gitlinks_are_detected_without_materializing_submodules() {
        let temporary = tempfile::tempdir().unwrap();
        let repository_path = temporary.path().join("repository");
        fs::create_dir(&repository_path).unwrap();
        fixture_git(&repository_path, &["init", "--initial-branch=main"]);
        fixture_git(&repository_path, &["config", "user.name", "Gitwrap Test"]);
        fixture_git(&repository_path, &["config", "user.email", "gitwrap@example.invalid"]);
        fs::write(repository_path.join("source.txt"), b"locked source\n").unwrap();
        fixture_git(&repository_path, &["add", "source.txt"]);
        fixture_git(&repository_path, &["commit", "-m", "source"]);

        let repository = Repository {
            path: repository_path.clone(),
            limits: Limits::DEFAULT,
            identity: Some(
                RepositoryIdentity::from_directory(&open_repository_directory(&repository_path).unwrap()).unwrap(),
            ),
            mirror: None,
        };
        let source_commit = fixture_git(&repository_path, &["rev-parse", "HEAD"]);
        assert!(!repository.contains_gitlinks(&source_commit).await.unwrap());

        let cache_info = format!("160000,{source_commit},vendor/dependency");
        fixture_git(&repository_path, &["update-index", "--add", "--cacheinfo", &cache_info]);
        fixture_git(&repository_path, &["commit", "-m", "gitlink"]);
        let gitlink_commit = fixture_git(&repository_path, &["rev-parse", "HEAD"]);

        assert!(repository.contains_gitlinks(&gitlink_commit).await.unwrap());
    }

    #[tokio::test]
    async fn annotated_tags_are_peeled_to_the_commit_object() {
        let temporary = tempfile::tempdir().unwrap();
        let repository_path = temporary.path().join("repository");
        fs::create_dir(&repository_path).unwrap();
        fixture_git(&repository_path, &["init", "--initial-branch=main"]);
        fixture_git(&repository_path, &["config", "user.name", "Gitwrap Test"]);
        fixture_git(&repository_path, &["config", "user.email", "gitwrap@example.invalid"]);
        fs::write(repository_path.join("source.txt"), b"locked source\n").unwrap();
        fixture_git(&repository_path, &["add", "source.txt"]);
        fixture_git(&repository_path, &["commit", "-m", "source"]);
        fixture_git(
            &repository_path,
            &["tag", "--annotate", "v1", "--message", "release v1"],
        );

        let commit = fixture_git(&repository_path, &["rev-parse", "HEAD"]);
        let tag_object = fixture_git(&repository_path, &["rev-parse", "v1"]);
        assert_ne!(tag_object, commit, "the fixture must use an annotated tag object");

        let repository = Repository {
            path: repository_path.clone(),
            limits: Limits::DEFAULT,
            identity: Some(
                RepositoryIdentity::from_directory(&open_repository_directory(&repository_path).unwrap()).unwrap(),
            ),
            mirror: None,
        };
        let peeled = repository.peel_commit("v1").await.unwrap();

        assert_eq!(peeled, commit);
        assert_eq!(peeled.len(), 40);
        assert!(peeled.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
    }
}
