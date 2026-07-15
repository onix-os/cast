struct RootHandle {
    path: PathBuf,
    file: fs::File,
    identity: Identity,
    descriptor_path: bool,
}

impl RootHandle {
    fn open(path: &Path) -> Result<Self, Error> {
        let path = std::path::absolute(path).map_err(|source| Error::Io {
            operation: "make Git materialization root absolute",
            path: path.to_owned(),
            source,
        })?;
        let file = openat2_file(
            libc::AT_FDCWD,
            path.as_os_str().as_bytes(),
            data_open_flags(true),
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
        )
        .map(|file| fs::File::from_parts(file, &path))
        .map_err(|source| Error::Io {
            operation: "open Git materialization root without symlinks",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| Error::Io {
            operation: "inspect opened Git materialization root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(Error::RootNotDirectory(path));
        }
        Ok(Self {
            path,
            identity: Identity::from_metadata(&metadata),
            file,
            descriptor_path: false,
        })
    }

    fn open_descriptor_path(path: &Path) -> Result<Self, Error> {
        let path = std::path::absolute(path).map_err(|source| Error::Io {
            operation: "make descriptor-rooted Git materialization path absolute",
            path: path.to_owned(),
            source,
        })?;
        let file = open_path_file(&path, data_open_flags(true))
            .map(|file| fs::File::from_parts(file, &path))
            .map_err(|source| Error::Io {
                operation: "open descriptor-rooted Git materialization root",
                path: path.clone(),
                source,
            })?;
        let metadata = file.metadata().map_err(|source| Error::Io {
            operation: "inspect descriptor-rooted Git materialization root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(Error::RootNotDirectory(path));
        }
        Ok(Self {
            path,
            identity: Identity::from_metadata(&metadata),
            file,
            descriptor_path: true,
        })
    }

    fn display_path(&self, relative: &[u8]) -> PathBuf {
        if relative.is_empty() {
            self.path.clone()
        } else {
            self.path.join(OsStr::from_bytes(relative))
        }
    }

    fn open_relative(&self, relative: &[u8], flags: i32, operation: &'static str) -> Result<fs::File, Error> {
        let path = self.display_path(relative);
        // A duplicated directory descriptor shares its directory-stream
        // offset with the original open file description. Open `.` afresh for
        // the root so every full-tree scan begins at offset zero.
        let relative = if relative.is_empty() { b".".as_slice() } else { relative };
        openat2_file(
            self.file.as_raw_fd(),
            relative,
            flags,
            libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV,
        )
        .map(|file| fs::File::from_parts(file, &path))
        .map_err(|source| Error::Io {
            operation,
            path,
            source,
        })
    }

    fn inspect_entry(
        &self,
        relative: Vec<u8>,
        context: &mut ScanContext<'_>,
        expected: Option<&[Entry]>,
    ) -> Result<Entry, Error> {
        context.check_time(&self.display_path(&relative))?;
        let path = self.display_path(&relative);
        let file = self.open_relative(
            &relative,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            "open Git materialization entry for inspection",
        )?;
        let metadata = file.metadata().map_err(|source| Error::Io {
            operation: "inspect opened Git materialization entry",
            path: path.clone(),
            source,
        })?;
        let identity = Identity::from_metadata(&metadata);
        let kind = classify_handle(&path, &file, &metadata, &relative, identity, expected, context)?;
        require_single_link(&path, &metadata, &kind)?;
        context.admit_kind(&kind, &path)?;
        Ok(Entry {
            relative,
            identity,
            kind,
            stamp: MetadataStamp::from_metadata(&metadata),
        })
    }

    fn open_data(&self, entry: &Entry) -> Result<fs::File, Error> {
        self.open_relative(
            &entry.relative,
            data_open_flags(entry.kind.is_directory()),
            "open Git materialization entry beneath root",
        )
    }

    fn open_inspection(&self, entry: &Entry) -> Result<fs::File, Error> {
        self.open_relative(
            &entry.relative,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            "reopen Git materialization entry for inspection",
        )
    }

    fn retarget(&mut self, path: &Path, descriptor_path: bool) -> Result<(), Error> {
        let path = std::path::absolute(path).map_err(|source| Error::Io {
            operation: "make installed Git materialization path absolute",
            path: path.to_owned(),
            source,
        })?;
        let reopened = if descriptor_path {
            open_path_file(&path, data_open_flags(true))
        } else {
            openat2_file(
                libc::AT_FDCWD,
                path.as_os_str().as_bytes(),
                data_open_flags(true),
                libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            )
        }
        .map_err(|source| Error::Io {
            operation: "open installed Git materialization root without symlinks",
            path: path.clone(),
            source,
        })?;
        let metadata = reopened.metadata().map_err(|source| Error::Io {
            operation: "inspect installed Git materialization root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() || Identity::from_metadata(&metadata) != self.identity {
            return Err(Error::EntryChanged(path));
        }
        self.path = path;
        self.descriptor_path = descriptor_path;
        Ok(())
    }

    fn require_path_identity(&self) -> Result<(), Error> {
        let reopened = if self.descriptor_path {
            open_path_file(&self.path, data_open_flags(true))
        } else {
            openat2_file(
                libc::AT_FDCWD,
                self.path.as_os_str().as_bytes(),
                data_open_flags(true),
                libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            )
        }
        .map_err(|source| Error::Io {
            operation: "reopen Git materialization root without symlinks",
            path: self.path.clone(),
            source,
        })?;
        let metadata = reopened.metadata().map_err(|source| Error::Io {
            operation: "verify Git materialization root path",
            path: self.path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() || Identity::from_metadata(&metadata) != self.identity {
            return Err(Error::EntryChanged(self.path.clone()));
        }
        Ok(())
    }
}

fn open_path_file(path: &Path, flags: i32) -> io::Result<std::fs::File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: `path` is NUL-terminated and a successful `open` returns a new
    // descriptor. O_NOFOLLOW applies to the checkout itself while permitting
    // the intentional held-fd magic link in an ancestor component.
    let result = unsafe { libc::open(path.as_ptr(), flags) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: successful open returned a fresh owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(result) }.into())
    }
}

fn data_open_flags(directory: bool) -> i32 {
    let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOATIME;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    flags
}

fn openat2_file(dirfd: RawFd, path: &[u8], flags: i32, resolve: u64) -> io::Result<std::fs::File> {
    let path = CString::new(path).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: every field of Linux `open_how` accepts zero, after which the
    // public fields used by this ABI version are initialized explicitly.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = 0;
    how.resolve = resolve;
    // SAFETY: `path` is NUL-terminated, `how` points to an initialized
    // `open_how`, and a successful syscall returns a new owned descriptor.
    let result = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            size_of::<libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat2 returns a fresh descriptor owned by us.
    let descriptor = unsafe { OwnedFd::from_raw_fd(result as RawFd) };
    Ok(descriptor.into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    relative: Vec<u8>,
    identity: Identity,
    kind: EntryKind,
    stamp: MetadataStamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MetadataStamp {
    mode: u32,
    links: u64,
    atime: i64,
    atime_nsec: i64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

impl MetadataStamp {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            mode: metadata.mode() & 0o7777,
            links: metadata.nlink(),
            atime: metadata.atime(),
            atime_nsec: metadata.atime_nsec(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryKind {
    Directory,
    Regular { executable: bool, length: u64 },
    Symlink { target: Vec<u8> },
}

impl EntryKind {
    fn tag(&self) -> u8 {
        match self {
            Self::Directory => DIRECTORY_TAG,
            Self::Regular { .. } => REGULAR_TAG,
            Self::Symlink { .. } => SYMLINK_TAG,
        }
    }

    fn normalized_mode(&self) -> u32 {
        match self {
            Self::Directory => DIRECTORY_MODE,
            Self::Regular { executable: true, .. } => EXECUTABLE_MODE,
            Self::Regular { executable: false, .. } => REGULAR_MODE,
            Self::Symlink { .. } => SYMLINK_MODE,
        }
    }

    fn is_directory(&self) -> bool {
        matches!(self, Self::Directory)
    }
}

#[derive(Debug, Default)]
struct ScanUsage {
    entries: u64,
    name_bytes: u64,
    path_bytes: u64,
    symlink_target_bytes: u64,
    regular_bytes: u64,
}

struct ScanContext<'a> {
    limits: MaterializationLimits,
    deadline: &'a Deadline,
    usage: ScanUsage,
}

impl<'a> ScanContext<'a> {
    fn new(limits: MaterializationLimits, deadline: &'a Deadline) -> Self {
        Self {
            limits,
            deadline,
            usage: ScanUsage::default(),
        }
    }

    fn check_time(&self, path: &Path) -> Result<(), Error> {
        self.deadline.check(path)
    }

    fn admit_entry_bytes(
        &mut self,
        name_bytes: usize,
        path_bytes: usize,
        depth: usize,
        path: &Path,
    ) -> Result<(), Error> {
        self.check_time(path)?;
        enforce_usize_limit("entry depth", self.limits.max_depth, depth, path)?;
        enforce_usize_limit("entry name bytes", self.limits.max_name_bytes, name_bytes, path)?;
        enforce_usize_limit("relative path bytes", self.limits.max_path_bytes, path_bytes, path)?;
        self.usage.entries = checked_add_limit("total entries", self.usage.entries, 1, self.limits.max_entries, path)?;
        self.usage.name_bytes = checked_add_limit(
            "total entry name bytes",
            self.usage.name_bytes,
            usize_to_u64(name_bytes, "total entry name bytes", path)?,
            self.limits.max_total_name_bytes,
            path,
        )?;
        self.usage.path_bytes = checked_add_limit(
            "total relative path bytes",
            self.usage.path_bytes,
            usize_to_u64(path_bytes, "total relative path bytes", path)?,
            self.limits.max_total_path_bytes,
            path,
        )?;
        Ok(())
    }

    fn admit_kind(&mut self, kind: &EntryKind, path: &Path) -> Result<(), Error> {
        self.check_time(path)?;
        match kind {
            EntryKind::Regular { length, .. } => {
                enforce_u64_limit("regular file bytes", self.limits.max_file_bytes, *length, path)?;
                self.usage.regular_bytes = checked_add_limit(
                    "total regular file bytes",
                    self.usage.regular_bytes,
                    *length,
                    self.limits.max_total_regular_bytes,
                    path,
                )?;
            }
            EntryKind::Symlink { target } => {
                enforce_usize_limit(
                    "symlink target bytes",
                    self.limits.max_symlink_target_bytes,
                    target.len(),
                    path,
                )?;
                self.usage.symlink_target_bytes = checked_add_limit(
                    "total symlink target bytes",
                    self.usage.symlink_target_bytes,
                    usize_to_u64(target.len(), "total symlink target bytes", path)?,
                    self.limits.max_total_symlink_target_bytes,
                    path,
                )?;
            }
            EntryKind::Directory => {}
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Deadline {
    started: Instant,
    limit: Duration,
}

impl Deadline {
    fn new(limit: Duration) -> Self {
        Self {
            started: Instant::now(),
            limit,
        }
    }

    fn check(&self, path: &Path) -> Result<(), Error> {
        if self.started.elapsed() >= self.limit {
            Err(Error::DurationExceeded {
                path: path.to_owned(),
                limit: self.limit,
            })
        } else {
            Ok(())
        }
    }
}
