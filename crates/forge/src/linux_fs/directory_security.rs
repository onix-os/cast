/// Reject an inheritable POSIX default ACL on an authenticated readable
/// directory descriptor.
///
/// Access-ACL write authority is represented by the group-class mode mask,
/// but a default ACL is not. Admitting one would let later children inherit
/// authority that an otherwise-safe directory mode does not reveal.
pub(crate) fn require_no_default_acl(file: &std::fs::File, path: &Path) -> io::Result<()> {
    require_no_acl_xattr(file, path, POSIX_DEFAULT_ACL_XATTR, "inheritable POSIX default", None)
}

/// Deadline-aware form used by finite frozen-root materialization.
pub(crate) fn require_no_default_acl_until(file: &std::fs::File, path: &Path, deadline: Instant) -> io::Result<()> {
    require_no_acl_xattr(
        file,
        path,
        POSIX_DEFAULT_ACL_XATTR,
        "inheritable POSIX default",
        Some(deadline),
    )
}

/// Reject an explicit POSIX access ACL on an authenticated readable inode.
///
/// The synthesized empty live `/usr` baseline requires a canonical mode-only
/// authority model. Existing trees continue to rely on the mode mask plus the
/// separate default-ACL check above.
pub(crate) fn require_no_access_acl(file: &std::fs::File, path: &Path) -> io::Result<()> {
    require_no_acl_xattr(file, path, POSIX_ACCESS_ACL_XATTR, "POSIX access", None)
}

/// Deadline-aware form used by finite frozen-root materialization.
pub(crate) fn require_no_access_acl_until(file: &std::fs::File, path: &Path, deadline: Instant) -> io::Result<()> {
    require_no_acl_xattr(file, path, POSIX_ACCESS_ACL_XATTR, "POSIX access", Some(deadline))
}

fn require_no_acl_xattr(
    file: &std::fs::File,
    path: &Path,
    name: &CStr,
    role: &'static str,
    deadline: Option<Instant>,
) -> io::Result<()> {
    let result = retry_interrupted(deadline, || {
        // SAFETY: `file` and the supplied static xattr name remain live. A null value
        // with size zero is the documented existence/size query and does not
        // copy attribute bytes into userspace.
        let result = unsafe { nix::libc::fgetxattr(file.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if result >= 0 {
            Ok(result)
        } else {
            Err(io::Error::last_os_error())
        }
    });
    match result {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("capability inode carries a {role} ACL: {}", path.display()),
        )),
        Err(source)
            if matches!(
                source.raw_os_error(),
                Some(nix::libc::ENODATA) | Some(nix::libc::EOPNOTSUPP)
            ) =>
        {
            Ok(())
        }
        Err(source) => Err(source),
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
    normalize_new_directory_with_deadline(path, mode, None)
}

fn normalize_new_directory_with_deadline(
    path: &Path,
    mode: u32,
    deadline: Option<Instant>,
) -> io::Result<std::fs::File> {
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
    let pinned = openat2_file_with_deadline(nix::libc::AT_FDCWD, &encoded, flags, 0, resolve, deadline)?;
    require_new_directory_residue(&pinned, path, mode)?;
    let expected = inode_identity(&pinned.metadata()?);
    chmod_path_descriptor_with_deadline(&pinned, mode, deadline)?;

    let named = openat2_file_with_deadline(nix::libc::AT_FDCWD, &encoded, flags, 0, resolve, deadline)?;
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
