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
use fs_err::os::unix::fs::OpenOptionsExt as _;
use tokio::{
    io::{self, AsyncReadExt},
    process,
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
    /// draining and progress parsing while caller code yields control. Public
    /// progress callbacks run synchronously and must return promptly; no Rust
    /// API can preempt arbitrary blocking code inside a callback.
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
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RepositoryIdentity {
    device: u64,
    inode: u64,
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
    /// A callback is fired repeatedly to track the cloning
    /// process in real time. The callback runs synchronously in the supervisor
    /// task and must not block, perform I/O, or re-enter this repository.
    pub async fn clone_mirror_progress<F>(path: &Path, url: &Url, callback: F) -> Result<Self, Error>
    where
        F: Fn(FetchProgress),
    {
        Self::clone_mirror_progress_with_limits(path, url, Limits::DEFAULT, callback).await
    }

    /// Clones a progress-reporting mirror under an explicit resource policy.
    /// The callback runs synchronously and must return promptly; its code is
    /// outside the boundary that a subprocess deadline can forcibly preempt.
    pub async fn clone_mirror_progress_with_limits<F>(
        path: &Path,
        url: &Url,
        limits: Limits,
        callback: F,
    ) -> Result<Self, Error>
    where
        F: Fn(FetchProgress),
    {
        clone_mirror_impl(path, url, limits, Some(callback)).await
    }

    /// Whether this repository has a commit identified by its hash.
    pub async fn has_commit(&self, commit: &str) -> Result<bool, Error> {
        let root = self.transaction_root()?;
        self.verify_usage(&root)?;
        let output = run_git_in_directory(
            &[OsStr::new("cat-file"), OsStr::new("-t"), OsStr::new(commit)],
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
        if object_id.len() != 40
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
        let root = self.transaction_root()?;
        self.verify_usage(&root)?;
        let output = run_git_in_directory(
            &[OsStr::new("remote"), OsStr::new("get-url"), OsStr::new(remote)],
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
        let root = self.transaction_root()?;
        self.verify_usage(&root)?;
        run_git_in_directory(
            &[
                OsStr::new("remote"),
                OsStr::new("set-url"),
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
    /// A callback is fired repeatedly to track the fetching
    /// process in real time. The callback runs synchronously in the supervisor
    /// task and must not block, perform I/O, or re-enter this repository.
    ///
    /// Fetch mutates the repository. On failure, cache-owning callers must
    /// discard the repository before reuse. Gitwrap never implicitly deletes
    /// an arbitrary path supplied through [`Self::open_bare`].
    pub async fn fetch_progress<F>(&self, callback: F) -> Result<(), Error>
    where
        F: Fn(FetchProgress),
    {
        let root = self.transaction_root()?;
        self.verify_usage(&root)?;
        run_git_in_directory(
            &[
                OsStr::new("fetch"),
                OsStr::new("--no-recurse-submodules"),
                OsStr::new("--progress"),
            ],
            self.limits,
            &root,
            Some(MonitoredRepository::directory(&root)?),
            Some(callback),
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
        if let Err(error) = verify_repository_path_identity(&path, &root) {
            return Err(error);
        }
        let identity = RepositoryIdentity::from_directory(&root)?;

        Ok(Self {
            path,
            limits: self.limits,
            identity: Some(identity),
        })
    }

    /// Whether `rev` contains Gitlink entries whose contents would require
    /// an additional, independently fetched submodule source graph.
    pub async fn contains_gitlinks(&self, rev: &str) -> Result<bool, Error> {
        let root = self.transaction_root()?;
        self.verify_usage(&root)?;
        let output = run_git_in_directory(
            &[
                OsStr::new("ls-tree"),
                OsStr::new("-r"),
                OsStr::new("--full-tree"),
                OsStr::new("--format=%(objectmode)"),
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

/// The argument of callbacks when they are invoked
/// for reporting a Git operation's progress.
pub struct FetchProgress {
    /// Completion percentage.
    pub percent: u8,
    /// Download speed in formatted human units per second
    pub speed: String,
}

async fn clone_mirror_impl<F>(path: &Path, url: &Url, limits: Limits, callback: Option<F>) -> Result<Repository, Error>
where
    F: Fn(FetchProgress),
{
    let limits = limits.validate()?;
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
    if let Err(source) = staging.close() {
        let cleanup = remove_path(&path).err().unwrap_or(source);
        return Err(InnerError::Cleanup(cleanup).into());
    }

    Ok(Repository {
        path,
        limits,
        identity: Some(identity),
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
            Self::Path(path) => RepositoryUsageScanner::new(path, limits),
            Self::Directory(directory) => {
                RepositoryUsageScanner::from_directory(directory.try_clone().map_err(InnerError::from)?, limits)
            }
        }
    }

    fn verify(&self, limits: Limits) -> Result<RepositoryUsage, Error> {
        match self {
            Self::Path(path) => verify_repository_usage(path, limits),
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

fn verify_repository_usage_directory(directory: &fs::File, limits: Limits) -> Result<RepositoryUsage, Error> {
    let deadline = Instant::now()
        .checked_add(limits.wall_timeout)
        .ok_or(InnerError::InvalidLimits)?;
    let mut scanner = RepositoryUsageScanner::from_directory(directory.try_clone().map_err(InnerError::from)?, limits)?;
    while !scanner.advance(8192, Some(deadline))? {}
    Ok(scanner.usage)
}

fn verify_repository_usage_before(path: &Path, limits: Limits, deadline: Instant) -> Result<RepositoryUsage, Error> {
    let mut scanner = RepositoryUsageScanner::new(path, limits)?;
    while !scanner.advance(8192, Some(deadline))? {}
    Ok(scanner.usage)
}

struct RepositoryUsageScanner {
    limits: Limits,
    usage: RepositoryUsage,
    directory_limit: usize,
    directories: Vec<DirectoryCursor>,
}

impl RepositoryUsageScanner {
    fn new(path: &Path, limits: Limits) -> Result<Self, Error> {
        let directory_limit = scanner_directory_limit(limits)?;
        let root = match DirectoryCursor::open(path) {
            Ok(root) => root,
            Err(source) if source.kind() == std_io::ErrorKind::NotFound => {
                return Ok(Self {
                    limits,
                    usage: RepositoryUsage::default(),
                    directory_limit,
                    directories: Vec::new(),
                });
            }
            Err(source)
                if source.kind() == std_io::ErrorKind::NotADirectory
                    || source.raw_os_error() == Some(nix::libc::ELOOP) =>
            {
                return Err(InnerError::InvalidRepositoryRoot.into());
            }
            Err(source) => return Err(InnerError::Io(source).into()),
        };
        Self::from_root(root, limits, directory_limit)
    }

    fn from_directory(directory: fs::File, limits: Limits) -> Result<Self, Error> {
        let directory_limit = scanner_directory_limit(limits)?;
        let root = DirectoryCursor::from_directory(directory).map_err(InnerError::from)?;
        Self::from_root(root, limits, directory_limit)
    }

    fn from_root(root: DirectoryCursor, limits: Limits, directory_limit: usize) -> Result<Self, Error> {
        let metadata = root.directory.metadata().map_err(InnerError::from)?;
        let usage = RepositoryUsage {
            logical_bytes: metadata.len(),
            allocated_bytes: metadata.blocks().saturating_mul(512),
            entries: 0,
        };
        enforce_repository_usage(usage, limits)?;
        Ok(Self {
            limits,
            usage,
            directory_limit,
            directories: vec![root],
        })
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
                Err(source) if source.kind() == std_io::ErrorKind::NotFound => continue,
                Err(source) => return Err(InnerError::Io(source).into()),
            };
            processed += 1;
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(source) if source.kind() == std_io::ErrorKind::NotFound => continue,
                Err(source) => return Err(InnerError::Io(source).into()),
            };
            self.usage.entries = self.usage.entries.saturating_add(1);
            self.usage.logical_bytes = self.usage.logical_bytes.saturating_add(metadata.len());
            self.usage.allocated_bytes = self
                .usage
                .allocated_bytes
                .saturating_add(metadata.blocks().saturating_mul(512));
            enforce_repository_usage(self.usage, self.limits)?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                if self.directories.len() >= self.directory_limit {
                    return Err(InnerError::RepositoryDepth {
                        limit: self.directory_limit,
                    }
                    .into());
                }
                match DirectoryCursor::open(&entry.path()) {
                    Ok(directory) => self.directories.push(directory),
                    Err(source) if transient_directory_replacement(&source) => {}
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
}

impl DirectoryCursor {
    fn open(path: &Path) -> std_io::Result<Self> {
        Self::from_directory(open_directory(path)?)
    }

    fn from_directory(directory: fs::File) -> std_io::Result<Self> {
        let descriptor_path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        let entries = fs::read_dir(descriptor_path)?;
        Ok(Self { directory, entries })
    }
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
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.symlinks=true",
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
        };

        let error = repository.fetch_progress(|_| {}).await.unwrap_err();
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
        };
        let peeled = repository.peel_commit("v1").await.unwrap();

        assert_eq!(peeled, commit);
        assert_eq!(peeled.len(), 40);
        assert!(peeled.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
    }
}
