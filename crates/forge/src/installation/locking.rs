fn require_open_directories_still_named(
    root_path: &Path,
    root: &std::fs::File,
    cast: Option<&ControlledDirectory>,
    custom_cache: Option<&ControlledDirectory>,
) -> Result<(), Error> {
    require_named_installation_root(root_path, root).map_err(|source| Error::ValidateRootDirectory {
        path: root_path.to_owned(),
        source,
    })?;
    if let Some(cast) = cast {
        require_named_controlled_child(root, CAST_DIRECTORY_NAME, cast).map_err(|source| Error::PrepareDirectory {
            path: cast.path.clone(),
            source,
        })?;
    }
    if let Some(cache) = custom_cache {
        require_named_controlled_directory_path(&cache.path, &cache.file).map_err(|source| {
            Error::ValidateCacheDirectory {
                path: cache.path.clone(),
                source,
            }
        })?;
    }
    Ok(())
}

fn acquire_controlled_locks(
    cast: &ControlledDirectory,
    custom_cache: Option<&ControlledDirectory>,
) -> Result<Vec<lockfile::Lock>, Error> {
    let mut locks = Vec::with_capacity(1 + usize::from(custom_cache.is_some()));
    locks.push(acquire_controlled_lock(
        &cast.file,
        &cast.path.join(LOCKFILE_NAME.to_string_lossy().as_ref()),
        format!("{} another process is using the Cast root", "Blocking".yellow().bold()),
    )?);

    if let Some(cache) = custom_cache {
        locks.push(acquire_controlled_lock(
            &cache.file,
            &cache.path.join(LOCKFILE_NAME.to_string_lossy().as_ref()),
            format!("{} another process is using the cache dir", "Blocking".yellow().bold()),
        )?);
    }
    Ok(locks)
}

fn acquire_controlled_lock(
    directory: &std::fs::File,
    path: &Path,
    block_message: impl fmt::Display,
) -> Result<lockfile::Lock, Error> {
    let (file, expected_identity) =
        open_controlled_lockfile(directory, path).map_err(|source| Error::PrepareLockfile {
            path: path.to_owned(),
            source,
        })?;
    let lock = lockfile::acquire_file(file, block_message)?;
    openat2_file(
        directory.as_raw_fd(),
        LOCKFILE_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .and_then(|file| {
        require_controlled_lockfile(&file, path)?;
        require_lockfile_identity(expected_identity, lockfile_identity(&file)?)
    })
    .map_err(|source| Error::PrepareLockfile {
        path: path.to_owned(),
        source,
    })?;
    Ok(lock)
}

fn open_controlled_lockfile(directory: &std::fs::File, path: &Path) -> io::Result<(std::fs::File, (u64, u64))> {
    loop {
        match openat2_file(
            directory.as_raw_fd(),
            LOCKFILE_NAME,
            nix::libc::O_RDWR
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK
                | nix::libc::O_CREAT
                | nix::libc::O_EXCL,
            LOCKFILE_MODE,
            controlled_resolution(),
        ) {
            Ok(file) => {
                require_fresh_lockfile(&file, path)?;
                file.set_permissions(std::fs::Permissions::from_mode(LOCKFILE_MODE))?;
                require_controlled_lockfile(&file, path)?;
                file.sync_all()?;
                directory.sync_all()?;
                let identity = lockfile_identity(&file)?;
                let named = openat2_file(
                    directory.as_raw_fd(),
                    LOCKFILE_NAME,
                    nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                    0,
                    controlled_resolution(),
                )?;
                require_controlled_lockfile(&named, path)?;
                require_lockfile_identity(identity, lockfile_identity(&named)?)?;
                return Ok((file, identity));
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                let probe = match openat2_file(
                    directory.as_raw_fd(),
                    LOCKFILE_NAME,
                    nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                    0,
                    controlled_resolution(),
                ) {
                    Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                    result => result?,
                };
                if require_controlled_lockfile(&probe, path).is_err() {
                    require_fresh_lockfile(&probe, path)?;
                    chmod_path_descriptor(&probe, LOCKFILE_MODE)?;
                    require_controlled_lockfile(&probe, path)?;
                }
                let identity = lockfile_identity(&probe)?;
                let file = match openat2_file(
                    directory.as_raw_fd(),
                    LOCKFILE_NAME,
                    nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
                    0,
                    controlled_resolution(),
                ) {
                    Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                    result => result?,
                };
                require_controlled_lockfile(&file, path)?;
                require_lockfile_identity(identity, lockfile_identity(&file)?)?;
                file.sync_all()?;
                directory.sync_all()?;
                return Ok((file, identity));
            }
            Err(source) => return Err(source),
        }
    }
}

fn require_fresh_lockfile(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if metadata.file_type().is_file()
        && metadata.uid() == Uid::effective().as_raw()
        && metadata.nlink() == 1
        && mode & !LOCKFILE_MODE == 0
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "fresh lockfile is not recoverable owner-only creation residue: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ))
    }
}

fn require_controlled_lockfile(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if metadata.file_type().is_file()
        && metadata.uid() == Uid::effective().as_raw()
        && metadata.nlink() == 1
        && mode & 0o7000 == 0
        && mode & 0o022 == 0
        && mode & 0o600 == 0o600
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "lockfile is not one safe owner-controlled regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ))
    }
}

fn lockfile_identity(file: &std::fs::File) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

fn require_lockfile_identity(expected: (u64, u64), actual: (u64, u64)) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::other("lockfile inode changed during acquisition"))
    }
}
