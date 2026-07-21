fn discard_frozen_root_destination(destination: &FrozenRootDestination) -> Result<(), Error> {
    let deadline = Instant::now() + FROZEN_MATERIALIZATION_TIMEOUT - FROZEN_NAMESPACE_RECOVERY_TIMEOUT;
    discard_frozen_root_destination_until(destination, deadline)
}

fn discard_frozen_root_destination_until(destination: &FrozenRootDestination, deadline: Instant) -> Result<(), Error> {
    discard_frozen_root_destination_with(
        destination,
        deadline,
        |source_directory, source_name, destination_directory, destination_name| {
            renameat2_noreplace_until(
                source_directory.file(),
                source_name,
                destination_directory.file(),
                destination_name,
                deadline,
            )
        },
    )
}

fn discard_frozen_root_destination_with(
    destination: &FrozenRootDestination,
    deadline: Instant,
    rename: impl FnOnce(&fs::File, &CStr, &fs::File, &CStr) -> io::Result<()>,
) -> Result<(), Error> {
    require_frozen_materialization_deadline(deadline)?;
    let _lock = lock_frozen_destination_until(destination, deadline)?;
    let Some(pinned) =
        open_frozen_named_entry_until(&destination.parent, &destination.name, &destination.root_path, deadline)?
    else {
        return Ok(());
    };
    let expected = frozen_root_identity(&pinned, &destination.root_path)?;
    let metadata = pinned.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    if metadata.mode() & nix::libc::S_IFMT != nix::libc::S_IFDIR
        || metadata.uid() != effective_owner
        || metadata.dev() != destination.parent_identity.device
    {
        return Err(Error::UnsafeFrozenRootDiscard {
            root: destination.root_path.clone(),
            owner: metadata.uid(),
            mode: metadata.mode(),
        });
    }
    let quarantine = create_frozen_private_directory(destination, b".forge-frozen-discard-", deadline)?;
    let detached = quarantine.path.join("root");
    let detached_identity = match detach_frozen_root_with(destination, &quarantine, &pinned, expected, deadline, rename)
    {
        Ok(identity) => identity,
        Err(primary) => {
            let cleanup_deadline = frozen_namespace_recovery_deadline();
            let cleanup = require_frozen_private_directory_entries(&quarantine, &[], cleanup_deadline)
                .and_then(|()| remove_empty_frozen_private_directory(&quarantine, destination, cleanup_deadline));
            return match cleanup {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(Error::CleanupFrozenDiscardQuarantine {
                    quarantine: quarantine.path,
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
    };

    // The root is now durably absent from its public name and exact at the
    // retained private slot. Destructive traversal gets its own finite budget;
    // any failure preserves the non-reusable quarantine instead of exposing a
    // partially deleted public root.
    let cleanup_deadline = Instant::now() + FROZEN_MATERIALIZATION_TIMEOUT;
    let moved = openat2_frozen_until(
        quarantine.file.as_raw_fd(),
        Path::new("root"),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        cleanup_deadline,
    )
    .map_err(|source| Error::OpenFrozenDiscardDirectory { source })?;
    if frozen_root_identity(&moved, &detached)? != detached_identity {
        return Err(Error::FrozenRootChangedDuringDiscard {
            root: destination.root_path.clone(),
            quarantine: detached,
        });
    }
    let mut entries = 1usize;
    discard_frozen_directory(&moved, &detached, 0, &mut entries, cleanup_deadline)?;
    if frozen_publication_name_state(
        &quarantine.file,
        c"root",
        &detached,
        detached_identity,
        cleanup_deadline,
    )? != FrozenPublicationNameState::Expected
    {
        return Err(Error::FrozenRootChangedDuringDiscard {
            root: destination.root_path.clone(),
            quarantine: detached,
        });
    }
    require_frozen_private_directory_entries(&quarantine, &[b"root"], cleanup_deadline)?;
    unlink_frozen_discard_entry_until(
        &quarantine.file,
        c"root",
        &detached,
        detached_identity,
        UnlinkatFlags::RemoveDir,
        cleanup_deadline,
    )?;
    require_frozen_private_directory_entries(&quarantine, &[], cleanup_deadline)?;
    sync_frozen_publication_file(
        &quarantine.file,
        &quarantine.path,
        "sync emptied frozen discard quarantine",
        cleanup_deadline,
    )?;
    remove_empty_frozen_private_directory(&quarantine, destination, cleanup_deadline)
}

fn detach_frozen_root_with(
    destination: &FrozenRootDestination,
    quarantine: &FrozenPrivateDirectory,
    pinned: &fs::File,
    expected: FrozenRootIdentity,
    deadline: Instant,
    rename: impl FnOnce(&fs::File, &CStr, &fs::File, &CStr) -> io::Result<()>,
) -> Result<FrozenRootIdentity, Error> {
    require_frozen_materialization_deadline(deadline)?;
    require_frozen_destination_parent(destination)?;
    require_frozen_private_directory_named(quarantine, destination, deadline)?;
    require_frozen_private_directory_entries(quarantine, &[], deadline)?;
    if frozen_root_identity(pinned, &destination.root_path)? != expected
        || frozen_publication_name_state(
            &destination.parent,
            &destination.name,
            &destination.root_path,
            expected,
            deadline,
        )? != FrozenPublicationNameState::Expected
    {
        return Err(Error::FrozenRootChangedDuringDiscard {
            root: destination.root_path.clone(),
            quarantine: quarantine.path.join("root"),
        });
    }

    // The root may legitimately be mode 000. syncfs on the retained parent
    // flushes the filesystem without requiring a readable root descriptor.
    // The exact mode is widened only immediately before rename below and is
    // restored through the retained descriptor on every failed detach.
    sync_filesystem_until(destination.parent.file(), deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::SyncFrozenPublication {
            path: destination.root_path.clone(),
            operation: "sync frozen-root filesystem before detach",
            source,
        })
    })?;
    sync_frozen_publication_file(
        &destination.parent,
        &destination.parent_path,
        "sync frozen-root parent before detach",
        deadline,
    )?;
    sync_frozen_publication_file(
        &quarantine.file,
        &quarantine.path,
        "sync empty frozen-root quarantine",
        deadline,
    )?;

    require_frozen_destination_parent(destination)?;
    require_frozen_private_directory_named(quarantine, destination, deadline)?;
    require_frozen_private_directory_entries(quarantine, &[], deadline)?;
    if frozen_publication_name_state(
        &destination.parent,
        &destination.name,
        &destination.root_path,
        expected,
        deadline,
    )? != FrozenPublicationNameState::Expected
    {
        return Err(Error::FrozenRootChangedDuringDiscard {
            root: destination.root_path.clone(),
            quarantine: quarantine.path.join("root"),
        });
    }

    // Linux requires write/search permission on a directory whose `..` entry
    // changes during a cross-parent rename. Restore owner access through the
    // retained descriptor immediately before the mutation, then restore the
    // exact original mode on every failed detach. Successful detaches keep the
    // widened mode only inside the private wrapper for bounded deletion.
    let detached_expected = prepare_frozen_discard_root_mode(pinned, destination, expected, deadline)?;

    let detach = (|| {
        require_frozen_materialization_deadline(deadline)?;
        if frozen_publication_name_state(
            &destination.parent,
            &destination.name,
            &destination.root_path,
            detached_expected,
            deadline,
        )? != FrozenPublicationNameState::Expected
        {
            return Err(Error::FrozenRootChangedDuringDiscard {
                root: destination.root_path.clone(),
                quarantine: quarantine.path.join("root"),
            });
        }

        let rename_error = rename(&destination.parent, &destination.name, &quarantine.file, c"root").err();
        let recovery_deadline = frozen_namespace_recovery_deadline();
        let source_state = frozen_publication_name_state(
            &destination.parent,
            &destination.name,
            &destination.root_path,
            detached_expected,
            recovery_deadline,
        )?;
        let quarantine_state = frozen_publication_name_state(
            &quarantine.file,
            c"root",
            &quarantine.path.join("root"),
            detached_expected,
            recovery_deadline,
        )?;
        if source_state == FrozenPublicationNameState::Absent
            && quarantine_state == FrozenPublicationNameState::Expected
        {
            sync_frozen_publication_file(
                &destination.parent,
                &destination.parent_path,
                "sync public parent after frozen-root detach",
                recovery_deadline,
            )?;
            sync_frozen_publication_file(
                &quarantine.file,
                &quarantine.path,
                "sync quarantine after frozen-root detach",
                recovery_deadline,
            )?;
            sync_filesystem_until(quarantine.file.file(), recovery_deadline).map_err(|source| {
                frozen_materialization_io_error(recovery_deadline, source, |source| Error::SyncFrozenPublication {
                    path: quarantine.path.clone(),
                    operation: "sync detached frozen-root namespace",
                    source,
                })
            })?;
            require_frozen_destination_parent(destination)?;
            require_frozen_private_directory_named(quarantine, destination, recovery_deadline)?;
            require_frozen_private_directory_entries(quarantine, &[b"root"], recovery_deadline)?;
            if frozen_publication_name_state(
                &destination.parent,
                &destination.name,
                &destination.root_path,
                detached_expected,
                recovery_deadline,
            )? != FrozenPublicationNameState::Absent
                || frozen_publication_name_state(
                    &quarantine.file,
                    c"root",
                    &quarantine.path.join("root"),
                    detached_expected,
                    recovery_deadline,
                )? != FrozenPublicationNameState::Expected
                || frozen_root_identity(pinned, &quarantine.path.join("root"))? != detached_expected
            {
                return Err(Error::FrozenDiscardNamespaceMismatch {
                    root: destination.root_path.clone(),
                    quarantine: quarantine.path.join("root"),
                });
            }
            return Ok(detached_expected);
        }

        match (source_state, quarantine_state, rename_error) {
            (FrozenPublicationNameState::Expected, FrozenPublicationNameState::Absent, Some(source)) => {
                Err(Error::DetachFrozenRoot {
                    root: destination.root_path.clone(),
                    quarantine: quarantine.path.join("root"),
                    source,
                })
            }
            _ => Err(Error::FrozenDiscardNamespaceMismatch {
                root: destination.root_path.clone(),
                quarantine: quarantine.path.join("root"),
            }),
        }
    })();

    match detach {
        Ok(identity) => Ok(identity),
        Err(primary) => Err(restore_frozen_discard_root_mode(pinned, destination, expected, primary)),
    }
}

fn prepare_frozen_discard_root_mode(
    pinned: &fs::File,
    destination: &FrozenRootDestination,
    expected: FrozenRootIdentity,
    deadline: Instant,
) -> Result<FrozenRootIdentity, Error> {
    prepare_frozen_discard_root_mode_with(pinned, destination, expected, deadline, frozen_root_identity)
}

fn prepare_frozen_discard_root_mode_with(
    pinned: &fs::File,
    destination: &FrozenRootDestination,
    expected: FrozenRootIdentity,
    deadline: Instant,
    inspect: impl FnOnce(&fs::File, &Path) -> Result<FrozenRootIdentity, Error>,
) -> Result<FrozenRootIdentity, Error> {
    let discard_permissions = expected.mode & 0o7777 | 0o700;
    let mut detached_expected = expected;
    detached_expected.mode = expected.mode & !0o7777 | discard_permissions;
    let normalize = chmod_path_descriptor_until(pinned.file(), discard_permissions, deadline);
    let normalized = match inspect(pinned, &destination.root_path) {
        Ok(normalized) => normalized,
        Err(primary) => {
            return Err(restore_frozen_discard_root_mode(pinned, destination, expected, primary));
        }
    };
    if normalized == detached_expected {
        return Ok(detached_expected);
    }
    let primary = match normalize {
        Err(source) => {
            frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenPrivateDirectory {
                path: destination.root_path.clone(),
                source,
            })
        }
        Ok(()) => Error::FrozenRootChangedDuringDiscard {
            root: destination.root_path.clone(),
            quarantine: destination.parent_path.clone(),
        },
    };
    Err(restore_frozen_discard_root_mode(pinned, destination, expected, primary))
}

fn restore_frozen_discard_root_mode(
    pinned: &fs::File,
    destination: &FrozenRootDestination,
    expected: FrozenRootIdentity,
    primary: Error,
) -> Error {
    if frozen_root_identity(pinned, &destination.root_path).ok() == Some(expected) {
        return primary;
    }
    let recovery_deadline = frozen_namespace_recovery_deadline();
    let restore = chmod_path_descriptor_until(pinned.file(), expected.mode & 0o7777, recovery_deadline)
        .map_err(|source| Error::NormalizeFrozenPrivateDirectory {
            path: destination.root_path.clone(),
            source,
        })
        .and_then(|()| {
            if frozen_root_identity(pinned, &destination.root_path)? == expected {
                Ok(())
            } else {
                Err(Error::FrozenRootChangedDuringDiscard {
                    root: destination.root_path.clone(),
                    quarantine: destination.parent_path.clone(),
                })
            }
        })
        .and_then(|()| {
            sync_filesystem_until(destination.parent.file(), recovery_deadline).map_err(|source| {
                frozen_materialization_io_error(recovery_deadline, source, |source| Error::SyncFrozenPublication {
                    path: destination.root_path.clone(),
                    operation: "sync restored frozen-root discard mode",
                    source,
                })
            })
        })
        .and_then(|()| {
            if frozen_root_identity(pinned, &destination.root_path)? == expected {
                Ok(())
            } else {
                Err(Error::FrozenRootChangedDuringDiscard {
                    root: destination.root_path.clone(),
                    quarantine: destination.parent_path.clone(),
                })
            }
        });
    match restore {
        Ok(()) => primary,
        Err(restore) => Error::RestoreFrozenDiscardRootMode {
            root: destination.root_path.clone(),
            primary: Box::new(primary),
            restore: Box::new(restore),
        },
    }
}

fn unlink_frozen_discard_entry_until(
    directory: &fs::File,
    name: &CStr,
    path: &Path,
    expected: FrozenRootIdentity,
    flags: UnlinkatFlags,
    deadline: Instant,
) -> Result<(), Error> {
    unlink_frozen_discard_entry_with(directory, name, path, expected, deadline, |directory, name| {
        unlinkat(Some(directory.as_raw_fd()), name, flags)
    })
}

fn unlink_frozen_discard_entry_with(
    directory: &fs::File,
    name: &CStr,
    path: &Path,
    expected: FrozenRootIdentity,
    deadline: Instant,
    mut remove: impl FnMut(&fs::File, &CStr) -> Result<(), Errno>,
) -> Result<(), Error> {
    if frozen_publication_name_state(directory, name, path, expected, deadline)? != FrozenPublicationNameState::Expected
    {
        return Err(Error::FrozenDiscardEntryChanged);
    }

    let mut interruptions = 0usize;
    loop {
        require_frozen_materialization_deadline(deadline)?;
        let result = remove(directory, name);

        // unlinkat can be interrupted after its observable namespace effect.
        // Always classify the retained parent/name before deciding whether to
        // retry. The still-open anchor held by the caller prevents the exact
        // inode number from being recycled during this reconciliation.
        let recovery_deadline = frozen_namespace_recovery_deadline();
        match frozen_publication_name_state(directory, name, path, expected, recovery_deadline)? {
            FrozenPublicationNameState::Absent => return Ok(()),
            FrozenPublicationNameState::Foreign => return Err(Error::FrozenDiscardEntryChanged),
            FrozenPublicationNameState::Expected => match result {
                Err(Errno::EINTR) if interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS => {
                    interruptions += 1;
                }
                Err(source) => {
                    return Err(frozen_materialization_io_error(
                        deadline,
                        io::Error::from_raw_os_error(source as i32),
                        |source| Error::RemoveFrozenDiscardEntry {
                            path: path.to_owned(),
                            source,
                        },
                    ));
                }
                Ok(()) => return Err(Error::FrozenDiscardEntryChanged),
            },
        }
    }
}
