// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Linux 5.6-compatible operations on retained filesystem capabilities.
//!
//! `O_PATH` descriptors deliberately cannot be passed to `fchmod(2)`, and
//! `fchmodat2(2)` did not exist in Linux 5.6.  The only path resolved here is
//! the live decimal descriptor name below an authenticated procfs instance.

use std::{
    ffi::{CStr, CString},
    io,
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd},
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::Path,
};

const PROC_SUPER_MAGIC: nix::libc::c_long = 0x0000_9fa0;
const POSIX_ACCESS_ACL_XATTR: &CStr = c"system.posix_acl_access";
const POSIX_DEFAULT_ACL_XATTR: &CStr = c"system.posix_acl_default";
const MAX_DECIMAL_PID_BYTES: usize = 16;
const MAX_THREAD_SELF_BYTES: usize = MAX_DECIMAL_PID_BYTES * 2 + 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InodeIdentity {
    device: u64,
    inode: u64,
}

/// Change the mode of the exact inode retained by an `O_PATH` descriptor.
///
/// This intentionally uses only Linux 5.6-era interfaces.  `/proc`, the
/// current process directory, and its `fd` directory are all authenticated as
/// procfs before `fchmodat(2)` is allowed to follow the descriptor magic link.
/// The retained descriptor is revalidated after the call so success means the
/// same inode has exactly the requested mode.
pub(crate) fn chmod_path_descriptor(file: &std::fs::File, mode: u32) -> io::Result<()> {
    if mode & !0o7777 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("filesystem mode is outside the canonical 07777 mask: {mode:#o}"),
        ));
    }

    let (descriptors, descriptor, expected) = authenticated_descriptor_name(file)?;
    loop {
        // SAFETY: the directory is authenticated procfs, the component is the
        // live target descriptor's canonical decimal number, and flags=0
        // deliberately follows that procfs magic link to the pinned inode.
        if unsafe { nix::libc::fchmodat(descriptors.as_raw_fd(), descriptor.as_ptr(), mode, 0) } == 0 {
            break;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }

    let metadata = file.metadata()?;
    let actual = inode_identity(&metadata);
    require_same_inode(expected, actual)?;
    let actual_mode = metadata.permissions().mode() & 0o7777;
    if actual_mode != mode {
        return Err(io::Error::other(format!(
            "retained filesystem capability has mode {actual_mode:04o} after chmod, expected {mode:04o}"
        )));
    }
    let post_alias = open_descriptor_alias(&descriptors, &descriptor)?;
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
pub(crate) fn set_path_descriptor_times(file: &std::fs::File, seconds: i64, nanoseconds: i64) -> io::Result<()> {
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

    loop {
        // SAFETY: `file` is live, the empty path is NUL-terminated, and
        // `times` contains the two initialized timespec values required by
        // Linux. AT_EMPTY_PATH binds the mutation to the retained descriptor.
        if unsafe { nix::libc::utimensat(file.as_raw_fd(), c"".as_ptr(), times.as_ptr(), flags) } == 0 {
            break;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }

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

/// Give an unnamed retained inode one no-replace name in an authenticated
/// target directory.
///
/// `linkat(AT_EMPTY_PATH)` requires `CAP_DAC_READ_SEARCH` even for the owner.
/// Following the exact descriptor name below this task's authenticated procfs
/// fd table is the documented unprivileged `O_TMPFILE` publication path. The
/// source alias and resulting target are both bound back to the retained inode.
pub(crate) fn link_path_descriptor_noreplace(
    file: &std::fs::File,
    target_directory: &std::fs::File,
    target_name: &CStr,
) -> io::Result<()> {
    if target_name.to_bytes().is_empty()
        || target_name.to_bytes().contains(&b'/')
        || matches!(target_name.to_bytes(), b"." | b"..")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "descriptor link target must be one nonempty component",
        ));
    }
    let source_metadata = file.metadata()?;
    if !source_metadata.file_type().is_file() || source_metadata.nlink() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "descriptor link source must be one unnamed regular inode",
        ));
    }

    let (descriptors, descriptor, expected) = authenticated_descriptor_name(file)?;
    loop {
        // SAFETY: both directory descriptors and names remain live. The source
        // parent is an authenticated procfs fd table and AT_SYMLINK_FOLLOW is
        // intentional: it follows only the proven descriptor magic link.
        if unsafe {
            nix::libc::linkat(
                descriptors.as_raw_fd(),
                descriptor.as_ptr(),
                target_directory.as_raw_fd(),
                target_name.as_ptr(),
                nix::libc::AT_SYMLINK_FOLLOW,
            )
        } == 0
        {
            break;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }

    let linked_metadata = file.metadata()?;
    require_same_inode(expected, inode_identity(&linked_metadata))?;
    if linked_metadata.nlink() != 1 {
        return Err(io::Error::other(format!(
            "descriptor link source has {} names after publication, expected exactly one",
            linked_metadata.nlink()
        )));
    }
    let post_alias = open_descriptor_alias(&descriptors, &descriptor)?;
    require_same_inode(expected, inode_identity(&post_alias.metadata()?))?;
    let target = openat2_file(
        target_directory.as_raw_fd(),
        target_name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    require_same_inode(expected, inode_identity(&target.metadata()?))
}

/// Move one exact directory entry between retained parents without replacing
/// any destination entry. Both names must be single components; callers keep
/// authority in the descriptors rather than mutable absolute pathnames.
pub(crate) fn renameat2_noreplace(
    source_directory: &std::fs::File,
    source_name: &CStr,
    destination_directory: &std::fs::File,
    destination_name: &CStr,
) -> io::Result<()> {
    for (role, name) in [("source", source_name), ("destination", destination_name)] {
        if name.to_bytes().is_empty() || name.to_bytes().contains(&b'/') || matches!(name.to_bytes(), b"." | b"..") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("descriptor-relative rename {role} must be one nonempty component"),
            ));
        }
    }

    loop {
        // SAFETY: both retained directory descriptors and both C strings stay
        // live for the syscall. RENAME_NOREPLACE prevents destination loss.
        if unsafe {
            nix::libc::syscall(
                nix::libc::SYS_renameat2,
                source_directory.as_raw_fd(),
                source_name.as_ptr(),
                destination_directory.as_raw_fd(),
                destination_name.as_ptr(),
                nix::libc::RENAME_NOREPLACE,
            )
        } == 0
        {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

/// Flush every pending write on the filesystem containing a retained
/// capability. This is intentionally broader than `fsync` on one directory:
/// failed-candidate preservation must not delete its database correlation
/// while trigger-created descendants are still only dirty cache state.
pub(crate) fn sync_filesystem(file: &std::fs::File) -> io::Result<()> {
    loop {
        // SAFETY: the retained descriptor remains live and identifies the
        // filesystem whose pending data and metadata must reach stable storage.
        if unsafe { nix::libc::syncfs(file.as_raw_fd()) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

fn authenticated_descriptor_name(file: &std::fs::File) -> io::Result<(std::fs::File, CString, InodeIdentity)> {
    let expected = inode_identity(&file.metadata()?);
    let proc = openat2_file(
        nix::libc::AT_FDCWD,
        c"/proc",
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )?;
    require_procfs(&proc, Path::new("/proc"))?;

    let (process_name, thread_name) = proc_thread_self_components(&proc)?;
    let process = openat2_file(
        proc.as_raw_fd(),
        &process_name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    require_procfs(&process, Path::new("/proc/<pid>"))?;

    let tasks = openat2_file(
        process.as_raw_fd(),
        c"task",
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    require_procfs(&tasks, Path::new("/proc/<pid>/task"))?;
    let thread = openat2_file(
        tasks.as_raw_fd(),
        &thread_name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    require_procfs(&thread, Path::new("/proc/<pid>/task/<tid>"))?;

    let descriptors = openat2_file(
        thread.as_raw_fd(),
        c"fd",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_procfs(&descriptors, Path::new("/proc/thread-self/fd"))?;

    let descriptor = CString::new(file.as_raw_fd().to_string()).expect("numeric descriptor contains no NUL");
    let alias = open_descriptor_alias(&descriptors, &descriptor)?;
    require_same_inode(expected, inode_identity(&alias.metadata()?))?;
    Ok((descriptors, descriptor, expected))
}

/// Reject an inheritable POSIX default ACL on an authenticated readable
/// directory descriptor.
///
/// Access-ACL write authority is represented by the group-class mode mask,
/// but a default ACL is not. Admitting one would let later children inherit
/// authority that an otherwise-safe directory mode does not reveal.
pub(crate) fn require_no_default_acl(file: &std::fs::File, path: &Path) -> io::Result<()> {
    require_no_acl_xattr(file, path, POSIX_DEFAULT_ACL_XATTR, "inheritable POSIX default")
}

/// Reject an explicit POSIX access ACL on an authenticated readable inode.
///
/// The synthesized empty live `/usr` baseline requires a canonical mode-only
/// authority model. Existing trees continue to rely on the mode mask plus the
/// separate default-ACL check above.
pub(crate) fn require_no_access_acl(file: &std::fs::File, path: &Path) -> io::Result<()> {
    require_no_acl_xattr(file, path, POSIX_ACCESS_ACL_XATTR, "POSIX access")
}

fn require_no_acl_xattr(file: &std::fs::File, path: &Path, name: &CStr, role: &'static str) -> io::Result<()> {
    loop {
        // SAFETY: `file` and the supplied static xattr name remain live. A null value
        // with size zero is the documented existence/size query and does not
        // copy attribute bytes into userspace.
        let result = unsafe { nix::libc::fgetxattr(file.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if result >= 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("capability inode carries a {role} ACL: {}", path.display()),
            ));
        }

        let source = io::Error::last_os_error();
        match source.raw_os_error() {
            Some(nix::libc::EINTR) => {}
            Some(nix::libc::ENODATA) | Some(nix::libc::EOPNOTSUPP) => return Ok(()),
            _ => return Err(source),
        }
    }
}

/// Pin and normalize one freshly-created directory without chmodding its
/// mutable pathname.
///
/// Callers must have just created `path` with a maximum mode of `mode`. The
/// owner/subset check prevents a privileged caller from laundering a raced-in
/// directory owned by another user. The public name is reopened after chmod so
/// a replacement cannot be reported as the normalized temporary root.
pub(crate) fn normalize_new_directory(path: &Path, mode: u32) -> io::Result<std::fs::File> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "new directory normalization requires an absolute path",
        ));
    }
    if mode & !0o7777 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("filesystem mode is outside the canonical 07777 mask: {mode:#o}"),
        ));
    }

    let encoded = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "new directory path contains NUL"))?;
    let flags = nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW;
    let resolve = (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64;
    let pinned = openat2_file(nix::libc::AT_FDCWD, &encoded, flags, 0, resolve)?;
    require_new_directory_residue(&pinned, path, mode)?;
    let expected = inode_identity(&pinned.metadata()?);
    chmod_path_descriptor(&pinned, mode)?;

    let named = openat2_file(nix::libc::AT_FDCWD, &encoded, flags, 0, resolve)?;
    require_same_inode(expected, inode_identity(&named.metadata()?))?;
    require_exact_directory(&pinned, path, mode)?;
    require_exact_directory(&named, path, mode)?;
    Ok(pinned)
}

/// Prove that a pathname still denotes one retained normalized directory.
pub(crate) fn require_named_directory(path: &Path, retained: &std::fs::File, mode: u32) -> io::Result<()> {
    let encoded = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory path contains NUL"))?;
    let flags = nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW;
    let resolve = (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64;
    let expected = inode_identity(&retained.metadata()?);
    require_exact_directory(retained, path, mode)?;
    let named = openat2_file(nix::libc::AT_FDCWD, &encoded, flags, 0, resolve)?;
    require_same_inode(expected, inode_identity(&named.metadata()?))?;
    require_exact_directory(&named, path, mode)
}

fn require_new_directory_residue(file: &std::fs::File, path: &Path, requested_mode: u32) -> io::Result<()> {
    let metadata = file.metadata()?;
    let actual_mode = metadata.permissions().mode() & 0o7777;
    // SAFETY: geteuid takes no arguments and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    if metadata.file_type().is_dir() && metadata.uid() == effective_owner && actual_mode & !requested_mode == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "fresh directory is not a same-owner subset-mode residue: {} (uid={}, mode={actual_mode:04o})",
                path.display(),
                metadata.uid()
            ),
        ))
    }
}

fn require_exact_directory(file: &std::fs::File, path: &Path, expected_mode: u32) -> io::Result<()> {
    let metadata = file.metadata()?;
    let actual_mode = metadata.permissions().mode() & 0o7777;
    // SAFETY: geteuid takes no arguments and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    if metadata.file_type().is_dir() && metadata.uid() == effective_owner && actual_mode == expected_mode {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "directory is not the exact normalized same-owner inode: {} (uid={}, mode={actual_mode:04o})",
                path.display(),
                metadata.uid()
            ),
        ))
    }
}

fn inode_identity(metadata: &std::fs::Metadata) -> InodeIdentity {
    InodeIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn require_same_inode(expected: InodeIdentity, actual: InodeIdentity) -> io::Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "procfs descriptor alias does not identify the retained inode: expected ({}, {}), found ({}, {})",
            expected.device, expected.inode, actual.device, actual.inode
        )))
    }
}

fn open_descriptor_alias(descriptors: &std::fs::File, descriptor: &CStr) -> io::Result<std::fs::File> {
    // This is the single intentional magic-link resolution. The parent has
    // already been pinned and authenticated as this thread's procfs fd table.
    openat2_file(
        descriptors.as_raw_fd(),
        descriptor,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC,
        0,
        0,
    )
}

fn proc_thread_self_components(proc: &std::fs::File) -> io::Result<(CString, CString)> {
    let mut bytes = [0_u8; MAX_THREAD_SELF_BYTES + 1];
    let length = loop {
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
            break usize::try_from(length).map_err(|_| io::Error::other("negative procfs self length"))?;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    };
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

fn require_procfs(file: &std::fs::File, path: &Path) -> io::Result<()> {
    // SAFETY: zeroed statfs storage is a valid output buffer and the file
    // descriptor remains live throughout fstatfs.
    let mut stat: nix::libc::statfs = unsafe { zeroed() };
    loop {
        if unsafe { nix::libc::fstatfs(file.as_raw_fd(), &mut stat) } == 0 {
            break;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
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
    // SAFETY: zero is valid for every public open_how field.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    let descriptor = loop {
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
            break descriptor;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    };
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

#[cfg(test)]
mod tests {
    use std::{
        io::Write as _,
        os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
        process::Command,
    };

    use super::*;

    #[test]
    fn procfs_authentication_rejects_an_ordinary_filesystem() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = std::fs::File::open(temporary.path()).unwrap();

        let error = require_procfs(&directory, temporary.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn proc_pid_parser_accepts_only_bounded_canonical_decimal() {
        assert_eq!(parse_decimal_pid(b"1").unwrap(), 1);
        assert_eq!(parse_decimal_pid(b"4294967295").unwrap(), u32::MAX);
        for invalid in [b"".as_slice(), b"0", b"01", b"-1", b"1\n", b"4294967296"] {
            assert_eq!(
                parse_decimal_pid(invalid).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        assert_eq!(
            parse_decimal_pid(b"12345678901234567").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn thread_self_parser_requires_exact_current_process_and_thread() {
        let thread_id = current_thread_id().unwrap();
        let canonical = format!("{}/task/{thread_id}", std::process::id());
        let (process, thread) = parse_thread_self(canonical.as_bytes()).unwrap();
        assert_eq!(process.to_bytes(), std::process::id().to_string().as_bytes());
        assert_eq!(thread.to_bytes(), thread_id.to_string().as_bytes());

        for malformed in [
            format!("{}/{thread_id}", std::process::id()),
            format!("{}/task/01", std::process::id()),
            format!("{}/task/{thread_id}/extra", std::process::id()),
        ] {
            assert_eq!(
                parse_thread_self(malformed.as_bytes()).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn chmod_revalidates_the_exact_opath_inode_and_mode() {
        let temporary = tempfile::tempdir().unwrap();
        let path = CString::new(temporary.path().as_os_str().as_encoded_bytes()).unwrap();
        let retained = openat2_file(
            nix::libc::AT_FDCWD,
            &path,
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
        )
        .unwrap();
        let before = retained.metadata().unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o500)).unwrap();

        chmod_path_descriptor(&retained, 0o700).unwrap();

        let after = retained.metadata().unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!(after.permissions().mode() & 0o7777, 0o700);
    }

    #[test]
    fn descriptor_times_update_the_retained_regular_inode_not_its_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let named = temporary.path().join("named");
        let displaced = temporary.path().join("displaced");
        std::fs::write(&named, b"retained").unwrap();
        std::fs::set_permissions(&named, std::fs::Permissions::from_mode(0o000)).unwrap();
        let encoded = CString::new(named.as_os_str().as_encoded_bytes()).unwrap();
        let retained = openat2_file(
            nix::libc::AT_FDCWD,
            &encoded,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
        )
        .unwrap();
        std::fs::rename(&named, &displaced).unwrap();
        std::fs::write(&named, b"replacement").unwrap();
        filetime::set_file_times(
            &named,
            filetime::FileTime::from_unix_time(222, 0),
            filetime::FileTime::from_unix_time(222, 0),
        )
        .unwrap();

        set_path_descriptor_times(&retained, 123, 456_789).unwrap();

        let retained_metadata = std::fs::symlink_metadata(&displaced).unwrap();
        let replacement_metadata = std::fs::symlink_metadata(&named).unwrap();
        assert_eq!(retained_metadata.atime(), 123);
        assert_eq!(retained_metadata.atime_nsec(), 456_789);
        assert_eq!(retained_metadata.mtime(), 123);
        assert_eq!(retained_metadata.mtime_nsec(), 456_789);
        assert_eq!(replacement_metadata.atime(), 222);
        assert_eq!(replacement_metadata.mtime(), 222);
        std::fs::set_permissions(&displaced, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn descriptor_times_support_a_mode_zero_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("directory");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o000)).unwrap();
        let encoded = CString::new(directory.as_os_str().as_encoded_bytes()).unwrap();
        let retained = openat2_file(
            nix::libc::AT_FDCWD,
            &encoded,
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
        )
        .unwrap();

        set_path_descriptor_times(&retained, 321, 0).unwrap();

        let metadata = retained.metadata().unwrap();
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o000);
        assert_eq!((metadata.atime(), metadata.mtime()), (321, 321));
        chmod_path_descriptor(&retained, 0o700).unwrap();
    }

    #[test]
    fn descriptor_times_update_a_symlink_without_touching_its_target() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let link = temporary.path().join("link");
        std::fs::write(&target, b"outside sentinel").unwrap();
        filetime::set_file_times(
            &target,
            filetime::FileTime::from_unix_time(444, 0),
            filetime::FileTime::from_unix_time(444, 0),
        )
        .unwrap();
        symlink(&target, &link).unwrap();
        let encoded = CString::new(link.as_os_str().as_encoded_bytes()).unwrap();
        let retained = openat2_file(
            nix::libc::AT_FDCWD,
            &encoded,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
        )
        .unwrap();

        set_path_descriptor_times(&retained, 555, 123).unwrap();

        let link_metadata = std::fs::symlink_metadata(&link).unwrap();
        let target_metadata = std::fs::symlink_metadata(&target).unwrap();
        assert_eq!((link_metadata.atime(), link_metadata.atime_nsec()), (555, 123));
        assert_eq!((link_metadata.mtime(), link_metadata.mtime_nsec()), (555, 123));
        assert_eq!((target_metadata.atime(), target_metadata.mtime()), (444, 444));
        assert_eq!(std::fs::read(&target).unwrap(), b"outside sentinel");
    }

    #[test]
    fn authenticated_procfs_links_an_unnamed_inode_without_privilege() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let directory = std::fs::File::open(temporary.path()).unwrap();
        let anonymous = openat2_file(
            directory.as_raw_fd(),
            c".",
            nix::libc::O_TMPFILE | nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0o600,
            controlled_resolution(),
        )
        .unwrap();
        let before = anonymous.metadata().unwrap();
        assert_eq!(before.nlink(), 0);
        (&anonymous).write_all(b"retained inode").unwrap();
        anonymous.sync_all().unwrap();

        link_path_descriptor_noreplace(&anonymous, &directory, c"published").unwrap();

        let after = anonymous.metadata().unwrap();
        let named = std::fs::metadata(temporary.path().join("published")).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!((named.dev(), named.ino()), (before.dev(), before.ino()));
        assert_eq!(after.nlink(), 1);
        assert_eq!(
            std::fs::read(temporary.path().join("published")).unwrap(),
            b"retained inode"
        );
        let competing = openat2_file(
            directory.as_raw_fd(),
            c".",
            nix::libc::O_TMPFILE | nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0o600,
            controlled_resolution(),
        )
        .unwrap();
        assert_eq!(
            link_path_descriptor_noreplace(&competing, &directory, c"published")
                .unwrap_err()
                .raw_os_error(),
            Some(nix::libc::EEXIST)
        );
    }

    #[test]
    fn new_directory_normalization_retains_identity_and_rejects_name_substitution() {
        let parent = tempfile::tempdir().unwrap();
        let temporary = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(parent.path())
            .unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o000)).unwrap();

        let retained = normalize_new_directory(temporary.path(), 0o700).unwrap();
        require_named_directory(temporary.path(), &retained, 0o700).unwrap();

        let displaced = parent.path().join("displaced");
        std::fs::rename(temporary.path(), &displaced).unwrap();
        std::fs::create_dir(temporary.path()).unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(require_named_directory(temporary.path(), &retained, 0o700).is_err());
        assert_eq!(
            (retained.metadata().unwrap().dev(), retained.metadata().unwrap().ino()),
            (
                std::fs::metadata(&displaced).unwrap().dev(),
                std::fs::metadata(&displaced).unwrap().ino()
            )
        );

        std::fs::remove_dir(temporary.path()).unwrap();
        symlink(&displaced, temporary.path()).unwrap();
        assert!(normalize_new_directory(temporary.path(), 0o700).is_err());
        std::fs::remove_file(temporary.path()).unwrap();
        std::fs::rename(displaced, temporary.path()).unwrap();
    }

    #[test]
    fn chmod_uses_the_calling_tasks_private_descriptor_table() {
        const CHILD: &str = "CAST_FORGE_PRIVATE_FD_TABLE_CHILD";
        const TEST: &str = "linux_fs::tests::chmod_uses_the_calling_tasks_private_descriptor_table";
        if std::env::var_os(CHILD).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .arg(TEST)
                .arg("--exact")
                .arg("--nocapture")
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "task-private descriptor-table child failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let temporary = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let path = temporary.path().to_owned();
        let outcome = std::thread::spawn(move || -> io::Result<Option<(u64, u64, u32)>> {
            // SAFETY: CLONE_FILES gives only this calling task a private copy
            // of the descriptor table. The test runs in a throwaway
            // subprocess and the task exits immediately after this proof.
            if unsafe { nix::libc::unshare(nix::libc::CLONE_FILES) } == -1 {
                let source = io::Error::last_os_error();
                if source.raw_os_error().is_some_and(|code| {
                    [
                        nix::libc::EPERM,
                        nix::libc::EACCES,
                        nix::libc::ENOSYS,
                        nix::libc::EINVAL,
                    ]
                    .contains(&code)
                }) {
                    eprintln!("skipping task-private descriptor-table proof: {source}");
                    return Ok(None);
                }
                return Err(source);
            }

            // Open this capability only after CLONE_FILES. Its descriptor
            // exists in this task's table but not in the TGID leader's table,
            // so /proc/<tgid>/fd would miss it or resolve an unrelated inode.
            let path = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            let retained = openat2_file(
                nix::libc::AT_FDCWD,
                &path,
                nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                0,
                (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
            )?;
            let before = retained.metadata()?;
            chmod_path_descriptor(&retained, 0o700)?;
            let after = retained.metadata()?;
            if (after.dev(), after.ino()) != (before.dev(), before.ino()) {
                return Err(io::Error::other("task-private chmod changed inode identity"));
            }
            Ok(Some((after.dev(), after.ino(), after.permissions().mode() & 0o7777)))
        })
        .join()
        .expect("task-private descriptor-table worker panicked")
        .unwrap();

        if let Some((device, inode, mode)) = outcome {
            let metadata = std::fs::metadata(temporary.path()).unwrap();
            assert_eq!((metadata.dev(), metadata.ino()), (device, inode));
            assert_eq!(mode, 0o700);
            assert_eq!(metadata.permissions().mode() & 0o7777, 0o700);
        }
    }
}
