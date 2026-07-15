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

#[cfg(test)]
fn require_procfs(file: &std::fs::File, path: &Path) -> io::Result<()> {
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
                "refusing unauthenticated descriptor chmod through {}: expected procfs magic {PROC_SUPER_MAGIC:#x}, found {:#x}",
                path.display(),
                stat.f_type
            ),
        ));
    }
    Ok(())
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
