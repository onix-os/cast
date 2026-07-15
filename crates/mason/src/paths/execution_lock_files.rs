fn execution_lock_leaf(derivation_id: &str) -> io::Result<CString> {
    let id = derivation_id.as_bytes();
    let name_len = id
        .len()
        .checked_add(EXECUTION_LOCK_SUFFIX.len())
        .ok_or_else(|| invalid_binding("execution lock name length overflowed".to_owned()))?;
    if id.is_empty() || name_len > MAX_EXECUTION_LOCK_NAME_BYTES || id.iter().any(|byte| *byte == b'/' || *byte == 0) {
        return Err(invalid_binding(format!(
            "invalid derivation identity for execution lock ({} bytes)",
            id.len()
        )));
    }
    let mut name = Vec::with_capacity(name_len);
    name.extend_from_slice(id);
    name.extend_from_slice(EXECUTION_LOCK_SUFFIX);
    CString::new(name).map_err(|_| invalid_binding("execution lock name contains NUL".to_owned()))
}

fn open_or_create_execution_lock_file(parent: &StdFile, name: &CStr) -> io::Result<StdFile> {
    match open_execution_lock_file(parent, name, true) {
        Ok(file) => {
            // O_EXCL proves this call created the inode, so normalizing the
            // exact pinned descriptor cannot launder an unsafe pre-existing
            // entry. This also makes creation independent of the caller's
            // process-global umask.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            Ok(file)
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => open_execution_lock_file(parent, name, false),
        Err(source) => Err(source),
    }
}

fn open_execution_lock_file(parent: &StdFile, name: &CStr, create_exclusive: bool) -> io::Result<StdFile> {
    // O_NONBLOCK is required even though a valid lock is a regular file: a
    // hostile pre-existing FIFO must be rejected rather than hanging before
    // its type can be authenticated.
    let mut flags = nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK;
    if create_exclusive {
        flags |= nix::libc::O_CREAT | nix::libc::O_EXCL;
    }
    // SAFETY: an all-zero open_how is valid before its public fields are set.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = if create_exclusive { 0o600 } else { 0 };
    how.resolve = nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV;
    // SAFETY: parent, the single validated component, and open_how remain live
    // for the syscall.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = RawFd::try_from(result)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {result}")))?;
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    Ok(unsafe { StdFile::from_raw_fd(descriptor) })
}

fn require_controlled_lock_file(file: &StdFile, root: &std::fs::Metadata, path: &Path) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions.
    let owner = unsafe { nix::libc::geteuid() };
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.uid() != owner
        || metadata.dev() != root.dev()
        || mode != 0o600
        || metadata.nlink() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "execution lock is not one private regular file: {path:?} (uid={}, mode={mode:#06o}, links={})",
                metadata.uid(),
                metadata.nlink()
            ),
        ));
    }
    Ok((metadata.dev(), metadata.ino()))
}
