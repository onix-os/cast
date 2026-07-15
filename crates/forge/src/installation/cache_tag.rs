#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CachedirTagIdentity {
    device: u64,
    inode: u64,
}

/// Publish a complete cache tag atomically. The canonical name is never an
/// incomplete file: bytes and mode are finalized and fsynced on a private
/// temporary inode before `RENAME_NOREPLACE` exposes it.
fn ensure_cachedir_tag(cache: &ControlledDirectory) -> io::Result<()> {
    let canonical_path = cache.path.join("CACHEDIR.TAG");
    if let Some(mut canonical) = open_cachedir_tag(cache, CACHEDIR_TAG_NAME)? {
        sync_exact_cachedir_tag(&mut canonical, &canonical_path)?;
        cleanup_cachedir_tag_residue(cache)?;
        return cache.file.sync_all();
    }

    let Some(mut temporary) = prepare_cachedir_tag_temporary(cache)? else {
        let mut canonical = open_cachedir_tag(cache, CACHEDIR_TAG_NAME)?
            .ok_or_else(|| io::Error::other("cache tag appeared and then disappeared during preparation"))?;
        sync_exact_cachedir_tag(&mut canonical, &canonical_path)?;
        return cache.file.sync_all();
    };
    let identity = cachedir_tag_identity(&temporary)?;
    let temporary_path = cache.path.join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref());

    if require_exact_cachedir_tag(&mut temporary, &temporary_path, CACHEDIR_TAG_MODE).is_err() {
        let build = (|| {
            temporary.set_len(0)?;
            temporary.seek(SeekFrom::Start(0))?;
            temporary.set_permissions(std::fs::Permissions::from_mode(CACHEDIR_TAG_TEMPORARY_MODE))?;
            temporary.write_all(CACHEDIR_TAG_CONTENTS)?;
            temporary.sync_all()?;
            temporary.set_permissions(std::fs::Permissions::from_mode(CACHEDIR_TAG_MODE))?;
            temporary.sync_all()?;
            require_exact_cachedir_tag(&mut temporary, &temporary_path, CACHEDIR_TAG_MODE)
        })();
        if let Err(source) = build {
            return Err(cleanup_cachedir_tag_after_failure(cache, identity, source));
        }
    }
    // A complete residue may have been left before its creator reached
    // fsync. Complete the inode durability step on every retry before the
    // atomic namespace publication.
    if let Err(source) = temporary.sync_all() {
        return Err(cleanup_cachedir_tag_after_failure(cache, identity, source));
    }

    match renameat2_noreplace(cache.file.as_raw_fd(), CACHEDIR_TAG_TEMPORARY_NAME, CACHEDIR_TAG_NAME) {
        Ok(()) => {
            let mut canonical = open_cachedir_tag(cache, CACHEDIR_TAG_NAME)?
                .ok_or_else(|| io::Error::other("published cache tag disappeared"))?;
            require_same_cachedir_tag(identity, cachedir_tag_identity(&canonical)?)?;
            sync_exact_cachedir_tag(&mut canonical, &canonical_path)?;
            cache.file.sync_all()
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            cleanup_cachedir_tag_identity(cache, identity)?;
            let mut canonical = open_cachedir_tag(cache, CACHEDIR_TAG_NAME)?
                .ok_or_else(|| io::Error::other("competing cache tag disappeared"))?;
            sync_exact_cachedir_tag(&mut canonical, &canonical_path)?;
            cache.file.sync_all()
        }
        Err(source) => Err(cleanup_cachedir_tag_after_failure(cache, identity, source)),
    }
}

fn prepare_cachedir_tag_temporary(cache: &ControlledDirectory) -> io::Result<Option<std::fs::File>> {
    loop {
        if open_cachedir_tag(cache, CACHEDIR_TAG_NAME)?.is_some() {
            return Ok(None);
        }

        let flags = nix::libc::O_RDWR
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_CREAT
            | nix::libc::O_EXCL;
        match openat2_file(
            cache.file.as_raw_fd(),
            CACHEDIR_TAG_TEMPORARY_NAME,
            flags,
            CACHEDIR_TAG_TEMPORARY_MODE,
            controlled_resolution(),
        ) {
            Ok(file) => {
                let identity = cachedir_tag_identity(&file)?;
                if let Err(source) = flock_exclusive(&file)
                    .and_then(|()| file.set_permissions(std::fs::Permissions::from_mode(CACHEDIR_TAG_TEMPORARY_MODE)))
                {
                    return Err(cleanup_cachedir_tag_after_failure(cache, identity, source));
                }
                return Ok(Some(file));
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                let Some((mut file, identity)) = lock_existing_cachedir_tag_temporary(cache)? else {
                    continue;
                };
                let path = cache.path.join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref());
                if require_exact_cachedir_tag(&mut file, &path, CACHEDIR_TAG_MODE).is_ok() {
                    return Ok(Some(file));
                }
                cleanup_cachedir_tag_identity(cache, identity)?;
            }
            Err(source) => return Err(source),
        }
    }
}

fn cleanup_cachedir_tag_residue(cache: &ControlledDirectory) -> io::Result<()> {
    let Some((_file, identity)) = lock_existing_cachedir_tag_temporary(cache)? else {
        return Ok(());
    };
    cleanup_cachedir_tag_identity(cache, identity)
}

fn lock_existing_cachedir_tag_temporary(
    cache: &ControlledDirectory,
) -> io::Result<Option<(std::fs::File, CachedirTagIdentity)>> {
    let pinned = match openat2_file(
        cache.file.as_raw_fd(),
        CACHEDIR_TAG_TEMPORARY_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source),
    };
    let mode = require_safe_cachedir_tag_temporary(&pinned, &cache.path.join(".CACHEDIR.TAG.cast-tmp"))?;
    if mode != CACHEDIR_TAG_TEMPORARY_MODE && mode != CACHEDIR_TAG_MODE {
        chmod_path_descriptor(&pinned, CACHEDIR_TAG_TEMPORARY_MODE)?;
    }
    let identity = cachedir_tag_identity(&pinned)?;
    let file = openat2_file(
        cache.file.as_raw_fd(),
        CACHEDIR_TAG_TEMPORARY_NAME,
        nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_same_cachedir_tag(identity, cachedir_tag_identity(&file)?)?;
    flock_exclusive(&file)?;

    let named = match openat2_file(
        cache.file.as_raw_fd(),
        CACHEDIR_TAG_TEMPORARY_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source),
    };
    require_same_cachedir_tag(identity, cachedir_tag_identity(&named)?)?;
    Ok(Some((file, identity)))
}

fn open_cachedir_tag(cache: &ControlledDirectory, name: &CStr) -> io::Result<Option<std::fs::File>> {
    // Pin and validate the final inode without opening it for data access.
    // O_PATH is side-effect-free for devices, FIFOs, sockets, directories,
    // and symlinks, all of which must be rejected before O_RDONLY is allowed.
    let pinned = match openat2_file(
        cache.file.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source),
    };
    let path = cache.path.join(name.to_string_lossy().as_ref());
    require_exact_cachedir_tag_metadata(&pinned, &path, CACHEDIR_TAG_MODE)?;
    let identity = cachedir_tag_identity(&pinned)?;

    let file = openat2_file(
        cache.file.as_raw_fd(),
        name,
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_same_cachedir_tag(identity, cachedir_tag_identity(&file)?)?;
    require_exact_cachedir_tag_metadata(&file, &path, CACHEDIR_TAG_MODE)?;
    Ok(Some(file))
}

fn require_safe_cachedir_tag_temporary(file: &std::fs::File, path: &Path) -> io::Result<u32> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    let recoverable_mode = mode & !CACHEDIR_TAG_TEMPORARY_MODE == 0 || mode == CACHEDIR_TAG_MODE;
    if !metadata.file_type().is_file()
        || metadata.uid() != Uid::effective().as_raw()
        || metadata.nlink() != 1
        || !recoverable_mode
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "cache-tag temporary is not one safely recoverable regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ));
    }
    Ok(mode)
}

fn require_exact_cachedir_tag(file: &mut std::fs::File, path: &Path, expected_mode: u32) -> io::Result<()> {
    require_exact_cachedir_tag_metadata(file, path, expected_mode)?;

    file.seek(SeekFrom::Start(0))?;
    let mut contents = Vec::with_capacity(CACHEDIR_TAG_CONTENTS.len() + 1);
    file.take(CACHEDIR_TAG_CONTENTS.len() as u64 + 1)
        .read_to_end(&mut contents)?;
    if contents != CACHEDIR_TAG_CONTENTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cache tag has noncanonical contents: {}", path.display()),
        ));
    }
    Ok(())
}

fn require_exact_cachedir_tag_metadata(file: &std::fs::File, path: &Path, expected_mode: u32) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.uid() != Uid::effective().as_raw()
        || metadata.nlink() != 1
        || mode != expected_mode
        || metadata.len() != CACHEDIR_TAG_CONTENTS.len() as u64
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "cache tag is not one exact owner-controlled regular file: {} (uid={}, mode={mode:04o}, links={}, bytes={})",
                path.display(),
                metadata.uid(),
                metadata.nlink(),
                metadata.len()
            ),
        ));
    }
    Ok(())
}

fn sync_exact_cachedir_tag(file: &mut std::fs::File, path: &Path) -> io::Result<()> {
    require_exact_cachedir_tag(file, path, CACHEDIR_TAG_MODE)?;
    file.sync_all()
}

fn cachedir_tag_identity(file: &std::fs::File) -> io::Result<CachedirTagIdentity> {
    let metadata = file.metadata()?;
    Ok(CachedirTagIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn require_same_cachedir_tag(expected: CachedirTagIdentity, actual: CachedirTagIdentity) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::other("cache-tag inode changed during atomic publication"))
    }
}

fn cleanup_cachedir_tag_identity(cache: &ControlledDirectory, expected: CachedirTagIdentity) -> io::Result<()> {
    let named = openat2_file(
        cache.file.as_raw_fd(),
        CACHEDIR_TAG_TEMPORARY_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    require_same_cachedir_tag(expected, cachedir_tag_identity(&named)?)?;
    unlinkat_name(cache.file.as_raw_fd(), CACHEDIR_TAG_TEMPORARY_NAME)?;
    cache.file.sync_all()
}

fn cleanup_cachedir_tag_after_failure(
    cache: &ControlledDirectory,
    identity: CachedirTagIdentity,
    source: io::Error,
) -> io::Error {
    match cleanup_cachedir_tag_identity(cache, identity) {
        Ok(()) => source,
        Err(cleanup) => io::Error::new(
            source.kind(),
            format!("{source}; retained cache-tag temporary cleanup also failed: {cleanup}"),
        ),
    }
}

fn renameat2_noreplace(directory: RawFd, from: &CStr, to: &CStr) -> io::Result<()> {
    loop {
        // SAFETY: the directory and both fixed single-component names remain
        // live. RENAME_NOREPLACE either publishes the retained inode or does
        // not change either name.
        let result = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_renameat2,
                directory,
                from.as_ptr(),
                directory,
                to.as_ptr(),
                nix::libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

fn unlinkat_name(directory: RawFd, name: &CStr) -> io::Result<()> {
    loop {
        // SAFETY: the directory and single-component name remain live. flags
        // zero unlinks a non-directory without following its final component.
        if unsafe { nix::libc::unlinkat(directory, name.as_ptr(), 0) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

fn flock_exclusive(file: &std::fs::File) -> io::Result<()> {
    loop {
        // SAFETY: flock operates on the live temporary-file descriptor.
        if unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}
