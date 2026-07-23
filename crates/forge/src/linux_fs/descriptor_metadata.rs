#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InodeIdentity {
    device: u64,
    inode: u64,
}

/// Attempt one mode change on the exact inode retained by an `O_PATH`
/// descriptor.
///
/// This is the deliberately unreconciled effect adapter for callers which
/// must classify the result from fresh semantic evidence. The procfs fd table
/// and descriptor alias are authenticated before the effect, but the
/// `fchmodat(2)` result is returned directly: an interrupted call is not
/// retried and success is not interpreted here.
pub(crate) fn chmod_path_descriptor_once(file: &std::fs::File, mode: u32) -> io::Result<()> {
    if mode & !0o7777 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("filesystem mode is outside the canonical 07777 mask: {mode:#o}"),
        ));
    }

    let (descriptors, descriptor, _expected) = authenticated_descriptor_name(file)?;
    // SAFETY: the directory is authenticated procfs, the component is the
    // live target descriptor's canonical decimal number, and flags=0
    // deliberately follows that procfs magic link to the pinned inode.
    if unsafe { nix::libc::fchmodat(descriptors.as_raw_fd(), descriptor.as_ptr(), mode, 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Change the mode of the exact inode retained by an `O_PATH` descriptor.
///
/// This intentionally uses only Linux 5.6-era interfaces.  `/proc`, the
/// current process directory, and its `fd` directory are all authenticated as
/// procfs before `fchmodat(2)` is allowed to follow the descriptor magic link.
/// The retained descriptor is revalidated after the call so success means the
/// same inode has exactly the requested mode.
pub(crate) fn chmod_path_descriptor(file: &std::fs::File, mode: u32) -> io::Result<()> {
    chmod_path_descriptor_with_deadline(file, mode, None)
}

/// Deadline-aware form used by finite frozen-root materialization.
pub(crate) fn chmod_path_descriptor_until(file: &std::fs::File, mode: u32, deadline: Instant) -> io::Result<()> {
    chmod_path_descriptor_with_deadline(file, mode, Some(deadline))
}

fn chmod_path_descriptor_with_deadline(file: &std::fs::File, mode: u32, deadline: Option<Instant>) -> io::Result<()> {
    if mode & !0o7777 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("filesystem mode is outside the canonical 07777 mask: {mode:#o}"),
        ));
    }

    let (descriptors, descriptor, expected) = authenticated_descriptor_name_with_deadline(file, deadline)?;
    retry_interrupted(deadline, || {
        // SAFETY: the directory is authenticated procfs, the component is the
        // live target descriptor's canonical decimal number, and flags=0
        // deliberately follows that procfs magic link to the pinned inode.
        if unsafe { nix::libc::fchmodat(descriptors.as_raw_fd(), descriptor.as_ptr(), mode, 0) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;

    let metadata = file.metadata()?;
    let actual = inode_identity(&metadata);
    require_same_inode(expected, actual)?;
    let actual_mode = metadata.permissions().mode() & 0o7777;
    if actual_mode != mode {
        return Err(io::Error::other(format!(
            "retained filesystem capability has mode {actual_mode:04o} after chmod, expected {mode:04o}"
        )));
    }
    let post_alias = open_descriptor_alias_with_deadline(&descriptors, &descriptor, deadline)?;
    require_same_inode(expected, inode_identity(&post_alias.metadata()?))?;
    Ok(())
}

/// Set access and modification times on the exact inode retained by an
/// `O_PATH` descriptor.
///
/// Linux `utimensat(2)` with `AT_EMPTY_PATH` operates on the descriptor
/// itself, including mode-000 files and symlinks opened with
/// `O_PATH | O_NOFOLLOW`.  There is deliberately no pathname fallback: an
/// older or incompatible kernel must fail instead of resolving a mutable name.
#[cfg(test)]
pub(crate) fn set_path_descriptor_times(file: &std::fs::File, seconds: i64, nanoseconds: i64) -> io::Result<()> {
    set_path_descriptor_times_with_deadline(file, seconds, nanoseconds, None)
}

/// Deadline-aware form used by finite frozen-root materialization.
pub(crate) fn set_path_descriptor_times_until(
    file: &std::fs::File,
    seconds: i64,
    nanoseconds: i64,
    deadline: Instant,
) -> io::Result<()> {
    set_path_descriptor_times_with_deadline(file, seconds, nanoseconds, Some(deadline))
}

fn set_path_descriptor_times_with_deadline(
    file: &std::fs::File,
    seconds: i64,
    nanoseconds: i64,
    deadline: Option<Instant>,
) -> io::Result<()> {
    let seconds = nix::libc::time_t::try_from(seconds)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "timestamp is outside time_t"))?;
    let nanoseconds = nix::libc::c_long::try_from(nanoseconds)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "timestamp nanoseconds are outside c_long"))?;
    if !(0..1_000_000_000).contains(&nanoseconds) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "timestamp nanoseconds are outside 0..1_000_000_000",
        ));
    }

    let before = file.metadata()?;
    let expected = inode_identity(&before);
    let kind = before.mode() & nix::libc::S_IFMT;
    if !matches!(kind, nix::libc::S_IFREG | nix::libc::S_IFDIR | nix::libc::S_IFLNK) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "descriptor timestamp target is not a regular file, directory, or symlink",
        ));
    }
    let timestamp = nix::libc::timespec {
        tv_sec: seconds,
        tv_nsec: nanoseconds,
    };
    let times = [timestamp, timestamp];
    let flags = nix::libc::AT_EMPTY_PATH
        | if kind == nix::libc::S_IFLNK {
            nix::libc::AT_SYMLINK_NOFOLLOW
        } else {
            0
        };

    retry_interrupted(deadline, || {
        // SAFETY: `file` is live, the empty path is NUL-terminated, and
        // `times` contains the two initialized timespec values required by
        // Linux. AT_EMPTY_PATH binds the mutation to the retained descriptor.
        if unsafe { nix::libc::utimensat(file.as_raw_fd(), c"".as_ptr(), times.as_ptr(), flags) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;

    let after = file.metadata()?;
    require_same_inode(expected, inode_identity(&after))?;
    if after.mode() & nix::libc::S_IFMT != kind {
        return Err(io::Error::other("retained timestamp capability changed inode type"));
    }
    if after.atime() != i64::from(seconds)
        || after.atime_nsec() != i64::from(nanoseconds)
        || after.mtime() != i64::from(seconds)
        || after.mtime_nsec() != i64::from(nanoseconds)
    {
        return Err(io::Error::other(format!(
            "retained timestamp capability has atime {}.{:09} and mtime {}.{:09}, expected {}.{:09}",
            after.atime(),
            after.atime_nsec(),
            after.mtime(),
            after.mtime_nsec(),
            seconds,
            nanoseconds,
        )));
    }
    Ok(())
}

/// Reopen one retained regular inode for side-effect-free content reads.
///
/// The only followed name is the descriptor magic link below this thread's
/// authenticated procfs fd table.  This avoids reopening a mutable package
/// pathname as a FIFO, device, or replacement regular file. `O_NOATIME` is
/// mandatory so a final digest cannot perturb the timestamp witness it is
/// meant to authenticate; freshly materialized package files are required to
/// be owned by the effective user, so lack of permission fails closed.
pub(crate) fn open_path_descriptor_readonly(file: &std::fs::File) -> io::Result<std::fs::File> {
    open_path_descriptor_readonly_with_deadline(file, None)
}

/// Deadline-aware form used by finite frozen-root materialization.
pub(crate) fn open_path_descriptor_readonly_until(
    file: &std::fs::File,
    deadline: Instant,
) -> io::Result<std::fs::File> {
    open_path_descriptor_readonly_with_deadline(file, Some(deadline))
}

fn open_path_descriptor_readonly_with_deadline(
    file: &std::fs::File,
    deadline: Option<Instant>,
) -> io::Result<std::fs::File> {
    let metadata = file.metadata()?;
    // SAFETY: geteuid takes no arguments and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_file() || metadata.uid() != effective_owner {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "readable descriptor target is not a same-owner regular inode",
        ));
    }

    let (descriptors, descriptor, expected) = authenticated_descriptor_name_with_deadline(file, deadline)?;
    let readable = openat2_file_with_deadline(
        descriptors.as_raw_fd(),
        &descriptor,
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NONBLOCK | nix::libc::O_NOATIME,
        0,
        0,
        deadline,
    )?;
    let readable_metadata = readable.metadata()?;
    require_same_inode(expected, inode_identity(&readable_metadata))?;
    if !readable_metadata.file_type().is_file() || readable_metadata.uid() != effective_owner {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "authenticated descriptor alias is not the expected same-owner regular inode",
        ));
    }
    require_same_inode(expected, inode_identity(&file.metadata()?))?;
    let post_alias = open_descriptor_alias_with_deadline(&descriptors, &descriptor, deadline)?;
    require_same_inode(expected, inode_identity(&post_alias.metadata()?))?;
    Ok(readable)
}
