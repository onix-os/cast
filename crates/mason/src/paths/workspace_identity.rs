fn open_directory_nofollow(path: &Path) -> io::Result<StdFile> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC | nix::libc::O_NONBLOCK)
        .open(path)
}

/// Pin one workspace root without following any symlink in its pathname.
///
/// The returned `O_PATH` descriptor is suitable for descriptor-backed
/// container binds. No ownership or mode policy is imposed here because Forge
/// intentionally accepts safe root-owned and read-only resolver roots.
pub(crate) fn pin_workspace_root(path: &Path) -> io::Result<StdFile> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let encoded = CString::new(absolute.as_os_str().as_bytes())
        .map_err(|_| invalid_binding(format!("workspace root contains NUL: {absolute:?}")))?;
    // SAFETY: an all-zero open_how is valid before its public fields are set.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags =
        u64::from((nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC) as u32);
    how.resolve = (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64;
    let descriptor = loop {
        // SAFETY: the encoded pathname and open_how remain live. Success
        // returns one fresh descriptor owned below.
        let descriptor = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_openat2,
                nix::libc::AT_FDCWD,
                encoded.as_ptr(),
                &how,
                size_of::<nix::libc::open_how>(),
            )
        };
        if descriptor != -1 {
            break descriptor;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::new(
                source.kind(),
                format!("pin workspace root without symlinks {absolute:?}: {source}"),
            ));
        }
    };
    let descriptor = RawFd::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    let directory = unsafe { StdFile::from_raw_fd(descriptor) };
    require_workspace_root_directory(&directory, &absolute)?;
    Ok(directory)
}

/// Prove that a mutable workspace pathname still names one retained root.
///
/// The newly opened descriptor is used only as a name witness. Callers must
/// continue using `expected` as their authority so a substitution after this
/// check can never redirect a destructive operation to the replacement.
pub(crate) fn require_workspace_root_path(expected: &StdFile, path: &Path) -> io::Result<()> {
    pin_matching_workspace_root(expected, path).map(drop)
}

/// Pin the current workspace name as an `O_PATH` capability and prove that it
/// still denotes the already-retained directory.
///
/// Callers which also need ordinary read access retain their existing handle
/// in parallel. The returned descriptor is the namespace-reopen witness used
/// by descriptor-authenticated container locators.
pub(crate) fn pin_matching_workspace_root(expected: &StdFile, path: &Path) -> io::Result<StdFile> {
    require_workspace_root_directory(expected, path)?;
    let reopened = pin_workspace_root(path)?;
    require_same_directory(expected, &reopened, path)?;
    Ok(reopened)
}

fn open_private_child(parent: &StdFile, name: &CStr) -> io::Result<StdFile> {
    // SAFETY: an all-zero open_how is valid before its public fields are set.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(
        (nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NONBLOCK) as u32,
    );
    how.resolve = nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV;
    // SAFETY: parent, component, and open_how remain live for the syscall.
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

fn require_controlled_directory(directory: &StdFile, path: &Path, exact_private: bool) -> io::Result<()> {
    let metadata = directory.metadata()?;
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    let owner = unsafe { nix::libc::geteuid() };
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != owner
        || metadata.mode() & 0o022 != 0
        || (exact_private && mode != 0o700)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "host directory is not privately controlled: {path:?} (uid={}, mode={mode:#06o})",
                metadata.uid()
            ),
        ));
    }
    Ok(())
}
fn require_same_device(root: &std::fs::Metadata, directory: &StdFile, path: &Path) -> io::Result<()> {
    let metadata = directory.metadata()?;
    if root.dev() != metadata.dev() {
        return Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            format!("private host path crosses a mount beneath the workspace: {path:?}"),
        ));
    }
    Ok(())
}

fn require_same_directory(expected: &StdFile, found: &StdFile, path: &Path) -> io::Result<()> {
    let expected = expected.metadata()?;
    let found = found.metadata()?;
    if expected.dev() != found.dev() || expected.ino() != found.ino() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("workspace path was replaced after construction: {path:?}"),
        ));
    }
    Ok(())
}
