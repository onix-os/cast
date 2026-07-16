fn open_descriptor_alias(descriptors: &std::fs::File, descriptor: &CStr) -> io::Result<std::fs::File> {
    open_descriptor_alias_with_deadline(descriptors, descriptor, None)
}

fn open_descriptor_alias_with_deadline(
    descriptors: &std::fs::File,
    descriptor: &CStr,
    deadline: Option<Instant>,
) -> io::Result<std::fs::File> {
    // This is the single intentional magic-link resolution. The parent has
    // already been pinned and authenticated as this thread's procfs fd table.
    openat2_file_with_deadline(
        descriptors.as_raw_fd(),
        descriptor,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC,
        0,
        0,
        deadline,
    )
}

fn proc_thread_self_components_with_deadline(
    proc: &std::fs::File,
    deadline: Option<Instant>,
) -> io::Result<(CString, CString)> {
    let mut bytes = [0_u8; MAX_THREAD_SELF_BYTES + 1];
    let length = retry_interrupted(deadline, || {
        // SAFETY: proc is a live authenticated directory, `thread-self` is a
        // fixed NUL-terminated component, and bytes is writable for its size.
        let length = unsafe {
            nix::libc::readlinkat(
                proc.as_raw_fd(),
                c"thread-self".as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if length >= 0 {
            usize::try_from(length).map_err(|_| io::Error::other("negative procfs self length"))
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    parse_thread_self(&bytes[..length])
}

fn parse_thread_self(bytes: &[u8]) -> io::Result<(CString, CString)> {
    if bytes.is_empty() || bytes.len() > MAX_THREAD_SELF_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "authenticated procfs thread-self link exceeds its canonical bound",
        ));
    }
    let mut components = bytes.split(|byte| *byte == b'/');
    let process = components.next().unwrap_or_default();
    let task = components.next().unwrap_or_default();
    let thread = components.next().unwrap_or_default();
    if task != b"task" || components.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "authenticated procfs thread-self link is not canonical <pid>/task/<tid>",
        ));
    }
    let process_id = parse_decimal_pid(process)?;
    let thread_id = parse_decimal_pid(thread)?;
    let expected_thread = current_thread_id()?;
    if process_id != std::process::id() || thread_id != expected_thread {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "authenticated procfs thread-self link names {process_id}/task/{thread_id}, expected {}/task/{expected_thread}",
                std::process::id()
            ),
        ));
    }
    Ok((
        CString::new(process).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "procfs PID contains NUL"))?,
        CString::new(thread).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "procfs TID contains NUL"))?,
    ))
}

fn current_thread_id() -> io::Result<u32> {
    // SAFETY: gettid has no arguments or memory-safety preconditions.
    let result = unsafe { nix::libc::syscall(nix::libc::SYS_gettid) };
    if result <= 0 {
        return Err(if result == -1 {
            io::Error::last_os_error()
        } else {
            io::Error::other(format!("gettid returned invalid value {result}"))
        });
    }
    u32::try_from(result).map_err(|_| io::Error::other(format!("gettid returned oversized value {result}")))
}

fn parse_decimal_pid(bytes: &[u8]) -> io::Result<u32> {
    if bytes.is_empty()
        || bytes.len() > MAX_DECIMAL_PID_BYTES
        || !bytes.iter().all(u8::is_ascii_digit)
        || bytes[0] == b'0'
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "authenticated procfs self link is not one bounded canonical decimal PID",
        ));
    }
    let value = bytes.iter().try_fold(0_u32, |value, digit| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(*digit - b'0')))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "authenticated procfs self PID exceeds u32"))
    })?;
    if value == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "authenticated procfs self PID must be nonzero",
        ));
    }
    Ok(value)
}

pub(crate) fn require_procfs(file: &std::fs::File, path: &Path) -> io::Result<()> {
    require_procfs_with_deadline(file, path, None)
}

fn require_procfs_with_deadline(file: &std::fs::File, path: &Path, deadline: Option<Instant>) -> io::Result<()> {
    // SAFETY: zeroed statfs storage is a valid output buffer and the file
    // descriptor remains live throughout fstatfs.
    let mut stat: nix::libc::statfs = unsafe { zeroed() };
    retry_interrupted(deadline, || {
        if unsafe { nix::libc::fstatfs(file.as_raw_fd(), &mut stat) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    if stat.f_type != PROC_SUPER_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "refusing unauthenticated procfs capability {}: expected filesystem magic {PROC_SUPER_MAGIC:#x}, found {:#x}",
                path.display(),
                stat.f_type
            ),
        ));
    }
    Ok(())
}

const MAX_PROC_FDINFO_BYTES: usize = 16 * 1024;

/// Read the mount ID for one retained descriptor from this thread's
/// authenticated procfs `fdinfo` entry.
///
/// Linux 5.6 does not expose `STATX_MNT_ID`. The numeric descriptor name is
/// therefore opened below the exact current-thread procfs directory, while
/// authenticated `/proc/<pid>/task/<tid>/fd` aliases sandwich the read so a
/// recycled or substituted descriptor can never be accepted.
pub(crate) fn descriptor_mount_id(file: &std::fs::File) -> io::Result<u64> {
    let (_descriptors, _descriptor, before) = authenticated_descriptor_name(file)?;
    let thread = authenticated_current_thread_procfs()?;
    let fdinfo_directory = openat2_file(
        thread.as_raw_fd(),
        c"fdinfo",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_procfs(&fdinfo_directory, Path::new("/proc/<pid>/task/<tid>/fdinfo"))?;

    let descriptor = CString::new(file.as_raw_fd().to_string()).expect("numeric descriptor contains no NUL");
    let mut fdinfo = openat2_file(
        fdinfo_directory.as_raw_fd(),
        &descriptor,
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_procfs(&fdinfo, Path::new("/proc/<pid>/task/<tid>/fdinfo/<fd>"))?;
    let mut bytes = Vec::with_capacity(512);
    fdinfo
        .by_ref()
        .take((MAX_PROC_FDINFO_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let mount_id = parse_descriptor_mount_id(&bytes)?;

    let (_descriptors, _descriptor, after) = authenticated_descriptor_name(file)?;
    require_same_inode(before, after)?;
    Ok(mount_id)
}

pub(crate) fn parse_descriptor_mount_id(bytes: &[u8]) -> io::Result<u64> {
    if bytes.is_empty() || bytes.len() > MAX_PROC_FDINFO_BYTES || bytes.last() != Some(&b'\n') || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "procfs fdinfo is empty, oversized, unterminated, or contains NUL",
        ));
    }

    let mut found = None;
    for line in bytes.split(|byte| *byte == b'\n') {
        if !line.starts_with(b"mnt_id:") {
            continue;
        }
        let digits = line.strip_prefix(b"mnt_id:\t").ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "procfs fdinfo mount ID has noncanonical spacing")
        })?;
        if digits.is_empty()
            || digits.len() > 20
            || !digits.iter().all(u8::is_ascii_digit)
            || digits[0] == b'0'
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "procfs fdinfo mount ID is not one canonical nonzero decimal u64",
            ));
        }
        let value = digits.iter().try_fold(0_u64, |value, digit| {
            value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(*digit - b'0')))
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "procfs fdinfo mount ID exceeds u64"))
        })?;
        if found.replace(value).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "procfs fdinfo contains duplicate mount IDs",
            ));
        }
    }

    found.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "procfs fdinfo does not contain a mount ID"))
}

/// Open one path through Linux 5.6 `openat2(2)` and return ownership of the
/// resulting descriptor.
///
/// Callers must choose an explicit resolution policy. Keeping the syscall in
/// one place prevents security-sensitive metadata stores from quietly falling
/// back to pathname traversal when `openat2` is unavailable.
pub(crate) fn openat2_file(
    dirfd: RawFd,
    path: &CStr,
    flags: i32,
    mode: u32,
    resolve: u64,
) -> io::Result<std::fs::File> {
    openat2_file_with_deadline(dirfd, path, flags, mode, resolve, None)
}

/// Deadline-aware form used by finite frozen-root materialization.
pub(crate) fn openat2_file_until(
    dirfd: RawFd,
    path: &CStr,
    flags: i32,
    mode: u32,
    resolve: u64,
    deadline: Instant,
) -> io::Result<std::fs::File> {
    openat2_file_with_deadline(dirfd, path, flags, mode, resolve, Some(deadline))
}

fn openat2_file_with_deadline(
    dirfd: RawFd,
    path: &CStr,
    flags: i32,
    mode: u32,
    resolve: u64,
    deadline: Option<Instant>,
) -> io::Result<std::fs::File> {
    // SAFETY: zero is valid for every public open_how field.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    let descriptor = retry_interrupted(deadline, || {
        // SAFETY: the descriptor, C string, and open_how remain live. Success
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
        if descriptor != -1 {
            Ok(descriptor)
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    let descriptor = i32::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned this fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(std::fs::File::from(descriptor))
}

/// Descriptor-relative resolution which cannot escape the retained directory,
/// follow links, or cross onto another mount.
pub(crate) fn controlled_resolution() -> u64 {
    (nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV) as u64
}
