fn sealed_archive_snapshot(
    mut source: File,
    expected: &str,
    limit: u64,
    deadline: ArchiveDeadline,
) -> Result<File, Error> {
    let descriptor = unsafe {
        libc::memfd_create(
            c"cast-locked-archive".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if descriptor == -1 {
        return Err(Error::DescriptorOperation {
            operation: "create sealed archive snapshot",
            source: io::Error::last_os_error(),
        });
    }
    let mut snapshot = unsafe { File::from_raw_fd(descriptor) };
    source.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    loop {
        deadline.checkpoint()?;
        let found = source.read(&mut buffer)?;
        deadline.checkpoint()?;
        if found == 0 {
            break;
        }
        total = total.checked_add(found as u64).ok_or(Error::ArithmeticOverflow)?;
        require_limit("compressed archive bytes", total, limit)?;
        snapshot.write_all(&buffer[..found])?;
        hasher.update(&buffer[..found]);
    }
    if hex::encode(hasher.finalize()) != expected {
        return Err(Error::ArchiveDigestMismatch);
    }
    if unsafe { libc::fchmod(snapshot.as_raw_fd(), 0o400) } == -1 {
        return Err(io::Error::last_os_error().into());
    }
    let required_seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if unsafe { libc::fcntl(snapshot.as_raw_fd(), libc::F_ADD_SEALS, required_seals) } == -1 {
        return Err(Error::DescriptorOperation {
            operation: "seal immutable archive snapshot",
            source: io::Error::last_os_error(),
        });
    }
    let found_seals = unsafe { libc::fcntl(snapshot.as_raw_fd(), libc::F_GET_SEALS) };
    if found_seals == -1 {
        return Err(Error::DescriptorOperation {
            operation: "verify immutable archive snapshot seals",
            source: io::Error::last_os_error(),
        });
    }
    if found_seals & required_seals != required_seals {
        return Err(Error::ArchiveSnapshotNotSealed {
            expected: required_seals,
            found: found_seals,
        });
    }
    snapshot.seek(SeekFrom::Start(0))?;
    Ok(snapshot)
}

fn validate_archive_file(file: &File, limits: ArchiveLimits) -> Result<(), Error> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(Error::ArchiveNotRegular);
    }
    require_limit("compressed archive bytes", metadata.len(), limits.compressed_bytes)
}

fn set_file_mode_and_time(file: &File, mode: u32, source_date_epoch: i64) -> Result<(), Error> {
    let result = unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) };
    if result == -1 {
        return Err(io::Error::last_os_error().into());
    }
    let timestamp = filetime::FileTime::from_unix_time(source_date_epoch, 0);
    filetime::set_file_handle_times(file, Some(timestamp), Some(timestamp))?;
    Ok(())
}

fn open_directory(path: &Path, operation: &'static str) -> Result<File, Error> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| Error::DescriptorOperation { operation, source })?;
    validate_directory(&file, operation)?;
    Ok(file)
}

fn validate_directory(file: &File, operation: &'static str) -> Result<(), Error> {
    if file.metadata()?.file_type().is_dir() {
        Ok(())
    } else {
        Err(Error::NotDirectory { operation })
    }
}

fn open_regular_beneath(root: &File, components: &[Vec<u8>], operation: &'static str) -> Result<File, Error> {
    let relative = cstring_path(components)?;
    let fd = openat2(
        root.as_raw_fd(),
        &relative,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )
    .map_err(|source| Error::DescriptorOperation { operation, source })?;
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.file_type().is_file() {
        return Err(Error::ArchiveNotRegular);
    }
    Ok(file)
}

fn open_directory_beneath(root: &File, components: &[Vec<u8>], operation: &'static str) -> Result<File, Error> {
    if components.is_empty() {
        return root.try_clone().map_err(Error::from);
    }
    let relative = cstring_path(components)?;
    let fd = openat2(
        root.as_raw_fd(),
        &relative,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )
    .map_err(|source| Error::DescriptorOperation { operation, source })?;
    let file = unsafe { File::from_raw_fd(fd) };
    validate_directory(&file, operation)?;
    Ok(file)
}

fn ensure_directories(root: &File, components: &[Vec<u8>]) -> Result<File, Error> {
    let mut current = root.try_clone()?;
    for component in components {
        let component = CString::new(component.as_slice()).map_err(|_| Error::InteriorNul)?;
        let created = mkdirat_directory(&current, &component, 0o700, "create archive directory")?;
        let path = openat2(
            current.as_raw_fd(),
            &component,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        )
        .map_err(|source| Error::DescriptorOperation {
            operation: "pin archive directory",
            source,
        })?;
        let path = unsafe { File::from_raw_fd(path) };
        validate_directory(&path, "pin archive directory")?;
        current = open_and_normalize_archive_directory(&current, &component, &path, created, "open archive directory")?;
    }
    Ok(current)
}

fn mkdirat_directory(parent: &File, name: &CStr, mode: u32, operation: &'static str) -> Result<bool, Error> {
    loop {
        // SAFETY: `parent` and the single NUL-terminated component remain
        // live; mkdirat never follows the final component.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode) } == 0 {
            return Ok(true);
        }
        let source = io::Error::last_os_error();
        match source.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::AlreadyExists => return Ok(false),
            _ => return Err(Error::DescriptorOperation { operation, source }),
        }
    }
}
fn open_and_normalize_archive_directory(
    parent: &File,
    name: &CStr,
    pinned: &File,
    created: bool,
    operation: &'static str,
) -> Result<File, Error> {
    let open_readable = || {
        openat2(
            parent.as_raw_fd(),
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )
    };
    let descriptor = match open_readable() {
        Ok(descriptor) => descriptor,
        Err(source) if created && source.kind() == io::ErrorKind::PermissionDenied => {
            // A hostile umask can remove every access bit from a directory we
            // just created. Only that exact same-owner O_PATH-pinned inode may
            // use the authenticated task-local procfs recovery path. An
            // unreadable pre-existing directory is evidence and fails
            // unchanged instead of being chmod-laundered.
            require_recoverable_created_directory(pinned, operation)?;
            #[cfg(test)]
            TEST_PROC_CHMOD_RECOVERIES.with(|recoveries| recoveries.set(recoveries.get() + 1));
            chmod_path_descriptor(pinned, 0o700).map_err(|source| Error::DescriptorOperation {
                operation: "recover unreadable newly-created archive directory",
                source,
            })?;
            open_readable().map_err(|source| Error::DescriptorOperation { operation, source })?
        }
        Err(source) => return Err(Error::DescriptorOperation { operation, source }),
    };
    let directory = unsafe { File::from_raw_fd(descriptor) };
    validate_directory(&directory, operation)?;
    require_same_directory(pinned, &directory, operation)?;
    fchmod_directory(&directory, 0o700, operation)?;
    Ok(directory)
}

fn require_recoverable_created_directory(file: &File, operation: &'static str) -> Result<(), Error> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    // SAFETY: geteuid has no memory-safety preconditions.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.file_type().is_dir() && metadata.uid() == effective_uid && mode & !0o700 == 0 {
        Ok(())
    } else {
        Err(Error::DescriptorOperation {
            operation,
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "new archive directory is not a recoverable same-owner mkdir residue (uid={}, mode={mode:04o})",
                    metadata.uid()
                ),
            ),
        })
    }
}

fn require_same_directory(pinned: &File, readable: &File, operation: &'static str) -> Result<(), Error> {
    let pinned = pinned.metadata()?;
    let readable = readable.metadata()?;
    if pinned.file_type().is_dir()
        && readable.file_type().is_dir()
        && (pinned.dev(), pinned.ino()) == (readable.dev(), readable.ino())
    {
        Ok(())
    } else {
        Err(Error::DescriptorOperation {
            operation,
            source: io::Error::other("readable archive directory does not match its retained O_PATH inode"),
        })
    }
}

fn fchmod_directory(file: &File, mode: u32, operation: &'static str) -> Result<(), Error> {
    loop {
        // SAFETY: `file` is a live readable directory descriptor and the mode
        // is restricted to ordinary permission bits.
        if unsafe { libc::fchmod(file.as_raw_fd(), mode) } == 0 {
            break;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(Error::DescriptorOperation { operation, source });
        }
    }
    let actual = file.metadata()?.mode() & 0o7777;
    if actual == mode {
        Ok(())
    } else {
        Err(Error::DescriptorOperation {
            operation,
            source: io::Error::other(format!(
                "archive directory mode is {actual:04o} after fchmod, expected {mode:04o}"
            )),
        })
    }
}

fn normalize_destination_parents(root: &File, components: &[Vec<u8>], source_date_epoch: i64) -> Result<(), Error> {
    for depth in (1..=components.len()).rev() {
        let directory = open_directory_beneath(root, &components[..depth], "archive destination parent")?;
        set_file_mode_and_time(&directory, 0o755, source_date_epoch)?;
        directory.sync_all()?;
    }
    Ok(())
}

fn create_regular_beneath(root: &File, path: &[Vec<u8>], source_date_epoch: i64) -> Result<File, Error> {
    inject_test_stage_write_failure()?;
    let (name, parents) = path.split_last().ok_or(Error::UnsafeInternalPath)?;
    let parent = ensure_directories(root, parents)?;
    let name = CString::new(name.as_slice()).map_err(|_| Error::InteriorNul)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd == -1 {
        return Err(Error::DescriptorOperation {
            operation: "create extracted regular file",
            source: io::Error::last_os_error(),
        });
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let timestamp = filetime::FileTime::from_unix_time(source_date_epoch, 0);
    filetime::set_file_handle_times(&file, Some(timestamp), Some(timestamp))?;
    Ok(file)
}

#[cfg(test)]
std::thread_local! {
    static TEST_STAGE_WRITES_BEFORE_FAILURE: std::cell::Cell<u64> = const { std::cell::Cell::new(u64::MAX) };
    static TEST_FAIL_STAGE_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TEST_FAIL_PUBLISH_AFTER_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TEST_PROC_CHMOD_RECOVERIES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn inject_test_stage_write_failure() -> Result<(), Error> {
    TEST_STAGE_WRITES_BEFORE_FAILURE.with(|remaining| {
        let value = remaining.get();
        if value == 0 {
            Err(io::Error::other("injected archive stage write failure").into())
        } else {
            remaining.set(value - 1);
            Ok(())
        }
    })
}

#[cfg(test)]
fn inject_test_stage_open_failure() -> Result<(), Error> {
    TEST_FAIL_STAGE_OPEN.with(|fail| {
        if fail.replace(false) {
            Err(io::Error::other("injected private archive stage open failure").into())
        } else {
            Ok(())
        }
    })
}

#[cfg(test)]
fn inject_test_publish_failure_after_rename() -> Result<(), Error> {
    TEST_FAIL_PUBLISH_AFTER_RENAME.with(|fail| {
        if fail.replace(false) {
            Err(io::Error::other("injected archive publication durability failure").into())
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
fn inject_test_stage_write_failure() -> Result<(), Error> {
    Ok(())
}

#[cfg(not(test))]
fn inject_test_stage_open_failure() -> Result<(), Error> {
    Ok(())
}

#[cfg(not(test))]
fn inject_test_publish_failure_after_rename() -> Result<(), Error> {
    Ok(())
}

fn create_symlink_beneath(root: &File, target: &[u8], path: &[Vec<u8>], source_date_epoch: i64) -> Result<(), Error> {
    let (name, parents) = path.split_last().ok_or(Error::UnsafeInternalPath)?;
    let parent = ensure_directories(root, parents)?;
    let name = CString::new(name.as_slice()).map_err(|_| Error::InteriorNul)?;
    let target = CString::new(target).map_err(|_| Error::InteriorNul)?;
    let result = unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), name.as_ptr()) };
    if result == -1 {
        return Err(Error::DescriptorOperation {
            operation: "create extracted symlink",
            source: io::Error::last_os_error(),
        });
    }
    let timestamp = libc::timespec {
        tv_sec: source_date_epoch as libc::time_t,
        tv_nsec: 0,
    };
    let times = [timestamp, timestamp];
    let result = unsafe {
        libc::utimensat(
            parent.as_raw_fd(),
            name.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == -1 {
        Err(Error::DescriptorOperation {
            operation: "normalize extracted symlink timestamp",
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

fn create_hardlink_beneath(root: &File, target: &[Vec<u8>], path: &[Vec<u8>]) -> Result<(), Error> {
    let (target_name, target_parents) = target.split_last().ok_or(Error::UnsafeInternalPath)?;
    let target_parent = open_directory_beneath(root, target_parents, "open hardlink target parent")?;
    let (name, parents) = path.split_last().ok_or(Error::UnsafeInternalPath)?;
    let parent = ensure_directories(root, parents)?;
    let target_name = CString::new(target_name.as_slice()).map_err(|_| Error::InteriorNul)?;
    let name = CString::new(name.as_slice()).map_err(|_| Error::InteriorNul)?;
    let result = unsafe {
        libc::linkat(
            target_parent.as_raw_fd(),
            target_name.as_ptr(),
            parent.as_raw_fd(),
            name.as_ptr(),
            0,
        )
    };
    if result == -1 {
        Err(Error::DescriptorOperation {
            operation: "create extracted hardlink",
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

fn cstring_path(components: &[Vec<u8>]) -> Result<CString, Error> {
    CString::new(join_components(components)).map_err(|_| Error::InteriorNul)
}

fn join_components(components: &[Vec<u8>]) -> Vec<u8> {
    let length = components.iter().map(Vec::len).sum::<usize>() + components.len().saturating_sub(1);
    let mut joined = Vec::with_capacity(length);
    for (index, component) in components.iter().enumerate() {
        if index != 0 {
            joined.push(b'/');
        }
        joined.extend_from_slice(component);
    }
    joined
}

fn split_joined_components(path: &[u8]) -> Vec<Vec<u8>> {
    path.split(|byte| *byte == b'/').map(<[u8]>::to_vec).collect()
}

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

const RESOLVE_NO_XDEV: u64 = 0x01;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;

fn openat2(parent: RawFd, path: &CStr, flags: i32, mode: libc::mode_t) -> io::Result<RawFd> {
    let how = OpenHow {
        flags: flags as u64,
        mode: mode as u64,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV,
    };
    loop {
        let result = unsafe { libc::syscall(libc::SYS_openat2, parent, path.as_ptr(), &how, size_of::<OpenHow>()) };
        if result != -1 {
            return RawFd::try_from(result)
                .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {result}")));
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}
