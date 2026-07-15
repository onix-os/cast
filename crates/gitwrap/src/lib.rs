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
    env,
    ffi::{CString, OsStr},
    io as std_io,
    os::{
        fd::AsRawFd as _,
        unix::{ffi::OsStrExt, fs::MetadataExt, process::CommandExt},
    },
    path::{self, Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use fs_err as fs;
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

include!("runtime/repository_operations.rs");
include!("runtime/process_supervision.rs");

mod repository_fs;
use repository_fs::*;

include!("runtime/command_sandbox.rs");

#[cfg(test)]
mod tests;
