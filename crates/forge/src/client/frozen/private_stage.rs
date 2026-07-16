#[derive(Debug)]
struct FrozenPrivateDirectory {
    name: CString,
    path: PathBuf,
    file: fs::File,
    identity: FrozenRootIdentity,
}

fn open_frozen_named_entry_until(
    parent: &fs::File,
    name: &CStr,
    path: &Path,
    deadline: Instant,
) -> Result<Option<fs::File>, Error> {
    let relative = Path::new(OsStr::from_bytes(name.to_bytes()));
    match openat2_frozen_until(
        parent.as_raw_fd(),
        relative,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    ) {
        Ok(file) => Ok(Some(file)),
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
        Err(source) => Err(frozen_materialization_io_error(deadline, source, |source| {
            Error::InspectFrozenPublicationName {
                path: path.to_owned(),
                source,
            }
        })),
    }
}

fn frozen_named_identity_until(
    parent: &fs::File,
    name: &CStr,
    path: &Path,
    deadline: Instant,
) -> Result<Option<FrozenRootIdentity>, Error> {
    open_frozen_named_entry_until(parent, name, path, deadline)?
        .map(|file| frozen_root_identity(&file, path))
        .transpose()
}

fn random_frozen_private_name(prefix: &[u8], deadline: Instant) -> Result<CString, Error> {
    let mut random = [0_u8; FROZEN_PRIVATE_DIRECTORY_RANDOM_BYTES];
    let mut filled = 0usize;
    let mut interruptions = 0usize;
    while filled < random.len() {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: the remaining slice is writable for the supplied length.
        // GRND_NONBLOCK avoids an unbounded entropy wait inside a supposedly
        // finite materialization operation.
        let result = unsafe {
            syscall(
                nix::libc::SYS_getrandom,
                random[filled..].as_mut_ptr(),
                random.len() - filled,
                nix::libc::GRND_NONBLOCK,
            )
        };
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted && interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS {
                interruptions += 1;
                continue;
            }
            return Err(source.into());
        }
        let read = usize::try_from(result).map_err(|_| io::Error::other("getrandom returned an invalid length"))?;
        if read == 0 || read > random.len() - filled {
            return Err(
                io::Error::new(io::ErrorKind::UnexpectedEof, "getrandom returned an invalid short read").into(),
            );
        }
        filled += read;
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = Vec::with_capacity(prefix.len() + random.len() * 2);
    encoded.extend_from_slice(prefix);
    for byte in random {
        encoded.push(HEX[usize::from(byte >> 4)]);
        encoded.push(HEX[usize::from(byte & 0x0f)]);
    }
    CString::new(encoded).map_err(|_| io::Error::other("generated frozen private name contains NUL").into())
}

fn create_frozen_private_directory(
    destination: &FrozenRootDestination,
    prefix: &[u8],
    deadline: Instant,
) -> Result<FrozenPrivateDirectory, Error> {
    create_frozen_private_directory_with(destination, prefix, deadline, |_, _| Ok(()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrozenPrivateDirectoryCheckpoint {
    Retained,
    ModeNormalized,
    ReadableOpened,
    AclsChecked,
    InventoryVerified,
}

#[derive(Debug)]
struct ProvisionalFrozenPrivateDirectory {
    name: CString,
    path: PathBuf,
    pinned: fs::File,
    device: u64,
    inode: u64,
}

fn create_frozen_private_directory_with(
    destination: &FrozenRootDestination,
    prefix: &[u8],
    deadline: Instant,
    mut checkpoint: impl FnMut(FrozenPrivateDirectoryCheckpoint, &Path) -> Result<(), Error>,
) -> Result<FrozenPrivateDirectory, Error> {
    'attempts: for _ in 0..MAX_FROZEN_PRIVATE_DIRECTORY_ATTEMPTS {
        require_frozen_materialization_deadline(deadline)?;
        let name = random_frozen_private_name(prefix, deadline)?;
        let path = destination.parent_path.join(OsStr::from_bytes(name.to_bytes()));
        let mut interruptions = 0usize;
        loop {
            require_frozen_materialization_deadline(deadline)?;
            // SAFETY: the retained parent and generated single-component name
            // remain live. mkdirat never follows or replaces the final name.
            if unsafe { nix::libc::mkdirat(destination.parent.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
                break;
            }
            let source = io::Error::last_os_error();
            match source.kind() {
                io::ErrorKind::Interrupted if interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS => {
                    interruptions += 1;
                }
                io::ErrorKind::AlreadyExists => continue 'attempts,
                _ => {
                    return Err(Error::CreateFrozenPrivateDirectory { path, source });
                }
            }
        }

        // Once mkdirat has changed the namespace, retaining or cleaning that
        // exact residue is recovery work. It gets a fresh finite budget even
        // when ordinary materialization time expired immediately after mkdir.
        let provisional = match retain_provisional_frozen_private_directory(
            destination,
            &name,
            &path,
            frozen_namespace_recovery_deadline(),
        ) {
            Ok(Some(provisional)) => provisional,
            Ok(None) => continue 'attempts,
            Err(primary) => {
                let cleanup = retain_provisional_frozen_private_directory(
                    destination,
                    &name,
                    &path,
                    frozen_namespace_recovery_deadline(),
                );
                return match cleanup {
                    Ok(None) => Err(primary),
                    Ok(Some(provisional)) => {
                        match cleanup_provisional_frozen_private_directory(
                            destination,
                            &provisional,
                            frozen_namespace_recovery_deadline(),
                        ) {
                            Ok(()) => Err(primary),
                            Err(cleanup) => Err(Error::CleanupFrozenPrivateDirectory {
                                path: provisional.path,
                                primary: Box::new(primary),
                                cleanup: Box::new(cleanup),
                            }),
                        }
                    }
                    Err(cleanup) => Err(Error::CleanupFrozenPrivateDirectory {
                        path: destination.parent_path.clone(),
                        primary: Box::new(primary),
                        cleanup: Box::new(cleanup),
                    }),
                };
            }
        };

        let result = finish_frozen_private_directory(destination, provisional, deadline, &mut checkpoint);
        return match result {
            Ok(directory) => Ok(directory),
            Err((primary, provisional)) => {
                match cleanup_provisional_frozen_private_directory(
                    destination,
                    &provisional,
                    frozen_namespace_recovery_deadline(),
                ) {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(Error::CleanupFrozenPrivateDirectory {
                        path: provisional.path,
                        primary: Box::new(primary),
                        cleanup: Box::new(cleanup),
                    }),
                }
            }
        };
    }
    Err(Error::CreateFrozenPrivateDirectory {
        path: destination.parent_path.clone(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "failed to reserve a unique private directory after {MAX_FROZEN_PRIVATE_DIRECTORY_ATTEMPTS} attempts"
            ),
        ),
    })
}

fn retain_provisional_frozen_private_directory(
    destination: &FrozenRootDestination,
    name: &CStr,
    path: &Path,
    deadline: Instant,
) -> Result<Option<ProvisionalFrozenPrivateDirectory>, Error> {
    require_frozen_materialization_deadline(deadline)?;
    let resolution = (nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_XDEV) as u64;
    let relative = Path::new(OsStr::from_bytes(name.to_bytes()));
    let pinned = match openat2_frozen_until(
        destination.parent.as_raw_fd(),
        relative,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        resolution,
        deadline,
    ) {
        Ok(pinned) => pinned,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::OpenFrozenPrivateDirectory {
                path: path.to_owned(),
                source,
            });
        }
    };
    let parent_metadata = destination.parent.metadata()?;
    let metadata = pinned.metadata()?;
    let mode = metadata.mode() & 0o7777;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    // A setgid parent may cause the fresh child to inherit S_ISGID. It is a
    // valid creation residue and is cleared through the retained descriptor
    // before the wrapper becomes usable. No other extra permission is
    // accepted.
    if !metadata.is_dir()
        || metadata.uid() != effective_owner
        || metadata.dev() != parent_metadata.dev()
        || mode & !(0o700 | nix::libc::S_ISGID) != 0
    {
        return Err(Error::FrozenPrivateDirectoryChanged { path: path.to_owned() });
    }
    Ok(Some(ProvisionalFrozenPrivateDirectory {
        name: name.to_owned(),
        path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
        pinned,
    }))
}

fn finish_frozen_private_directory(
    destination: &FrozenRootDestination,
    provisional: ProvisionalFrozenPrivateDirectory,
    deadline: Instant,
    checkpoint: &mut impl FnMut(FrozenPrivateDirectoryCheckpoint, &Path) -> Result<(), Error>,
) -> Result<FrozenPrivateDirectory, (Error, ProvisionalFrozenPrivateDirectory)> {
    let result = (|| -> Result<FrozenPrivateDirectory, Error> {
        require_frozen_materialization_deadline(deadline)?;
        checkpoint(FrozenPrivateDirectoryCheckpoint::Retained, &provisional.path)?;
        let resolution = (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64;
        let relative = Path::new(OsStr::from_bytes(provisional.name.to_bytes()));
        chmod_path_descriptor_until(provisional.pinned.file(), 0o700, deadline).map_err(|source| {
            frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenPrivateDirectory {
                path: provisional.path.clone(),
                source,
            })
        })?;
        checkpoint(FrozenPrivateDirectoryCheckpoint::ModeNormalized, &provisional.path)?;
        let readable = openat2_frozen_until(
            destination.parent.as_raw_fd(),
            relative,
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            resolution,
            deadline,
        )
        .map_err(|source| Error::OpenFrozenPrivateDirectory {
            path: provisional.path.clone(),
            source,
        })?;
        let identity = frozen_root_identity(&provisional.pinned, &provisional.path)?;
        if identity.device != provisional.device
            || identity.inode != provisional.inode
            || identity != frozen_root_identity(&readable, &provisional.path)?
            || identity.mode & 0o7777 != 0o700
        {
            return Err(Error::FrozenPrivateDirectoryChanged {
                path: provisional.path.clone(),
            });
        }
        checkpoint(FrozenPrivateDirectoryCheckpoint::ReadableOpened, &provisional.path)?;
        require_no_access_acl_until(readable.file(), &provisional.path, deadline).map_err(|source| {
            frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenPrivateDirectory {
                path: provisional.path.clone(),
                source,
            })
        })?;
        require_no_default_acl_until(readable.file(), &provisional.path, deadline).map_err(|source| {
            frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenPrivateDirectory {
                path: provisional.path.clone(),
                source,
            })
        })?;
        checkpoint(FrozenPrivateDirectoryCheckpoint::AclsChecked, &provisional.path)?;
        let mut entries = 0usize;
        if !frozen_discard_entry_names(readable.as_raw_fd(), &mut entries, deadline)?.is_empty() {
            return Err(Error::FrozenPrivateDirectoryChanged {
                path: provisional.path.clone(),
            });
        }
        checkpoint(FrozenPrivateDirectoryCheckpoint::InventoryVerified, &provisional.path)?;
        Ok(FrozenPrivateDirectory {
            name: provisional.name.clone(),
            path: provisional.path.clone(),
            file: readable,
            identity,
        })
    })();
    result.map_err(|error| (error, provisional))
}

fn cleanup_provisional_frozen_private_directory(
    destination: &FrozenRootDestination,
    provisional: &ProvisionalFrozenPrivateDirectory,
    deadline: Instant,
) -> Result<(), Error> {
    require_frozen_materialization_deadline(deadline)?;
    let Some(named) =
        open_frozen_named_entry_until(&destination.parent, &provisional.name, &provisional.path, deadline)?
    else {
        return Ok(());
    };
    let metadata = named.metadata()?;
    if metadata.dev() != provisional.device || metadata.ino() != provisional.inode || !metadata.is_dir() {
        return Err(Error::FrozenPrivateDirectoryChanged {
            path: provisional.path.clone(),
        });
    }
    let readable = openat2_frozen_until(
        destination.parent.as_raw_fd(),
        Path::new(OsStr::from_bytes(provisional.name.to_bytes())),
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
    .map_err(|source| Error::OpenFrozenPrivateDirectory {
        path: provisional.path.clone(),
        source,
    })?;
    let readable_metadata = readable.metadata()?;
    if (readable_metadata.dev(), readable_metadata.ino()) != (provisional.device, provisional.inode) {
        return Err(Error::FrozenPrivateDirectoryChanged {
            path: provisional.path.clone(),
        });
    }
    let mut entries = 0usize;
    if !frozen_discard_entry_names(readable.as_raw_fd(), &mut entries, deadline)?.is_empty() {
        return Err(Error::FrozenPrivateDirectoryChanged {
            path: provisional.path.clone(),
        });
    }
    unlinkat(
        Some(destination.parent.as_raw_fd()),
        provisional.name.as_c_str(),
        UnlinkatFlags::RemoveDir,
    )?;
    if frozen_named_identity_until(&destination.parent, &provisional.name, &provisional.path, deadline)?.is_some() {
        return Err(Error::FrozenPrivateDirectoryChanged {
            path: provisional.path.clone(),
        });
    }
    sync_frozen_publication_file(
        &destination.parent,
        &destination.parent_path,
        "sync provisional frozen-root cleanup",
        deadline,
    )
}
