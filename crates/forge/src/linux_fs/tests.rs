use std::{
    cell::Cell,
    io::{Read as _, Write as _},
    os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
    process::Command,
    time::Duration,
};

use super::*;

mod chmod_once;
mod mountinfo_bounds;
mod mountinfo_compatibility;
mod mountinfo_grammar;
mod sysfs_block_identity;
mod sysfs_block_links;
mod sysfs_block_numeric;
mod sysfs_block_uevent;
mod xattrs;

struct InterruptingBoundedReader<'a> {
    interruptions: usize,
    bytes: &'a [u8],
    offset: usize,
    calls: usize,
    bytewise: bool,
}

impl<'a> InterruptingBoundedReader<'a> {
    fn new(interruptions: usize, bytes: &'a [u8], bytewise: bool) -> Self {
        Self {
            interruptions,
            bytes,
            offset: 0,
            calls: 0,
            bytewise,
        }
    }
}

impl io::Read for InterruptingBoundedReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.interruptions > 0 {
            self.interruptions -= 1;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        if self.offset == self.bytes.len() {
            return Ok(0);
        }
        let available = self.bytes.len() - self.offset;
        let count = if self.bytewise { 1 } else { available.min(output.len()) };
        output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

#[test]
fn interrupted_retry_limit_accepts_n_and_rejects_n_plus_one() {
    let accepted_attempts = Cell::new(0usize);
    retry_interrupted(None, || {
        let attempt = accepted_attempts.get();
        accepted_attempts.set(attempt + 1);
        if attempt < MAX_INTERRUPTED_SYSCALL_RETRIES {
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(accepted_attempts.get(), MAX_INTERRUPTED_SYSCALL_RETRIES + 1);

    let rejected_attempts = Cell::new(0usize);
    let error = retry_interrupted(None, || -> io::Result<()> {
        rejected_attempts.set(rejected_attempts.get() + 1);
        Err(io::Error::from(io::ErrorKind::Interrupted))
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(rejected_attempts.get(), MAX_INTERRUPTED_SYSCALL_RETRIES + 1);

    let mut accepted_read = InterruptingBoundedReader::new(MAX_INTERRUPTED_SYSCALL_RETRIES, b"ok", false);
    assert_eq!(read_to_end_bounded(&mut accepted_read, 3).unwrap(), b"ok");
    assert_eq!(accepted_read.calls, MAX_INTERRUPTED_SYSCALL_RETRIES + 2);

    let mut rejected_read = InterruptingBoundedReader::new(MAX_INTERRUPTED_SYSCALL_RETRIES + 1, b"ok", false);
    let error = read_to_end_bounded(&mut rejected_read, 3).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(rejected_read.calls, MAX_INTERRUPTED_SYSCALL_RETRIES + 1);

    let mut bytewise = InterruptingBoundedReader::new(0, b"oversized", true);
    assert_eq!(read_to_end_bounded(&mut bytewise, 4).unwrap(), b"over");
    assert_eq!(bytewise.calls, 4);

    let reservation_calls = Cell::new(0usize);
    let growth_reservations = Cell::new(0usize);
    let growth_input = vec![b'g'; 4 * 1024 + 1];
    let mut fallible_growth = InterruptingBoundedReader::new(0, &growth_input, false);
    let error = read_to_end_bounded_with_deadline_and_reservation(
        &mut fallible_growth,
        growth_input.len(),
        None,
        |bytes, additional| {
            reservation_calls.set(reservation_calls.get() + 1);
            if bytes.len() + additional > bytes.capacity() {
                let growth = growth_reservations.get();
                growth_reservations.set(growth + 1);
                if growth == 1 {
                    return Err(io::Error::other("injected bounded-read growth allocation failure"));
                }
            }
            bytes
                .try_reserve(additional)
                .map_err(|source| io::Error::other(format!("test allocation failed: {source}")))
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(growth_reservations.get(), 2);
    assert_eq!(reservation_calls.get(), 10);
    assert_eq!(fallible_growth.calls, 9);
}

#[test]
fn expired_retry_deadline_fails_before_another_syscall() {
    let attempts = Cell::new(0usize);
    let deadline = Instant::now() - Duration::from_millis(1);
    let error = retry_interrupted(Some(deadline), || {
        attempts.set(attempts.get() + 1);
        Ok(())
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(attempts.get(), 0);

    let mut reader = InterruptingBoundedReader::new(0, b"unread", false);
    let error = read_to_end_bounded_until(&mut reader, 6, deadline).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(reader.calls, 0);

    let checkpoints = Cell::new(0usize);
    let reservations = Cell::new(0usize);
    let mut final_copy = InterruptingBoundedReader::new(0, b"x", false);
    let error = read_to_end_bounded_with_deadline_and_hooks(
        &mut final_copy,
        1,
        None,
        |bytes, additional| {
            reservations.set(reservations.get() + 1);
            bytes
                .try_reserve(additional)
                .map_err(|source| io::Error::other(format!("test allocation failed: {source}")))
        },
        |_| {
            let checkpoint = checkpoints.get();
            checkpoints.set(checkpoint + 1);
            if checkpoint == 3 {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "injected deadline expiry after the final bounded copy",
                ))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(checkpoints.get(), 4);
    assert_eq!(reservations.get(), 2);
    assert_eq!(final_copy.calls, 1);

    let temporary = tempfile::tempdir().unwrap();
    let directory = std::fs::File::open(temporary.path()).unwrap();
    assert_eq!(
        descriptor_mount_id_until(&directory, deadline).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn mkdirat_once_issues_one_descriptor_relative_creation() {
    let temporary = tempfile::tempdir().unwrap();
    let retained_path = temporary.path().join("retained");
    let displaced_path = temporary.path().join("displaced");
    std::fs::create_dir(&retained_path).unwrap();
    std::fs::set_permissions(&retained_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let retained_directory = std::fs::File::open(&retained_path).unwrap();

    std::fs::rename(&retained_path, &displaced_path).unwrap();
    std::fs::create_dir(&retained_path).unwrap();

    mkdirat_once(&retained_directory, c"created", 0o700).unwrap();

    let created = std::fs::symlink_metadata(displaced_path.join("created")).unwrap();
    assert!(created.file_type().is_dir());
    let created_mode = created.permissions().mode() & 0o7777;
    assert_eq!(created_mode & !0o700, 0);
    assert_eq!(
        std::fs::symlink_metadata(retained_path.join("created"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn mkdirat_once_reports_eexist_without_replacing_the_existing_entry() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = std::fs::File::open(temporary.path()).unwrap();
    let existing = temporary.path().join("existing");
    std::fs::create_dir(&existing).unwrap();
    std::fs::write(existing.join("sentinel"), b"retained contents").unwrap();
    let before = std::fs::symlink_metadata(&existing).unwrap();

    let error = mkdirat_once(&directory, c"existing", 0o700).unwrap_err();

    assert_eq!(error.raw_os_error(), Some(nix::libc::EEXIST));
    let after = std::fs::symlink_metadata(&existing).unwrap();
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!(std::fs::read(existing.join("sentinel")).unwrap(), b"retained contents");
}

#[test]
fn mkdirat_once_rejects_invalid_components_and_modes_without_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let nested = temporary.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let directory = std::fs::File::open(temporary.path()).unwrap();
    let inventory = |path: &Path| {
        let mut names = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    let parent_before = inventory(temporary.path());
    let nested_before = inventory(&nested);

    for invalid in [c"".as_ref(), c".".as_ref(), c"..".as_ref(), c"nested/name".as_ref()] {
        assert_eq!(
            mkdirat_once(&directory, invalid, 0o700).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        mkdirat_once(&directory, c"outside-mode", 0o10000).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );

    assert_eq!(inventory(temporary.path()), parent_before);
    assert_eq!(inventory(&nested), nested_before);
}

#[test]
fn expired_rename_deadline_preserves_both_namespaces() {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let source_directory = std::fs::File::open(source.path()).unwrap();
    let destination_directory = std::fs::File::open(destination.path()).unwrap();
    std::fs::write(source.path().join("candidate"), b"retained candidate").unwrap();

    let error = renameat2_noreplace_until(
        &source_directory,
        c"candidate",
        &destination_directory,
        c"published",
        Instant::now() - Duration::from_millis(1),
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        std::fs::read(source.path().join("candidate")).unwrap(),
        b"retained candidate"
    );
    assert!(!destination.path().join("published").exists());
}

#[test]
fn expired_sync_filesystem_deadline_fails_before_syncfs() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = std::fs::File::open(temporary.path()).unwrap();

    let error = sync_filesystem_until(&directory, Instant::now() - Duration::from_millis(1)).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

#[test]
fn procfs_authentication_rejects_an_ordinary_filesystem() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = std::fs::File::open(temporary.path()).unwrap();

    let error = require_procfs(&directory, temporary.path()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn sysfs_authentication_accepts_kernel_sysfs_and_rejects_other_filesystems() {
    let temporary = tempfile::tempdir().unwrap();
    let ordinary = std::fs::File::open(temporary.path()).unwrap();
    assert_eq!(
        require_sysfs(&ordinary, temporary.path()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    let sysfs = std::fs::File::open("/sys").unwrap();
    require_sysfs(&sysfs, Path::new("/sys")).unwrap();
    assert_eq!(
        require_sysfs_until(&sysfs, Path::new("/sys"), Instant::now() - Duration::from_millis(1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn authenticated_procfs_descriptor_child_path_binds_the_retained_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = std::fs::File::open(temporary.path()).unwrap();
    let path = authenticated_procfs_descriptor_child_path(&directory, c"database").unwrap();
    assert_eq!(path, format!("/proc/thread-self/fd/{}/database", directory.as_raw_fd()));
    std::fs::write(&path, b"retained directory child").unwrap();
    assert_eq!(
        std::fs::read(temporary.path().join("database")).unwrap(),
        b"retained directory child"
    );

    for invalid in [c"".as_ref(), c".".as_ref(), c"..".as_ref(), c"nested/name".as_ref()] {
        assert_eq!(
            authenticated_procfs_descriptor_child_path(&directory, invalid)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
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
fn descriptor_read_uses_the_retained_inode_and_preserves_atime() {
    let temporary = tempfile::tempdir().unwrap();
    let named = temporary.path().join("named");
    let displaced = temporary.path().join("displaced");
    std::fs::write(&named, b"retained bytes").unwrap();
    std::fs::set_permissions(&named, std::fs::Permissions::from_mode(0o600)).unwrap();
    filetime::set_file_times(
        &named,
        filetime::FileTime::from_unix_time(111, 0),
        filetime::FileTime::from_unix_time(222, 0),
    )
    .unwrap();
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
    std::fs::write(&named, b"replacement bytes").unwrap();

    let mut readable = open_path_descriptor_readonly(&retained).unwrap();
    let mut bytes = Vec::new();
    readable.read_to_end(&mut bytes).unwrap();

    assert_eq!(bytes, b"retained bytes");
    assert_eq!(retained.metadata().unwrap().atime(), 111);
    assert_eq!(std::fs::read(&named).unwrap(), b"replacement bytes");
}

#[test]
fn descriptor_read_rejects_non_regular_capabilities() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = CString::new(temporary.path().as_os_str().as_encoded_bytes()).unwrap();
    let retained_directory = openat2_file(
        nix::libc::AT_FDCWD,
        &directory,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )
    .unwrap();
    assert_eq!(
        open_path_descriptor_readonly(&retained_directory).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );

    let link = temporary.path().join("link");
    symlink("target", &link).unwrap();
    let link = CString::new(link.as_os_str().as_encoded_bytes()).unwrap();
    let retained_link = openat2_file(
        nix::libc::AT_FDCWD,
        &link,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )
    .unwrap();
    assert_eq!(
        open_path_descriptor_readonly(&retained_link).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
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
