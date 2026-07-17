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
    retry_interrupted(None, || {
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
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;

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

/// Give one exact retained, already-named regular inode a second no-replace
/// name in an authenticated target directory.
///
/// The procfs descriptor alias binds the source to the open inode rather than
/// a mutable source pathname. This is deliberately separate from
/// [`link_path_descriptor_noreplace`], whose strict unnamed-inode contract is
/// retained for ordinary `O_TMPFILE` publication.
pub(crate) fn link_retained_file_noreplace(
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
            "retained-file link target must be one nonempty component",
        ));
    }
    let source_metadata = file.metadata()?;
    if !source_metadata.file_type().is_file() || source_metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "retained-file link source must be one singly-named regular inode",
        ));
    }

    let (descriptors, descriptor, expected) = authenticated_descriptor_name(file)?;
    retry_interrupted(None, || {
        // SAFETY: both directory descriptors and names remain live. The only
        // followed source is the authenticated procfs alias for `file`.
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
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;

    let linked_metadata = file.metadata()?;
    require_same_inode(expected, inode_identity(&linked_metadata))?;
    if linked_metadata.nlink() != 2 {
        return Err(io::Error::other(format!(
            "retained-file link source has {} names after publication, expected exactly two",
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

/// Make exactly one descriptor-relative directory-creation attempt.
///
/// This is only a raw syscall adapter, not evidence that the requested name
/// is present or absent. The requested mode is an upper bound under the
/// process umask and inherited directory policy. The caller must retain the
/// parent and freshly reconcile the name after every return value; in
/// particular, this function never retries an interrupted attempt or adopts
/// an existing entry as success.
pub(crate) fn mkdirat_once(parent_directory: &std::fs::File, name: &CStr, mode: u32) -> io::Result<()> {
    let name_bytes = name.to_bytes();
    if name_bytes.is_empty() || name_bytes.contains(&b'/') || matches!(name_bytes, b"." | b"..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "descriptor-relative mkdir name must be one nonempty component",
        ));
    }
    if mode & !0o7777 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("descriptor-relative mkdir mode is outside the canonical 07777 mask: {mode:#o}"),
        ));
    }

    // SAFETY: the retained parent descriptor and validated single-component
    // C string remain live for this one call. mkdirat never follows or
    // replaces the final component. The caller reconciles every raw result.
    if unsafe { nix::libc::mkdirat(parent_directory.as_raw_fd(), name.as_ptr(), mode) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
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
    renameat2_noreplace_with_deadline(
        source_directory,
        source_name,
        destination_directory,
        destination_name,
        None,
    )
}

/// Deadline-aware form used by finite frozen-root publication and cleanup.
pub(crate) fn renameat2_noreplace_until(
    source_directory: &std::fs::File,
    source_name: &CStr,
    destination_directory: &std::fs::File,
    destination_name: &CStr,
    deadline: Instant,
) -> io::Result<()> {
    renameat2_noreplace_with_deadline(
        source_directory,
        source_name,
        destination_directory,
        destination_name,
        Some(deadline),
    )
}

/// Exchange two single-component names beneath retained directory parents.
///
/// This deliberately performs exactly one syscall attempt.  An interrupted
/// or otherwise failed `RENAME_EXCHANGE` may already have taken effect, and a
/// blind retry would exchange the names back.  Callers must retain both
/// parents and reconcile both names after every return value.
pub(crate) fn renameat2_exchange_once(
    first_directory: &std::fs::File,
    first_name: &CStr,
    second_directory: &std::fs::File,
    second_name: &CStr,
) -> io::Result<()> {
    for (role, name) in [("first", first_name), ("second", second_name)] {
        if name.to_bytes().is_empty() || name.to_bytes().contains(&b'/') || matches!(name.to_bytes(), b"." | b"..") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("descriptor-relative exchange {role} name must be one nonempty component"),
            ));
        }
    }

    // SAFETY: both retained directory descriptors and both validated C
    // strings remain live for this one syscall attempt.  The result remains
    // ambiguous until the caller reconciles the two retained namespaces.
    if unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            first_directory.as_raw_fd(),
            first_name.as_ptr(),
            second_directory.as_raw_fd(),
            second_name.as_ptr(),
            nix::libc::RENAME_EXCHANGE,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Move one single-component name between retained parents without replacing
/// the destination, making exactly one syscall attempt.
///
/// Unlike [`renameat2_noreplace`], this primitive must not retry `EINTR`: an
/// error can be reported after the kernel has already moved the name.  The
/// caller is responsible for retaining both parents and reconciling both
/// names after every result.
pub(crate) fn renameat2_noreplace_once(
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

    // SAFETY: both retained directory descriptors and both validated C
    // strings remain live for this one syscall attempt. RENAME_NOREPLACE
    // prevents destination loss; the caller reconciles an ambiguous result.
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
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn renameat2_noreplace_with_deadline(
    source_directory: &std::fs::File,
    source_name: &CStr,
    destination_directory: &std::fs::File,
    destination_name: &CStr,
    deadline: Option<Instant>,
) -> io::Result<()> {
    for (role, name) in [("source", source_name), ("destination", destination_name)] {
        if name.to_bytes().is_empty() || name.to_bytes().contains(&b'/') || matches!(name.to_bytes(), b"." | b"..") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("descriptor-relative rename {role} must be one nonempty component"),
            ));
        }
    }

    retry_interrupted(deadline, || {
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
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

/// Flush every pending write on the filesystem containing a retained
/// capability. This is intentionally broader than `fsync` on one directory:
/// failed-candidate preservation must not delete its database correlation
/// while trigger-created descendants are still only dirty cache state.
pub(crate) fn sync_filesystem(file: &std::fs::File) -> io::Result<()> {
    sync_filesystem_with_deadline(file, None)
}

/// Deadline-aware form used by finite frozen-root publication and cleanup.
pub(crate) fn sync_filesystem_until(file: &std::fs::File, deadline: Instant) -> io::Result<()> {
    sync_filesystem_with_deadline(file, Some(deadline))
}

fn sync_filesystem_with_deadline(file: &std::fs::File, deadline: Option<Instant>) -> io::Result<()> {
    retry_interrupted(deadline, || {
        // SAFETY: the retained descriptor remains live and identifies the
        // filesystem whose pending data and metadata must reach stable storage.
        if unsafe { nix::libc::syncfs(file.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

fn authenticated_descriptor_name(file: &std::fs::File) -> io::Result<(std::fs::File, CString, InodeIdentity)> {
    authenticated_descriptor_name_with_deadline(file, None)
}

fn authenticated_descriptor_name_with_deadline(
    file: &std::fs::File,
    deadline: Option<Instant>,
) -> io::Result<(std::fs::File, CString, InodeIdentity)> {
    let expected = inode_identity(&file.metadata()?);
    let thread = authenticated_current_thread_procfs_with_deadline(deadline)?;
    let descriptors = openat2_file_with_deadline(
        thread.as_raw_fd(),
        c"fd",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
        deadline,
    )?;
    require_procfs_with_deadline(&descriptors, Path::new("/proc/thread-self/fd"), deadline)?;

    let descriptor = CString::new(file.as_raw_fd().to_string()).expect("numeric descriptor contains no NUL");
    let alias = open_descriptor_alias_with_deadline(&descriptors, &descriptor, deadline)?;
    require_same_inode(expected, inode_identity(&alias.metadata()?))?;
    Ok((descriptors, descriptor, expected))
}

/// Build one UTF-8 child path below an exact retained descriptor, but only
/// after authenticating both the current-thread procfs mount and its numeric
/// descriptor alias. Callers which hand the path to an external library must
/// repeat this proof after the library opens the path.
pub(crate) fn authenticated_procfs_descriptor_child_path(
    file: &std::fs::File,
    child: &CStr,
) -> io::Result<String> {
    let child_bytes = child.to_bytes();
    if child_bytes.is_empty() || child_bytes == b"." || child_bytes == b".." || child_bytes.contains(&b'/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "procfs descriptor child must be one non-dot path component",
        ));
    }
    let child = child
        .to_str()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "procfs descriptor child must be UTF-8"))?;
    let (_descriptors, descriptor, _expected) = authenticated_descriptor_name(file)?;
    let descriptor = descriptor
        .to_str()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "numeric descriptor name is not UTF-8"))?;
    Ok(format!("/proc/thread-self/fd/{descriptor}/{child}"))
}

pub(crate) fn authenticated_procfs_root() -> io::Result<std::fs::File> {
    authenticated_procfs_root_with_deadline(None)
}

fn authenticated_procfs_root_with_deadline(deadline: Option<Instant>) -> io::Result<std::fs::File> {
    let proc = openat2_file_with_deadline(
        nix::libc::AT_FDCWD,
        c"/proc",
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
        deadline,
    )?;
    require_procfs_with_deadline(&proc, Path::new("/proc"), deadline)?;
    Ok(proc)
}

pub(crate) fn authenticated_current_thread_procfs() -> io::Result<std::fs::File> {
    authenticated_current_thread_procfs_with_deadline(None)
}

fn authenticated_current_thread_procfs_with_deadline(deadline: Option<Instant>) -> io::Result<std::fs::File> {
    let proc = authenticated_procfs_root_with_deadline(deadline)?;

    let (process_name, thread_name) = proc_thread_self_components_with_deadline(&proc, deadline)?;
    let process = openat2_file_with_deadline(
        proc.as_raw_fd(),
        &process_name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        deadline,
    )?;
    require_procfs_with_deadline(&process, Path::new("/proc/<pid>"), deadline)?;

    let tasks = openat2_file_with_deadline(
        process.as_raw_fd(),
        c"task",
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        deadline,
    )?;
    require_procfs_with_deadline(&tasks, Path::new("/proc/<pid>/task"), deadline)?;
    let thread = openat2_file_with_deadline(
        tasks.as_raw_fd(),
        &thread_name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        deadline,
    )?;
    require_procfs_with_deadline(&thread, Path::new("/proc/<pid>/task/<tid>"), deadline)?;
    Ok(thread)
}
