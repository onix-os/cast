
//! Linux 5.6-compatible operations on retained filesystem capabilities.
//!
//! `O_PATH` descriptors cannot be passed to `fchmod(2)`, while
//! `fchmodat2(2)` postdates Mason's Linux 5.6 baseline. The only pathname
//! resolved here is a live decimal descriptor below authenticated procfs.

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
const MAX_DECIMAL_PID_BYTES: usize = 16;
const MAX_THREAD_SELF_BYTES: usize = MAX_DECIMAL_PID_BYTES * 2 + 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InodeIdentity {
    device: u64,
    inode: u64,
}

/// Change the mode of the exact inode retained by an `O_PATH` descriptor.
///
/// This uses only Linux 5.6-era interfaces. `/proc`, the current process
/// directory, and its `fd` directory must all authenticate as procfs before
/// `fchmodat(2)` may follow the descriptor magic link. Success is reported
/// only after one post-call metadata snapshot proves both exact mode and
/// unchanged inode identity.
pub(crate) fn chmod_path_descriptor(file: &std::fs::File, mode: u32) -> io::Result<()> {
    if mode & !0o7777 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("filesystem mode is outside the canonical 07777 mask: {mode:#o}"),
        ));
    }

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
    loop {
        // SAFETY: the directory is authenticated procfs, the component is the
        // target descriptor's canonical decimal number, and flags=0 follows
        // that procfs magic link to the retained inode.
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
    let expected_thread_id = current_thread_id()?;
    if process_id != std::process::id() || thread_id != expected_thread_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "authenticated procfs thread-self link names {process_id}/task/{thread_id}, expected {}/task/{}",
                std::process::id(),
                expected_thread_id
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

fn openat2_file(dirfd: RawFd, path: &CStr, flags: i32, mode: u32, resolve: u64) -> io::Result<std::fs::File> {
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

fn controlled_resolution() -> u64 {
    (nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV) as u64
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::CString,
        os::unix::fs::{MetadataExt as _, PermissionsExt as _},
        process::Command,
    };

    use super::*;

    #[test]
    fn procfs_authentication_rejects_an_ordinary_filesystem() {
        let temporary = crate::private_tempdir();
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
        let canonical = format!("{}/task/{}", std::process::id(), current_thread_id().unwrap());
        let (process, thread) = parse_thread_self(canonical.as_bytes()).unwrap();
        assert_eq!(process.to_bytes(), std::process::id().to_string().as_bytes());
        assert_eq!(thread.to_bytes(), current_thread_id().unwrap().to_string().as_bytes());

        for malformed in [
            format!("{}/{}", std::process::id(), current_thread_id().unwrap()),
            format!("{}/task/01", std::process::id()),
            format!("{}/task/{}/extra", std::process::id(), current_thread_id().unwrap()),
        ] {
            assert_eq!(
                parse_thread_self(malformed.as_bytes()).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn chmod_revalidates_the_exact_opath_inode_and_mode() {
        let temporary = crate::private_tempdir();
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
    fn chmod_uses_the_calling_tasks_private_descriptor_table() {
        const CHILD: &str = "CAST_MASON_PRIVATE_FD_TABLE_CHILD";
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

        let temporary = crate::private_tempdir();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let path = temporary.path().to_owned();
        let outcome = std::thread::spawn(move || -> io::Result<Option<(u64, u64, u32)>> {
            // SAFETY: CLONE_FILES asks the kernel to give only this calling
            // task a private copy of the process descriptor table. The test
            // runs in a throwaway subprocess and the task exits after use.
            if unsafe { nix::libc::unshare(nix::libc::CLONE_FILES) } == -1 {
                let source = io::Error::last_os_error();
                if matches!(
                    source.raw_os_error(),
                    Some(nix::libc::EPERM | nix::libc::EACCES | nix::libc::ENOSYS | nix::libc::EINVAL)
                ) {
                    eprintln!("skipping task-private descriptor-table proof: {source}");
                    return Ok(None);
                }
                return Err(source);
            }

            // Open the capability only after CLONE_FILES. Its descriptor
            // number therefore exists in this task's fd table but not in the
            // TGID leader's table. A /proc/<tgid>/fd implementation would
            // either miss it or resolve an unrelated descriptor.
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
