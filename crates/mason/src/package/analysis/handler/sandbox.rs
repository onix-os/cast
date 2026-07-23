use super::{execution::analyzer_cleanup_deadline, *};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SandboxSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    pub(super) size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl SandboxSnapshot {
    pub(super) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    pub(super) fn same_node(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

pub(super) fn open_sandbox_file(directory: &StdFile, name: &CStr, path: &Path) -> Result<StdFile, BoxError> {
    // SAFETY: directory and name are live; O_NOFOLLOW rejects a substituted
    // symlink and O_NONBLOCK prevents a substituted FIFO from blocking.
    let descriptor = unsafe {
        nix::libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            nix::libc::O_RDONLY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK
                | nix::libc::O_NOCTTY,
            0,
        )
    };
    if descriptor == -1 {
        Err(Box::new(AnalyzerSandboxError::OpenFile {
            path: path.to_owned(),
            source: std::io::Error::last_os_error(),
        }))
    } else {
        // SAFETY: openat returned a fresh owned descriptor.
        Ok(unsafe { StdFile::from_raw_fd(descriptor) })
    }
}

pub(super) fn digest_sandbox_file(mut file: StdFile, expected_size: u64, path: &Path) -> Result<[u8; 32], BoxError> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| AnalyzerSandboxError::Length {
                path: path.to_owned(),
                expected: expected_size,
                actual: u64::MAX,
            })?;
        if bytes > expected_size {
            return Err(Box::new(AnalyzerSandboxError::Length {
                path: path.to_owned(),
                expected: expected_size,
                actual: bytes,
            }));
        }
        hasher.update(&buffer[..read]);
    }
    if bytes != expected_size {
        return Err(Box::new(AnalyzerSandboxError::Length {
            path: path.to_owned(),
            expected: expected_size,
            actual: bytes,
        }));
    }
    Ok(hasher.finalize().into())
}

pub(super) fn require_sandbox_snapshot(
    path: &Path,
    expected: SandboxSnapshot,
    actual: &std::fs::Metadata,
    subject: &'static str,
) -> Result<(), BoxError> {
    let actual = SandboxSnapshot::from_metadata(actual);
    if actual == expected {
        Ok(())
    } else {
        Err(Box::new(AnalyzerSandboxError::SnapshotChanged {
            path: path.to_owned(),
            subject,
        }))
    }
}

pub(super) fn require_sandbox_node(
    path: &Path,
    expected: SandboxSnapshot,
    actual: &std::fs::Metadata,
    subject: &'static str,
) -> Result<(), BoxError> {
    if expected.same_node(SandboxSnapshot::from_metadata(actual)) {
        Ok(())
    } else {
        Err(Box::new(AnalyzerSandboxError::SnapshotChanged {
            path: path.to_owned(),
            subject,
        }))
    }
}

pub(super) fn validate_sandbox_component(file_name: &str) -> Result<(), BoxError> {
    CString::new(file_name).map_err(|_| {
        Box::new(AnalyzerSandboxError::InvalidName {
            name: file_name.to_owned(),
        }) as BoxError
    })?;
    if Path::new(file_name).file_name().and_then(|name| name.to_str()) == Some(file_name) {
        Ok(())
    } else {
        Err(Box::new(AnalyzerSandboxError::InvalidName {
            name: file_name.to_owned(),
        }))
    }
}

pub(super) fn read_mutated_sandbox_regular(
    info: &PathInfo,
    directory: &StdFile,
    name: &CStr,
    path: &Path,
    byte_limit: u64,
) -> Result<Vec<u8>, BoxError> {
    let mut file = open_sandbox_file(directory, name, path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(Box::new(AnalyzerSandboxError::InvalidMutatedOutput {
            path: path.to_owned(),
            detail: "expected one single-link regular file",
        }));
    }
    let expected = SandboxSnapshot::from_metadata(&metadata);
    if expected.size > byte_limit {
        return Err(Box::new(AnalyzerSandboxError::MutatedOutputTooLarge {
            path: path.to_owned(),
            size: expected.size,
            limit: byte_limit,
        }));
    }
    let capacity = usize::try_from(expected.size).map_err(|_| AnalyzerSandboxError::MutatedOutputAllocation {
        path: path.to_owned(),
        size: expected.size,
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|source| AnalyzerSandboxError::MutatedOutputReserve {
            path: path.to_owned(),
            size: expected.size,
            detail: source.to_string(),
        })?;
    let mut buffer = [0_u8; 64 * 1024];
    while bytes.len() < capacity {
        info.check_deadline()?;
        let allowed = (capacity - bytes.len()).min(buffer.len());
        let read = file.read(&mut buffer[..allowed])?;
        if read == 0 {
            return Err(Box::new(AnalyzerSandboxError::MutatedOutputLength {
                path: path.to_owned(),
                expected: expected.size,
                actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            }));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let mut probe = [0_u8; 1];
    if file.read(&mut probe)? != 0 {
        return Err(Box::new(AnalyzerSandboxError::MutatedOutputLength {
            path: path.to_owned(),
            expected: expected.size,
            actual: expected.size.saturating_add(1),
        }));
    }
    require_sandbox_snapshot(path, expected, &file.metadata()?, "mutable analyzer output descriptor")?;
    let reopened = open_sandbox_file(directory, name, path)?;
    require_sandbox_snapshot(path, expected, &reopened.metadata()?, "mutable analyzer output path")?;
    Ok(bytes)
}

pub(super) fn verify_mutated_sandbox_inventory(
    directory: &StdFile,
    display_path: &Path,
    expected_names: &[&CStr],
) -> Result<(), BoxError> {
    let entries = sandbox_directory_entries(directory, expected_names.len() + 1, analyzer_cleanup_deadline()).map_err(
        |source| AnalyzerSandboxError::Inspect {
            path: display_path.to_owned(),
            source,
        },
    )?;
    if entries.len() == expected_names.len()
        && expected_names
            .iter()
            .all(|expected| entries.iter().any(|actual| actual.as_bytes() == expected.to_bytes()))
    {
        Ok(())
    } else {
        Err(Box::new(AnalyzerSandboxError::InventoryChanged {
            path: display_path.to_owned(),
        }))
    }
}

pub(super) fn verify_sandbox_inventory(
    directory: &StdFile,
    display_path: &Path,
    expected_name: &OsStr,
    expected_snapshot: SandboxSnapshot,
) -> Result<(), BoxError> {
    // Enumerate a duplicate of the pinned descriptor, not the mutable
    // pathname. Checking metadata on both sides turns concurrent directory mutation
    // into a verification failure rather than a clean-path TOCTOU bypass.
    require_sandbox_snapshot(
        display_path,
        expected_snapshot,
        &directory.metadata()?,
        "analyzer sandbox directory before inventory",
    )?;
    let entries = sandbox_directory_entries(directory, 2, analyzer_cleanup_deadline()).map_err(|source| {
        AnalyzerSandboxError::Inspect {
            path: display_path.to_owned(),
            source,
        }
    })?;
    require_sandbox_snapshot(
        display_path,
        expected_snapshot,
        &directory.metadata()?,
        "analyzer sandbox directory after inventory",
    )?;
    if entries.len() == 1 && entries[0].as_bytes() == expected_name.as_bytes() {
        Ok(())
    } else {
        Err(Box::new(AnalyzerSandboxError::InventoryChanged {
            path: display_path.to_owned(),
        }))
    }
}

struct DirectoryStream(*mut nix::libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: fdopendir returned this uniquely owned stream.
        unsafe { nix::libc::closedir(self.0) };
    }
}

pub(super) fn sandbox_directory_entries(
    directory: &StdFile,
    entry_limit: usize,
    deadline: Instant,
) -> std::io::Result<Vec<CString>> {
    // fdopendir consumes its descriptor, so enumerate through a CLOEXEC
    // duplicate while retaining the authenticated directory descriptor.
    // SAFETY: directory is live and F_DUPFD_CLOEXEC returns a fresh descriptor.
    let descriptor = unsafe { nix::libc::fcntl(directory.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 3) };
    if descriptor == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: descriptor is a fresh directory descriptor transferred to DIR.
    let stream = unsafe { nix::libc::fdopendir(descriptor) };
    if stream.is_null() {
        let source = std::io::Error::last_os_error();
        // SAFETY: fdopendir failed and therefore did not consume descriptor.
        unsafe { nix::libc::close(descriptor) };
        return Err(source);
    }
    let stream = DirectoryStream(stream);
    // dup/fdopendir shares the underlying open-file-description offset with
    // the pinned fd. Always rewind before a fresh exact inventory pass.
    // SAFETY: stream is live and uniquely used by this enumeration.
    unsafe { nix::libc::rewinddir(stream.0) };
    let mut entries = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "sandbox directory enumeration exceeded its cleanup deadline",
            ));
        }
        // readdir uses a null result for both EOF and failure.
        // SAFETY: this project is Linux-only and the errno pointer is valid for
        // the current thread.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream remains live until the DirectoryStream is dropped.
        let entry = unsafe { nix::libc::readdir(stream.0) };
        if entry.is_null() {
            // SAFETY: see the errno reset immediately above.
            let errno = unsafe { *nix::libc::__errno_location() };
            if errno == 0 {
                break;
            }
            return Err(std::io::Error::from_raw_os_error(errno));
        }
        // SAFETY: POSIX guarantees a NUL-terminated d_name for a live dirent.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            if entries.len() == entry_limit {
                return Err(std::io::Error::other(format!(
                    "sandbox directory exceeds its {entry_limit}-entry enumeration limit"
                )));
            }
            entries
                .try_reserve(1)
                .map_err(|source| std::io::Error::other(format!("reserve sandbox directory entry: {source}")))?;
            let source = name.to_bytes_with_nul();
            let mut owned = Vec::new();
            owned
                .try_reserve_exact(source.len())
                .map_err(|error| std::io::Error::other(format!("reserve sandbox entry name: {error}")))?;
            owned.extend_from_slice(source);
            let owned = CString::from_vec_with_nul(owned)
                .map_err(|error| std::io::Error::other(format!("copy sandbox entry name: {error}")))?;
            entries.push(owned);
        }
    }
    Ok(entries)
}

fn sandbox_entry_is_directory(directory: &StdFile, name: &CStr) -> std::io::Result<bool> {
    let mut metadata = MaybeUninit::<nix::libc::stat>::uninit();
    // SAFETY: all pointers are live and AT_SYMLINK_NOFOLLOW authenticates the
    // directory entry itself rather than a possible symlink target.
    let result = unsafe {
        nix::libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            nix::libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fstatat initialized metadata on success.
    let metadata = unsafe { metadata.assume_init() };
    Ok(metadata.st_mode & nix::libc::S_IFMT == nix::libc::S_IFDIR)
}

pub(super) fn open_sandbox_directory(directory: &StdFile, name: &CStr) -> std::io::Result<StdFile> {
    // Refuse every mount crossing as well as symlink traversal. A hostile
    // analyzer must never turn cleanup into a recursive walk of a foreign bind
    // mount placed below its private directory.
    // SAFETY: an all-zero `open_how` is valid before its public fields are set.
    let mut how: nix::libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = u64::from(
        (nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NONBLOCK) as u32,
    );
    how.resolve = nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV;
    // SAFETY: directory, name, and `how` are live for this descriptor-relative
    // lookup and successful openat2 returns a fresh descriptor.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            directory.as_raw_fd(),
            name.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        let descriptor = i32::try_from(result)
            .map_err(|_| std::io::Error::other(format!("openat2 returned invalid descriptor {result}")))?;
        // SAFETY: openat returned a fresh owned descriptor.
        Ok(unsafe { StdFile::from_raw_fd(descriptor) })
    }
}

pub(super) fn empty_sandbox_directory(
    directory: &StdFile,
    remaining_entries: &mut usize,
    depth: usize,
    deadline: Instant,
) -> std::io::Result<()> {
    if Instant::now() >= deadline {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "sandbox cleanup exceeded its deadline",
        ));
    }
    if depth > SANDBOX_CLEANUP_DEPTH_LIMIT {
        return Err(std::io::Error::other(format!(
            "sandbox cleanup exceeded its {SANDBOX_CLEANUP_DEPTH_LIMIT}-directory depth limit"
        )));
    }
    // Multiple finite passes catch entries which were observed during a
    // concurrent rename without allowing hostile churn to make Drop unbounded.
    for _ in 0..8 {
        let entries = sandbox_directory_entries(directory, *remaining_entries, deadline)?;
        if entries.is_empty() {
            return Ok(());
        }
        for name in entries {
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "sandbox cleanup exceeded its deadline",
                ));
            }
            if *remaining_entries == 0 {
                return Err(std::io::Error::other(format!(
                    "sandbox cleanup exceeded its {SANDBOX_CLEANUP_ENTRY_LIMIT}-entry limit"
                )));
            }
            *remaining_entries -= 1;
            let is_directory = match sandbox_entry_is_directory(directory, &name) {
                Ok(value) => value,
                Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => continue,
                Err(source) => return Err(source),
            };
            if is_directory {
                let child = open_sandbox_directory(directory, &name)?;
                // SAFETY: child is the pinned directory just opened above.
                if unsafe { nix::libc::fchmod(child.as_raw_fd(), 0o700) } == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                empty_sandbox_directory(&child, remaining_entries, depth + 1, deadline)?;
                // SAFETY: parent and name are live and the child has been
                // emptied descriptor-relatively without following symlinks.
                let removed =
                    unsafe { nix::libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), nix::libc::AT_REMOVEDIR) };
                if removed == -1 {
                    let source = std::io::Error::last_os_error();
                    if source.raw_os_error() != Some(nix::libc::ENOENT) {
                        return Err(source);
                    }
                }
            } else {
                // SAFETY: parent and name are live; unlinkat never follows the
                // final component and therefore cannot escape the sandbox.
                let removed = unsafe { nix::libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
                if removed == -1 {
                    let source = std::io::Error::last_os_error();
                    if source.raw_os_error() != Some(nix::libc::ENOENT) {
                        return Err(source);
                    }
                }
            }
        }
    }
    Err(std::io::Error::other(
        "sandbox directory kept changing during descriptor-rooted cleanup",
    ))
}

pub(super) fn combine_finalization_errors(
    verification: Result<(), BoxError>,
    cleanup: Result<(), BoxError>,
    deadline: Result<(), BoxError>,
) -> Result<(), BoxError> {
    let mut errors = Vec::new();
    for error in [verification.err(), cleanup.err(), deadline.err()]
        .into_iter()
        .flatten()
    {
        errors.push(error.to_string());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Box::new(AnalyzerSandboxError::Finalization {
            details: errors.join("; "),
        }))
    }
}

pub(super) fn cleanup_unfinished_sandbox(directory: tempfile::TempDir, display_path: &Path) -> Result<(), BoxError> {
    let path = directory.path().to_owned();
    let protect = std::fs::set_permissions(&path, Permissions::from_mode(0o700));
    let close = directory.close();
    match (protect, close) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(source), Ok(())) | (Ok(()), Err(source)) => Err(Box::new(AnalyzerSandboxError::Cleanup {
            path: display_path.to_owned(),
            source,
        })),
        (Err(first), Err(second)) => Err(Box::new(AnalyzerSandboxError::Finalization {
            details: format!("failed to restore sandbox permissions: {first}; failed to remove sandbox: {second}"),
        })),
    }
}

#[derive(Debug, Error)]
#[error("{operation}; analyzer input finalization also failed: {finalization}")]
pub(super) struct AnalyzerOperationFinalizationError {
    pub(super) operation: BoxError,
    pub(super) finalization: BoxError,
}

#[derive(Debug, Error)]
pub(super) enum AnalyzerSandboxError {
    #[error("invalid analyzer sandbox file name {name:?}")]
    InvalidName { name: String },
    #[error("invalid analyzer sandbox directory path {path}")]
    InvalidDirectoryPath { path: PathBuf },
    #[error("failed to create private analyzer directory for {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to open private analyzer directory for {path}: {source}")]
    OpenDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to create private analyzer file for {path}: {source}")]
    CreateFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write private analyzer file for {path}: {source}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("private analyzer file for {path} changed length: expected {expected}, found {actual}")]
    Length { path: PathBuf, expected: u64, actual: u64 },
    #[error("failed to sync private analyzer file for {path}: {source}")]
    SyncFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to protect private analyzer file for {path}: {source}")]
    ProtectFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to open private analyzer file for {path}: {source}")]
    OpenFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to protect private analyzer directory for {path}: {source}")]
    ProtectDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to sync private analyzer directory for {path}: {source}")]
    SyncDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to inspect private analyzer input {path}: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{subject} changed during analysis at {path}")]
    SnapshotChanged { path: PathBuf, subject: &'static str },
    #[error("private analyzer directory inventory changed at {path}")]
    InventoryChanged { path: PathBuf },
    #[error("private analyzer input content changed at {path}")]
    DigestChanged { path: PathBuf },
    #[error("mutable analyzer output at {path} is invalid: {detail}")]
    InvalidMutatedOutput { path: PathBuf, detail: &'static str },
    #[error("mutable analyzer output at {path} is {size} bytes, exceeding the {limit}-byte limit")]
    MutatedOutputTooLarge { path: PathBuf, size: u64, limit: u64 },
    #[error("mutable analyzer output at {path} of {size} bytes cannot fit in memory")]
    MutatedOutputAllocation { path: PathBuf, size: u64 },
    #[error("failed to reserve {size} bytes for mutable analyzer output at {path}: {detail}")]
    MutatedOutputReserve { path: PathBuf, size: u64, detail: String },
    #[error("mutable analyzer output at {path} changed length: expected {expected}, found {actual}")]
    MutatedOutputLength { path: PathBuf, expected: u64, actual: u64 },
    #[error("private analyzer directory was detached before cleanup: {path}")]
    DetachedDirectory { path: PathBuf },
    #[error("unfinished private analyzer directory retained for namespace teardown: {path}")]
    UnfinishedDirectory { path: PathBuf },
    #[error("failed to clean private analyzer input {path}: {source}")]
    Cleanup {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("analyzer input finalization failed: {details}")]
    Finalization { details: String },
}
