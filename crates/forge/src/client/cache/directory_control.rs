const PRIVATE_FILE_MODE: u32 = 0o600;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const CACHE_DIRECTORY_MODE: u32 = 0o755;
const MAX_PUBLICATION_ATTEMPTS: usize = 8;
const MAX_PRIVATE_STAGE_ENTRIES: usize = 64;
const MAX_PRIVATE_STAGE_NAME_BYTES: usize = 16 * 1024;
const PUBLICATION_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const PUBLICATION_LOCK_RETRY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileFingerprint {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
struct Directory {
    file: std::fs::File,
    path: PathBuf,
}

impl Directory {
    fn open_absolute(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cache directory must be absolute: {}", path.display()),
            ));
        }
        let relative = path
            .strip_prefix(Path::new("/"))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cache directory is not absolute"))?;
        let root = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
            .open("/")?;
        let file = if relative.as_os_str().is_empty() {
            root.try_clone()?
        } else {
            openat2(
                root.as_raw_fd(),
                relative.as_os_str(),
                nix::libc::O_RDONLY | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                0,
                nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS,
            )?
        };
        // A pre-existing capability root is evidence, not something this
        // boundary may launder with chmod. Callers must provision it safely.
        validate_directory(&file, None)?;
        Ok(Self {
            file,
            path: path.to_owned(),
        })
    }

    fn open_or_create_directory(&self, name: &OsStr, mode: u32) -> io::Result<Self> {
        validate_component(name)?;
        let name_c = cstring(name)?;
        // SAFETY: the parent and NUL-terminated component remain live.
        let mkdir_result = unsafe { nix::libc::mkdirat(self.file.as_raw_fd(), name_c.as_ptr(), mode) };
        let created = if mkdir_result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
            false
        } else {
            true
        };
        let file = openat2(
            self.file.as_raw_fd(),
            name,
            nix::libc::O_RDONLY | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            beneath_no_links(),
        )?;
        if created {
            // Only an inode created by this call may be normalized against a
            // hostile umask. Existing entries must already satisfy policy.
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        validate_directory(&file, Some(mode))?;
        Ok(Self {
            file,
            path: self.path.join(name),
        })
    }

    fn open_regular(&self, name: &OsStr) -> io::Result<std::fs::File> {
        validate_component(name)?;
        openat2(
            self.file.as_raw_fd(),
            name,
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            0,
            beneath_no_links(),
        )
    }

    fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

fn validate_directory(file: &std::fs::File, exact_mode: Option<u32>) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache component is not a directory",
        ));
    }
    if metadata.uid() != nix::unistd::Uid::effective().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cache directory is not owned by the effective user",
        ));
    }
    let mode = metadata.mode() & 0o7777;
    if exact_mode.is_some_and(|expected| mode != expected) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "cache directory mode is {mode:04o}, expected exactly {:04o}",
                exact_mode.unwrap()
            ),
        ));
    }
    if mode & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("cache directory mode {mode:04o} permits group/other writes"),
        ));
    }
    Ok(())
}

fn beneath_no_links() -> u64 {
    nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV
}

fn openat2(dirfd: RawFd, path: &OsStr, flags: i32, mode: u32, resolve: u64) -> io::Result<std::fs::File> {
    let path = cstring(path)?;
    // SAFETY: zero is valid for every `open_how` field.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: `dirfd`, the C string, and `open_how` remain live. Success
    // returns one fresh descriptor owned below.
    let descriptor = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = i32::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned this fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(std::fs::File::from(descriptor))
}

fn cstring(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn validate_component(component: &OsStr) -> io::Result<()> {
    let bytes = component.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid cache path component {component:?}"),
        ));
    }
    Ok(())
}

fn random_stage_name(prefix: &str) -> io::Result<String> {
    let mut random = [0_u8; 16];
    let mut filled = 0;
    while filled < random.len() {
        // SAFETY: the remaining slice is writable for the supplied length.
        let result = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_getrandom,
                random[filled..].as_mut_ptr(),
                random.len() - filled,
                0,
            )
        };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let read = usize::try_from(result).map_err(|_| io::Error::other("getrandom returned an invalid length"))?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "getrandom returned no bytes",
            ));
        }
        filled += read;
    }
    let suffix = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    Ok(format!("{prefix}{suffix}"))
}

fn create_unique_file(parent: &Directory, prefix: &str) -> io::Result<(String, std::fs::File)> {
    for _ in 0..128 {
        let name = random_stage_name(prefix)?;
        match openat2(
            parent.file.as_raw_fd(),
            OsStr::new(&name),
            nix::libc::O_RDWR
                | nix::libc::O_CREAT
                | nix::libc::O_EXCL
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            PRIVATE_FILE_MODE,
            beneath_no_links(),
        ) {
            Ok(file) => {
                if let Err(error) = file
                    .set_permissions(std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))
                    .and_then(|()| {
                        validate_regular_metadata(&file.metadata()?, None, u64::MAX, Some(PRIVATE_FILE_MODE)).map(drop)
                    })
                {
                    let _ = unlink_entry(parent, OsStr::new(&name));
                    return Err(error);
                }
                return Ok((name, file));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique cache stage",
    ))
}

struct AnonymousFile {
    file: std::fs::File,
}

impl AnonymousFile {
    fn create(parent: &Directory, prefix: &str) -> io::Result<Self> {
        let (name, file) = create_unique_file(parent, prefix)?;
        unlink_entry(parent, OsStr::new(&name))?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() || metadata.nlink() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "anonymous content stage did not detach from its pathname",
            ));
        }
        Ok(Self { file })
    }
}

struct NamedStageFile {
    parent: std::fs::File,
    name: String,
    file: std::fs::File,
    moved: bool,
}

impl NamedStageFile {
    fn create(parent: &Directory, prefix: &str) -> io::Result<Self> {
        let parent_file = parent.file.try_clone()?;
        let (name, file) = create_unique_file(parent, prefix)?;
        Ok(Self {
            parent: parent_file,
            name,
            file,
            moved: false,
        })
    }

    fn mark_moved(&mut self) {
        self.moved = true;
    }
}

impl Drop for NamedStageFile {
    fn drop(&mut self) {
        if !self.moved {
            let _ = unlinkat(self.parent.as_raw_fd(), OsStr::new(&self.name), 0);
        }
    }
}

/// Rolls back only the exact inode moved into a public name if any durability
/// or post-publication authentication step fails.
struct PublishedEntryGuard {
    parent: std::fs::File,
    name: OsString,
    device: u64,
    inode: u64,
    armed: bool,
}

impl PublishedEntryGuard {
    fn new(parent: &Directory, name: &OsStr, fingerprint: FileFingerprint) -> io::Result<Self> {
        Ok(Self {
            parent: parent.file.try_clone()?,
            name: name.to_owned(),
            device: fingerprint.device,
            inode: fingerprint.inode,
            armed: false,
        })
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PublishedEntryGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let parent = Directory {
            file: match self.parent.try_clone() {
                Ok(file) => file,
                Err(_) => return,
            },
            path: PathBuf::new(),
        };
        let Ok(file) = parent.open_regular(&self.name) else {
            return;
        };
        let Ok(metadata) = file.metadata() else {
            return;
        };
        if (metadata.dev(), metadata.ino()) == (self.device, self.inode) {
            let _ = unlinkat(self.parent.as_raw_fd(), &self.name, 0);
            let _ = self.parent.sync_all();
        }
    }
}

struct PrivateStageDirectory {
    parent: std::fs::File,
    name: String,
    path: PathBuf,
    directory: Directory,
}

impl PrivateStageDirectory {
    fn create(parent: &Directory, prefix: &str) -> io::Result<Self> {
        let parent_file = parent.file.try_clone()?;
        for _ in 0..128 {
            let name = random_stage_name(prefix)?;
            let name_c = cstring(OsStr::new(&name))?;
            // SAFETY: parent and component are valid and live.
            let result =
                unsafe { nix::libc::mkdirat(parent.file.as_raw_fd(), name_c.as_ptr(), PRIVATE_DIRECTORY_MODE) };
            if result == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(error);
            }
            let directory = match parent.open_or_create_directory(OsStr::new(&name), PRIVATE_DIRECTORY_MODE) {
                Ok(directory) => directory,
                Err(error) => {
                    let _ = unlinkat(parent.file.as_raw_fd(), OsStr::new(&name), nix::libc::AT_REMOVEDIR);
                    return Err(error);
                }
            };
            return Ok(Self {
                parent: parent_file,
                name,
                path: directory.path.clone(),
                directory,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique download stage directory",
        ))
    }

    fn require_inventory(&self, expected: &[&OsStr]) -> io::Result<()> {
        let mut actual = directory_entry_names(&self.directory)?;
        let mut expected = expected.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        if actual != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("private download stage contains {actual:?}, expected exactly {expected:?}"),
            ));
        }
        Ok(())
    }

    fn cleanup_entries(&self) {
        if let Ok(entries) = directory_entry_names(&self.directory) {
            for entry in entries {
                let _ = unlinkat(self.directory.file.as_raw_fd(), &entry, 0);
            }
            let _ = self.directory.sync();
        }
    }
}

impl Drop for PrivateStageDirectory {
    fn drop(&mut self) {
        self.cleanup_entries();
        let _ = unlinkat(self.parent.as_raw_fd(), OsStr::new(&self.name), nix::libc::AT_REMOVEDIR);
    }
}

fn directory_entry_names(directory: &Directory) -> io::Result<Vec<OsString>> {
    // fdopendir owns its descriptor, so duplicate the retained capability.
    // SAFETY: fcntl receives a live descriptor and returns a fresh one.
    let duplicate = unsafe { nix::libc::fcntl(directory.file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fcntl returned one fresh descriptor.
    let duplicate = unsafe { OwnedFd::from_raw_fd(duplicate) };
    // SAFETY: fdopendir consumes the raw descriptor on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate.as_raw_fd()) };
    if stream.is_null() {
        return Err(io::Error::last_os_error());
    }
    std::mem::forget(duplicate);

    struct Stream(*mut nix::libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            // SAFETY: this stream is uniquely owned and still open.
            let _ = unsafe { nix::libc::closedir(self.0) };
        }
    }
    let stream = Stream(stream);
    let mut entries = Vec::new();
    let mut total_name_bytes = 0_usize;
    loop {
        // SAFETY: Linux exposes thread-local errno through this pointer.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: the directory stream remains live and exclusively consumed.
        let entry = unsafe { nix::libc::readdir(stream.0) };
        if entry.is_null() {
            // SAFETY: read immediately after readdir on this thread.
            let errno = unsafe { *nix::libc::__errno_location() };
            if errno != 0 {
                return Err(io::Error::from_raw_os_error(errno));
            }
            break;
        }
        // SAFETY: d_name is NUL-terminated for a successful readdir result and
        // remains valid until the next call.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        if entries.len() == MAX_PRIVATE_STAGE_ENTRIES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("private stage exceeds {MAX_PRIVATE_STAGE_ENTRIES} entries"),
            ));
        }
        total_name_bytes = total_name_bytes
            .checked_add(name.to_bytes().len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "private stage name budget overflow"))?;
        if total_name_bytes > MAX_PRIVATE_STAGE_NAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("private stage names exceed {MAX_PRIVATE_STAGE_NAME_BYTES} bytes"),
            ));
        }
        entries.push(OsStr::from_bytes(name.to_bytes()).to_owned());
    }
    Ok(entries)
}

struct DirectoryLock<'a> {
    directory: &'a std::fs::File,
}

impl<'a> DirectoryLock<'a> {
    fn try_exclusive(directory: &'a std::fs::File) -> io::Result<Option<Self>> {
        loop {
            // SAFETY: flock only borrows the live descriptor.
            if unsafe { nix::libc::flock(directory.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) } == 0 {
                return Ok(Some(Self { directory }));
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(None),
                _ => return Err(error),
            }
        }
    }

    fn exclusive_until(directory: &'a std::fs::File, deadline: Instant) -> io::Result<Self> {
        loop {
            if let Some(lock) = Self::try_exclusive(directory)? {
                return Ok(lock);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("cache publication lock exceeded {PUBLICATION_LOCK_TIMEOUT:?}"),
                ));
            }
            std::thread::sleep(PUBLICATION_LOCK_RETRY.min(deadline.saturating_duration_since(now)));
        }
    }
}

async fn lock_directory_async(directory: &std::fs::File) -> io::Result<DirectoryLock<'_>> {
    let deadline = Instant::now() + PUBLICATION_LOCK_TIMEOUT;
    loop {
        if let Some(lock) = DirectoryLock::try_exclusive(directory)? {
            return Ok(lock);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("cache publication lock exceeded {PUBLICATION_LOCK_TIMEOUT:?}"),
            ));
        }
        tokio::time::sleep(PUBLICATION_LOCK_RETRY.min(deadline.saturating_duration_since(now))).await;
    }
}

impl Drop for DirectoryLock<'_> {
    fn drop(&mut self) {
        // SAFETY: the descriptor remains live for this guard's lifetime.
        let _ = unsafe { nix::libc::flock(self.directory.as_raw_fd(), nix::libc::LOCK_UN) };
    }
}
