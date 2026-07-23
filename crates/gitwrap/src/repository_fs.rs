//! Descriptor-rooted repository filesystem inspection and cache hardening.

use std::{
    collections::BTreeMap,
    ffi::{CString, OsStr},
    io as std_io,
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::{
            ffi::OsStrExt as _,
            fs::{MetadataExt as _, PermissionsExt as _},
        },
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fs_err as fs;
use fs_err::os::unix::fs::OpenOptionsExt as _;
use tokio::time::Instant;
use url::Url;

use super::{
    error::{Constraint, InnerError},
    run_git_in_directory, validate_transport_url, Error, FetchProgress, Limits, ObjectFormat,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct RepositoryUsage {
    pub(super) logical_bytes: u64,
    pub(super) allocated_bytes: u64,
    pub(super) entries: u64,
}

pub(super) fn open_repository_directory(path: &Path) -> Result<fs::File, Error> {
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

pub(super) async fn verify_bare_repository(root: &fs::File, limits: Limits) -> Result<(), Error> {
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
pub(super) async fn inspect_private_mirror_config(
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

pub(super) fn canonical_mirror_config(origin: &Url, object_format: ObjectFormat) -> Result<Vec<u8>, Error> {
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
pub(super) fn write_canonical_mirror_config(
    root: &fs::File,
    origin: &Url,
    object_format: ObjectFormat,
) -> Result<(), Error> {
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

pub(super) fn verify_canonical_mirror_config(
    root: &fs::File,
    origin: &Url,
    object_format: ObjectFormat,
) -> Result<(), Error> {
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

pub(super) fn secure_mirror_permissions(root: &fs::File) -> Result<(), Error> {
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

pub(super) fn verify_repository_path_identity(path: &Path, directory: &fs::File) -> Result<(), Error> {
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

pub(super) fn verify_repository_usage(path: &Path, limits: Limits) -> Result<RepositoryUsage, Error> {
    let deadline = Instant::now()
        .checked_add(limits.wall_timeout)
        .ok_or(InnerError::InvalidLimits)?;
    verify_repository_usage_before(path, limits, deadline)
}

/// Creation monitors start before Git creates their staging root. Absence is
/// accepted only at this explicit pre-spawn boundary; every mandatory scan of
/// an expected root uses strict mode and rejects disappearance.
pub(super) fn verify_repository_usage_or_absent_for_creation(
    path: &Path,
    limits: Limits,
) -> Result<RepositoryUsage, Error> {
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

pub(super) fn verify_repository_usage_directory(
    directory: &fs::File,
    limits: Limits,
) -> Result<RepositoryUsage, Error> {
    let deadline = Instant::now()
        .checked_add(limits.wall_timeout)
        .ok_or(InnerError::InvalidLimits)?;
    verify_two_repository_snapshots(|| scan_repository_directory_strict(directory, limits, deadline))
}

pub(super) fn verify_repository_usage_before(
    path: &Path,
    limits: Limits,
    deadline: Instant,
) -> Result<RepositoryUsage, Error> {
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

pub(super) fn scan_repository_path_strict(
    path: &Path,
    limits: Limits,
    deadline: Instant,
) -> Result<RepositorySnapshot, Error> {
    let mut scanner = RepositoryUsageScanner::new(path, limits, ScanMode::Strict)?;
    while !scanner.advance(8192, Some(deadline))? {}
    Ok(scanner.snapshot)
}

pub(super) fn verify_two_repository_snapshots(
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
pub(super) enum ScanMode {
    Live,
    Strict,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct RepositorySnapshot {
    pub(super) usage: RepositoryUsage,
    pub(super) entries: BTreeMap<Vec<u8>, SnapshotMetadata>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SnapshotMetadata {
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

pub(super) struct RepositoryUsageScanner {
    limits: Limits,
    mode: ScanMode,
    pub(super) snapshot: RepositorySnapshot,
    pub(super) snapshot_bytes: u64,
    snapshot_limit: u64,
    directory_limit: usize,
    pub(super) directories: Vec<DirectoryCursor>,
}

const SNAPSHOT_ENTRY_OVERHEAD: u64 = 128;

impl RepositoryUsageScanner {
    pub(super) fn new(path: &Path, limits: Limits, mode: ScanMode) -> Result<Self, Error> {
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

    pub(super) fn from_directory(directory: fs::File, limits: Limits, mode: ScanMode) -> Result<Self, Error> {
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
    pub(super) fn advance(&mut self, entry_budget: usize, deadline: Option<Instant>) -> Result<bool, Error> {
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

pub(super) fn scanner_cursor_capacity(ceiling: u64, open: u64, reserve: u64, descriptors_per_cursor: u64) -> u64 {
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

pub(super) struct DirectoryCursor {
    /// Keeps the inode pinned while `/proc/self/fd/<n>` is used to open an
    /// enumeration cursor. Descendants are opened with O_NOFOLLOW, so replacing
    /// a name with a symlink cannot redirect accounting outside this root.
    directory: fs::File,
    entries: fs::ReadDir,
    relative: Vec<u8>,
}

impl DirectoryCursor {
    pub(super) fn open(path: &Path) -> std_io::Result<Self> {
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

    pub(super) fn child_relative(&self, name: &[u8], remaining: u64, snapshot_limit: u64) -> Result<Vec<u8>, Error> {
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

pub(super) fn scan_metadata_at(
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

pub(super) fn open_directory(path: &Path) -> std_io::Result<fs::File> {
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

pub(super) fn remove_path(path: &Path) -> std_io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == std_io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(target_pointer_width = "64")]
fn rlim_to_u64(value: nix::libc::rlim_t) -> u64 {
    value
}

#[cfg(not(target_pointer_width = "64"))]
fn rlim_to_u64(value: nix::libc::rlim_t) -> u64 {
    u64::from(value)
}
