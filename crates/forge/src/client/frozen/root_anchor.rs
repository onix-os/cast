fn open_frozen_root_anchor(root: &Path) -> Result<fs::File, Error> {
    open_frozen_root_anchor_with_deadline(root, None)
}

/// Retain the exact directory selected as the frozen publication namespace.
///
/// `Client::frozen` canonicalizes the authored parent once, then opens it from
/// the filesystem root without following symlinks.  Every later staging,
/// publication, and discard operation is relative to this descriptor; the
/// pathname is retained only so we can fail closed if the public namespace is
/// renamed or replaced while the client is alive.
fn open_frozen_destination_parent(parent: &Path) -> Result<fs::File, Error> {
    let relative = parent
        .strip_prefix(Path::new("/"))
        .ok()
        .filter(|relative| {
            relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        })
        .ok_or_else(|| Error::InvalidFrozenRootDestination(parent.to_owned()))?;
    let system_root = fs::File::open("/").map_err(|source| Error::OpenFrozenRootDestinationParent {
        path: parent.to_owned(),
        source,
    })?;
    let relative = if relative.as_os_str().is_empty() {
        Path::new(".")
    } else {
        relative
    };
    let resolution =
        (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64;
    let pinned = openat2_frozen(
        system_root.as_raw_fd(),
        relative,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        resolution,
    )
    .map_err(|source| Error::OpenFrozenRootDestinationParent {
        path: parent.to_owned(),
        source,
    })?;
    let readable = openat2_frozen(
        system_root.as_raw_fd(),
        relative,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        resolution,
    )
    .map_err(|source| Error::OpenFrozenRootDestinationParent {
        path: parent.to_owned(),
        source,
    })?;
    if frozen_root_identity(&pinned, parent)? != frozen_root_identity(&readable, parent)? {
        return Err(Error::FrozenRootDestinationParentChanged(parent.to_owned()));
    }
    Ok(readable)
}

fn require_frozen_destination_parent(destination: &FrozenRootDestination) -> Result<(), Error> {
    let retained = frozen_root_identity(&destination.parent, &destination.parent_path)?;
    let named = open_frozen_destination_parent(&destination.parent_path)?;
    if retained != destination.parent_identity
        || frozen_root_identity(&named, &destination.parent_path)? != destination.parent_identity
    {
        return Err(Error::FrozenRootDestinationParentChanged(
            destination.parent_path.clone(),
        ));
    }
    Ok(())
}

fn lock_frozen_destination_until(
    destination: &FrozenRootDestination,
    deadline: Instant,
) -> Result<FrozenDestinationLock, Error> {
    require_frozen_materialization_deadline(deadline)?;
    require_frozen_destination_parent(destination)?;
    let directory = openat2_frozen_until(
        destination.parent.as_raw_fd(),
        Path::new("."),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    )
    .map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::LockFrozenRootDestinationParent {
            path: destination.parent_path.clone(),
            source,
        })
    })?;
    if frozen_root_identity(&directory, &destination.parent_path)? != destination.parent_identity {
        return Err(Error::FrozenRootDestinationParentChanged(
            destination.parent_path.clone(),
        ));
    }

    let mut interruptions = 0usize;
    loop {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: directory remains open for the guard's lifetime. LOCK_NB
        // keeps the materialization deadline observable while another
        // cooperating Forge process owns this namespace.
        if unsafe { nix::libc::flock(directory.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) } == 0 {
            break;
        }
        let source = io::Error::last_os_error();
        match source.raw_os_error() {
            Some(code) if code == nix::libc::EWOULDBLOCK || code == nix::libc::EAGAIN => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    require_frozen_materialization_deadline(deadline)?;
                }
                std::thread::sleep(remaining.min(FROZEN_DESTINATION_LOCK_RETRY));
            }
            Some(nix::libc::EINTR) if interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS => {
                interruptions += 1;
            }
            _ => {
                return Err(Error::LockFrozenRootDestinationParent {
                    path: destination.parent_path.clone(),
                    source,
                });
            }
        }
    }
    require_frozen_destination_parent(destination)?;
    Ok(FrozenDestinationLock { _directory: directory })
}

fn open_frozen_root_anchor_until(root: &Path, deadline: Instant) -> Result<fs::File, Error> {
    open_frozen_root_anchor_with_deadline(root, Some(deadline))
}

fn open_frozen_root_anchor_with_deadline(root: &Path, deadline: Option<Instant>) -> Result<fs::File, Error> {
    let relative = root
        .strip_prefix(Path::new("/"))
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .filter(|relative| {
            relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        })
        .ok_or_else(|| Error::InvalidFrozenExecutableRoot(root.to_owned()))?;
    let system_root = fs::File::open("/").map_err(|source| Error::OpenFrozenExecutableRoot {
        path: root.to_owned(),
        source,
    })?;
    let opened = match deadline {
        Some(deadline) => openat2_frozen_until(
            system_root.as_raw_fd(),
            relative,
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC,
            (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
            deadline,
        ),
        None => openat2_frozen(
            system_root.as_raw_fd(),
            relative,
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC,
            (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
        ),
    };
    opened.map_err(|source| match deadline {
        Some(deadline) => frozen_materialization_io_error(deadline, source, |source| Error::OpenFrozenExecutableRoot {
            path: root.to_owned(),
            source,
        }),
        None => Error::OpenFrozenExecutableRoot {
            path: root.to_owned(),
            source,
        },
    })
}

fn frozen_root_anchor_witness(file: &fs::File, path: &Path) -> Result<FrozenExecutableWitness, Error> {
    file.metadata()
        .map(|metadata| FrozenExecutableWitness::from_metadata(&metadata))
        .map_err(|source| Error::StatFrozenExecutableRoot {
            path: path.to_owned(),
            source,
        })
}

fn frozen_root_identity(file: &fs::File, path: &Path) -> Result<FrozenRootIdentity, Error> {
    file.metadata()
        .map(|metadata| FrozenRootIdentity::from_metadata(&metadata))
        .map_err(|source| Error::StatFrozenExecutableRoot {
            path: path.to_owned(),
            source,
        })
}

fn require_materialized_frozen_root(path: &Path, pinned: &fs::File, expected: FrozenRootIdentity) -> Result<(), Error> {
    let descriptor = frozen_root_identity(pinned, path)?;
    let Ok(reopened) = open_frozen_root_anchor(path) else {
        return Err(Error::MaterializedFrozenRootReplaced(path.to_owned()));
    };
    let named = frozen_root_identity(&reopened, path)?;
    if descriptor != expected || named != expected {
        return Err(Error::MaterializedFrozenRootReplaced(path.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
fn test_materialized_frozen_root(path: &Path) -> Result<MaterializedFrozenRoot, Error> {
    let root = open_frozen_root_anchor(path)?;
    let identity = frozen_root_identity(&root, path)?;
    Ok(MaterializedFrozenRoot {
        root_path: path.to_owned(),
        root,
        identity,
    })
}

fn require_pinned_frozen_root_anchor(
    path: &Path,
    pinned: &fs::File,
    expected: FrozenExecutableWitness,
) -> Result<(), Error> {
    let descriptor = frozen_root_anchor_witness(pinned, path)?;
    let Ok(reopened) = open_frozen_root_anchor(path) else {
        return Err(Error::FrozenExecutableRootReplaced(path.to_owned()));
    };
    let named = frozen_root_anchor_witness(&reopened, path)?;
    if descriptor != expected || named != expected {
        return Err(Error::FrozenExecutableRootReplaced(path.to_owned()));
    }
    Ok(())
}

fn open_frozen_executable(
    root: &fs::File,
    binding: &FrozenExecutableBinding,
    resolved_path: &Path,
) -> Result<fs::File, Error> {
    let relative = resolved_path
        .strip_prefix(Path::new("/"))
        .map_err(|_| Error::InvalidFrozenExecutablePath {
            package: binding.package.clone(),
            path: binding.path.clone(),
        })?;
    openat2_frozen(
        root.as_raw_fd(),
        relative,
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
    )
    .map_err(|source| Error::OpenFrozenExecutable {
        package: binding.package.clone(),
        path: binding.path.clone(),
        source,
    })
}

fn open_frozen_symlink(root: &fs::File, binding: &FrozenExecutableBinding, path: &Path) -> Result<fs::File, Error> {
    let relative = path
        .strip_prefix(Path::new("/"))
        .map_err(|_| Error::InvalidFrozenExecutablePath {
            package: binding.package.clone(),
            path: path.to_owned(),
        })?;
    openat2_frozen(
        root.as_raw_fd(),
        relative,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
    )
    .map_err(|source| Error::OpenFrozenExecutableSymlink {
        package: binding.package.clone(),
        path: path.to_owned(),
        source,
    })
}

fn openat2_frozen(dirfd: RawFd, path: &Path, flags: i32, resolve: u64) -> io::Result<fs::File> {
    let display_path = path.to_owned();
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let file = openat2_file(dirfd, &path, flags, 0, resolve)?;
    Ok(fs::File::from_parts(file, display_path))
}

fn openat2_frozen_until(
    dirfd: RawFd,
    path: &Path,
    flags: i32,
    resolve: u64,
    deadline: Instant,
) -> io::Result<fs::File> {
    let display_path = path.to_owned();
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let file = openat2_file_until(dirfd, &path, flags, 0, resolve, deadline)?;
    Ok(fs::File::from_parts(file, display_path))
}

fn frozen_executable_witness(
    file: &fs::File,
    binding: &FrozenExecutableBinding,
) -> Result<FrozenExecutableWitness, Error> {
    file.metadata()
        .map(|metadata| FrozenExecutableWitness::from_metadata(&metadata))
        .map_err(|source| Error::StatFrozenExecutable {
            package: binding.package.clone(),
            path: binding.path.clone(),
            source,
        })
}

fn frozen_symlink_witness(
    file: &fs::File,
    binding: &FrozenExecutableBinding,
    path: &Path,
) -> Result<FrozenExecutableWitness, Error> {
    file.metadata()
        .map(|metadata| FrozenExecutableWitness::from_metadata(&metadata))
        .map_err(|source| Error::StatFrozenExecutableSymlink {
            package: binding.package.clone(),
            path: path.to_owned(),
            source,
        })
}

fn read_frozen_symlink(file: &fs::File, binding: &FrozenExecutableBinding, path: &Path) -> Result<OsString, Error> {
    let mut target = [0_u8; MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1];
    // Linux reads the exact symlink pinned by an O_PATH|O_NOFOLLOW descriptor
    // when readlinkat receives an empty relative path.
    // SAFETY: `file` is live and `target` is writable for its complete length.
    let read =
        unsafe { nix::libc::readlinkat(file.as_raw_fd(), c"".as_ptr(), target.as_mut_ptr().cast(), target.len()) };
    if read < 0 {
        return Err(Error::ReadFrozenExecutableSymlink {
            package: binding.package.clone(),
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let read = usize::try_from(read).map_err(|_| Error::ReadFrozenExecutableSymlink {
        package: binding.package.clone(),
        path: path.to_owned(),
        source: io::Error::other("readlinkat returned a negative size"),
    })?;
    if read > MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES {
        return Err(Error::FrozenExecutableSymlinkTargetTooLong {
            package: binding.package.clone(),
            path: path.to_owned(),
            limit: MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES,
            actual: read,
        });
    }
    Ok(OsString::from_vec(target[..read].to_vec()))
}

fn digest_frozen_executable(
    file: &mut fs::File,
    expected_length: u64,
    deadline: Instant,
    binding: &FrozenExecutableBinding,
) -> Result<FrozenExecutableDigest, Error> {
    let mut hasher = StoneDigestWriterHasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut actual = 0u64;
    let mut shebang_probe = Vec::with_capacity(MAX_FROZEN_SHEBANG_LINE_BYTES + 1);
    loop {
        require_frozen_executable_deadline(deadline)?;
        let read = file.read(&mut buffer).map_err(|source| Error::ReadFrozenExecutable {
            package: binding.package.clone(),
            path: binding.path.clone(),
            source,
        })?;
        if read == 0 {
            break;
        }
        actual = actual
            .checked_add(read as u64)
            .ok_or_else(|| Error::FrozenExecutableLengthChanged {
                package: binding.package.clone(),
                path: binding.path.clone(),
                expected: expected_length,
                actual: u64::MAX,
            })?;
        if actual > expected_length {
            return Err(Error::FrozenExecutableLengthChanged {
                package: binding.package.clone(),
                path: binding.path.clone(),
                expected: expected_length,
                actual,
            });
        }
        hasher.update(&buffer[..read]);
        let remaining = (MAX_FROZEN_SHEBANG_LINE_BYTES + 1).saturating_sub(shebang_probe.len());
        shebang_probe.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    if actual != expected_length {
        return Err(Error::FrozenExecutableLengthChanged {
            package: binding.package.clone(),
            path: binding.path.clone(),
            expected: expected_length,
            actual,
        });
    }
    Ok(FrozenExecutableDigest {
        digest: hasher.digest128(),
        shebang_probe,
    })
}

fn require_frozen_executable_deadline(deadline: Instant) -> Result<(), Error> {
    if Instant::now() > deadline {
        Err(Error::FrozenExecutableVerificationTimeout {
            seconds: FROZEN_EXECUTABLE_VERIFICATION_TIMEOUT.as_secs(),
        })
    } else {
        Ok(())
    }
}

fn require_frozen_materialization_deadline(deadline: Instant) -> Result<(), Error> {
    if Instant::now() > deadline {
        Err(Error::FrozenMaterializationTimeout {
            seconds: FROZEN_MATERIALIZATION_TIMEOUT.as_secs(),
        })
    } else {
        Ok(())
    }
}

fn frozen_namespace_recovery_deadline() -> Instant {
    Instant::now() + FROZEN_NAMESPACE_RECOVERY_TIMEOUT
}

fn frozen_materialization_io_error(
    deadline: Instant,
    source: io::Error,
    map: impl FnOnce(io::Error) -> Error,
) -> Error {
    match require_frozen_materialization_deadline(deadline) {
        Err(timeout) => timeout,
        Ok(()) => map(source),
    }
}

fn require_blit_deadline(deadline: Option<Instant>) -> Result<(), Error> {
    if let Some(deadline) = deadline {
        require_frozen_materialization_deadline(deadline)?;
    }
    Ok(())
}
