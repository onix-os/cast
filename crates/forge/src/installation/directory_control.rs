const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const LOCKFILE_MODE: u32 = 0o600;
const CAST_DIRECTORY_NAME: &CStr = c".cast";
const LOCKFILE_NAME: &CStr = c".cast-lockfile";
const CACHEDIR_TAG_MODE: u32 = 0o644;
const CACHEDIR_TAG_TEMPORARY_MODE: u32 = 0o600;
const CACHEDIR_TAG_NAME: &CStr = c"CACHEDIR.TAG";
const CACHEDIR_TAG_TEMPORARY_NAME: &CStr = c".CACHEDIR.TAG.cast-tmp";
const CACHEDIR_TAG_CONTENTS: &[u8] = br#"Signature: 8a477f597d28d172789f06886806bc55
# This file is a cache directory tag created by Cast.
# For information about cache directory tags see https://bford.info/cachedir/"#;

#[derive(Debug)]
struct ControlledDirectory {
    file: std::fs::File,
    path: PathBuf,
}

/// Ensures Cast directories are created without allowing the cache and asset
/// capability roots to inherit a permissive process-global umask.
fn ensure_dirs_exist(
    root_directory: &std::fs::File,
    root: &Path,
) -> Result<mutable_namespace::ProvisionedDirectories, Error> {
    let cast_path = root.join(".cast");
    let cast = ensure_controlled_child(root_directory, OsStr::new(".cast"), &cast_path).map_err(|source| {
        Error::PrepareDirectory {
            path: cast_path.clone(),
            source,
        }
    })?;

    let cache_path = cast_path.join("cache");
    let cache = ensure_controlled_child(&cast.file, OsStr::new("cache"), &cache_path).map_err(|source| {
        Error::PrepareDirectory {
            path: cache_path.clone(),
            source,
        }
    })?;
    let assets_path = cast_path.join("assets");
    ensure_controlled_child(&cast.file, OsStr::new("assets"), &assets_path).map_err(|source| {
        Error::PrepareDirectory {
            path: assets_path,
            source,
        }
    })?;
    let quarantine_path = cast_path.join("quarantine");
    ensure_controlled_child(&cast.file, OsStr::new("quarantine"), &quarantine_path).map_err(|source| {
        Error::PrepareDirectory {
            path: quarantine_path,
            source,
        }
    })?;

    // Build the remaining fixed directory topology through the same pinned,
    // durable creation boundary. Existing safe shared-readable modes are
    // preserved, while new entries and restrictive-umask residue become 0700.
    let database_path = cast_path.join("db");
    let database = ensure_controlled_child(&cast.file, OsStr::new("db"), &database_path).map_err(|source| {
        Error::PrepareDirectory {
            path: database_path,
            source,
        }
    })?;
    let repo_path = cast_path.join("repo");
    ensure_controlled_child(&cast.file, OsStr::new("repo"), &repo_path).map_err(|source| {
        Error::PrepareDirectory {
            path: repo_path,
            source,
        }
    })?;
    let roots_path = cast_path.join("root");
    let roots = ensure_controlled_child(&cast.file, OsStr::new("root"), &roots_path).map_err(|source| {
        Error::PrepareDirectory {
            path: roots_path.clone(),
            source,
        }
    })?;
    for name in ["staging", "isolation"] {
        let path = roots_path.join(name);
        ensure_controlled_child(&roots.file, OsStr::new(name), &path)
            .map_err(|source| Error::PrepareDirectory { path, source })?;
    }

    ensure_cachedir_tag(&cache).map_err(|source| Error::PrepareCachedirTag {
        path: cache_path.join("CACHEDIR.TAG"),
        source,
    })?;
    Ok(mutable_namespace::ProvisionedDirectories { cast, database })
}

fn ensure_controlled_child(parent: &std::fs::File, name: &OsStr, path: &Path) -> io::Result<ControlledDirectory> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory name contains NUL"))?;
    mkdirat_if_absent(parent.as_raw_fd(), name.as_c_str(), PRIVATE_DIRECTORY_MODE)?;

    let pinned = openat2_file(
        parent.as_raw_fd(),
        name.as_c_str(),
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    if requires_private_mode_recovery(&pinned, path)? {
        // Normalize only this retained inode. This also completes a prior
        // mkdir/open crash whose restrictive umask exposed an owner-only
        // mode below 0700. Unsafe pre-existing evidence is rejected above
        // and is never chmod-laundered.
        chmod_path_descriptor(&pinned, PRIVATE_DIRECTORY_MODE)?;
    }
    require_controlled_directory(&pinned, path)?;

    let directory = openat2_file(
        parent.as_raw_fd(),
        name.as_c_str(),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_controlled_directory(&directory, path)?;
    require_no_default_acl(&directory, path)?;
    require_same_directory(&pinned, &directory, path)?;
    // A readable descriptor is required for directory fsync on Linux. Always
    // sync both descriptors so retrying after a crash also completes an entry
    // whose earlier parent-directory sync may not have reached stable storage.
    directory.sync_all()?;
    parent.sync_all()?;

    let named = openat2_file(
        parent.as_raw_fd(),
        name.as_c_str(),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_controlled_directory(&named, path)?;
    require_no_default_acl(&directory, path)?;
    require_no_default_acl(&named, path)?;
    require_same_directory(&directory, &named, path)?;
    Ok(ControlledDirectory {
        file: directory,
        path: path.to_owned(),
    })
}

fn mkdirat_if_absent(parent: RawFd, name: &CStr, mode: u32) -> io::Result<()> {
    loop {
        // SAFETY: the parent descriptor and single NUL-terminated component
        // remain live. mkdirat never follows the final component.
        if unsafe { nix::libc::mkdirat(parent, name.as_ptr(), mode) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        match source.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::AlreadyExists => return Ok(()),
            _ => return Err(source),
        }
    }
}

fn requires_private_mode_recovery(file: &std::fs::File, path: &Path) -> io::Result<bool> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;

    // Existing safe 0750/0755-style capability roots remain untouched.
    if require_controlled_directory_metadata(&metadata, path, Uid::effective().as_raw()).is_ok() {
        return Ok(false);
    }

    // A mkdir requested 0700. Under a restrictive umask, a crash can expose
    // only a same-owner directory whose mode is a strict subset of 0700.
    if metadata.file_type().is_dir()
        && metadata.uid() == Uid::effective().as_raw()
        && mode & !PRIVATE_DIRECTORY_MODE == 0
    {
        return Ok(true);
    }

    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "capability root is neither safe nor a recoverable owner-only mkdir residue: {} (uid={}, mode={mode:04o})",
            path.display(),
            metadata.uid()
        ),
    ))
}

fn require_controlled_directory(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    require_controlled_directory_metadata(&metadata, path, Uid::effective().as_raw())
}

fn require_controlled_directory_metadata(
    metadata: &std::fs::Metadata,
    path: &Path,
    expected_owner: u32,
) -> io::Result<()> {
    // POSIX access-ACL effective permissions are reflected through the owning
    // group class mask in st_mode, so shared write access is rejected here as
    // 0020. Default ACLs are a separate inherited authority and are rejected
    // on the retained readable descriptor below.
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != expected_owner
        || mode & 0o7000 != 0
        || mode & 0o700 != 0o700
        || mode & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "capability root is not one safe owner-controlled directory: {} (uid={}, mode={mode:04o})",
                path.display(),
                metadata.uid()
            ),
        ));
    }
    Ok(())
}

fn require_same_directory(first: &std::fs::File, second: &std::fs::File, path: &Path) -> io::Result<()> {
    let first = first.metadata()?;
    let second = second.metadata()?;
    if (first.dev(), first.ino()) != (second.dev(), second.ino()) {
        return Err(io::Error::other(format!(
            "capability root changed while opening: {}",
            path.display()
        )));
    }
    Ok(())
}

fn open_directory_path(path: &Path) -> io::Result<std::fs::File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory path contains NUL"))?;
    openat2_file(
        nix::libc::AT_FDCWD,
        &path,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )
}

fn open_controlled_directory_path(path: &Path) -> io::Result<std::fs::File> {
    let pinned = open_directory_path(path)?;
    require_controlled_directory(&pinned, path)?;

    let encoded = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory path contains NUL"))?;
    let directory = openat2_file(
        nix::libc::AT_FDCWD,
        &encoded,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )?;
    require_controlled_directory(&directory, path)?;
    require_no_default_acl(&directory, path)?;
    require_same_directory(&pinned, &directory, path)?;
    Ok(directory)
}

fn require_named_controlled_directory_path(path: &Path, retained: &std::fs::File) -> io::Result<()> {
    require_controlled_directory(retained, path)?;
    require_no_default_acl(retained, path)?;
    let named = open_controlled_directory_path(path)?;
    require_same_directory(retained, &named, path)
}

fn require_named_controlled_child(
    parent: &std::fs::File,
    name: &CStr,
    retained: &ControlledDirectory,
) -> io::Result<()> {
    require_controlled_directory(&retained.file, &retained.path)?;
    require_no_default_acl(&retained.file, &retained.path)?;
    let named = openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_controlled_directory(&named, &retained.path)?;
    require_no_default_acl(&named, &retained.path)?;
    require_same_directory(&retained.file, &named, &retained.path)
}

fn open_installation_root_path(path: &Path) -> io::Result<std::fs::File> {
    let pinned = open_directory_path(path)?;
    require_installation_root(&pinned, path)?;

    let encoded = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory path contains NUL"))?;
    let directory = openat2_file(
        nix::libc::AT_FDCWD,
        &encoded,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )?;
    require_installation_root(&directory, path)?;
    require_no_default_acl(&directory, path)?;
    require_same_directory(&pinned, &directory, path)?;
    Ok(directory)
}

fn require_named_installation_root(path: &Path, retained: &std::fs::File) -> io::Result<()> {
    require_installation_root(retained, path)?;
    require_no_default_acl(retained, path)?;
    let named = open_installation_root_path(path)?;
    require_same_directory(retained, &named, path)
}

fn require_installation_root(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    require_installation_root_policy(
        metadata.file_type().is_dir(),
        metadata.uid(),
        metadata.mode() & 0o7777,
        Uid::effective().as_raw(),
        path,
    )
}

fn require_installation_root_policy(
    is_directory: bool,
    owner: u32,
    mode: u32,
    effective_owner: u32,
    path: &Path,
) -> io::Result<()> {
    if !is_directory
        || (owner != effective_owner && owner != 0)
        || mode & 0o7000 != 0
        || mode & 0o022 != 0
        || mode & 0o500 != 0o500
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "installation root is not a safe effective-user- or root-owned readable directory: {} (uid={owner}, mode={mode:04o})",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn classify_installation_root_access(owner: u32, mode: u32, effective_owner: u32) -> Mutability {
    if owner == effective_owner && mode & 0o200 != 0 {
        Mutability::ReadWrite
    } else {
        // A root-owned installation opened by an unprivileged caller is
        // intentionally read-only even if some ambient credential mechanism
        // would make a pathname access probe succeed. Provisioning authority
        // is derived only from the authenticated owner and owner-write bit.
        Mutability::ReadOnly
    }
}

fn openat2_file(dirfd: RawFd, path: &CStr, flags: i32, mode: u32, resolve: u64) -> io::Result<std::fs::File> {
    // SAFETY: zero is valid for every public `open_how` field.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: the descriptor, C string, and open_how remain live. Success
    // returns one fresh descriptor owned below.
    let descriptor = loop {
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
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(std::fs::File::from(descriptor))
}

fn controlled_resolution() -> u64 {
    (nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV) as u64
}
