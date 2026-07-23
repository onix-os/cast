fn publish_frozen_root(
    stage: &FrozenPrivateDirectory,
    destination: &FrozenRootDestination,
    staged_root: &fs::File,
    staged_root_anchor: fs::File,
    deadline: Instant,
) -> Result<MaterializedFrozenRoot, Error> {
    publish_frozen_root_with(
        stage,
        destination,
        staged_root,
        staged_root_anchor,
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

fn publish_frozen_root_with(
    stage: &FrozenPrivateDirectory,
    destination: &FrozenRootDestination,
    staged_root: &fs::File,
    staged_root_anchor: fs::File,
    deadline: Instant,
    rename: impl FnOnce(&fs::File, &CStr, &fs::File, &CStr) -> io::Result<()>,
) -> Result<MaterializedFrozenRoot, Error> {
    require_frozen_materialization_deadline(deadline)?;
    require_frozen_destination_parent(destination)?;
    require_frozen_private_directory_named(stage, destination, deadline)?;
    require_frozen_private_directory_entries(stage, &[b"root"], deadline)?;

    let staged_identity = frozen_root_identity(staged_root, &stage.path.join("root"))?;
    let anchor_identity = frozen_root_identity(&staged_root_anchor, &stage.path.join("root"))?;
    // Activation must retain the capability opened from the private staging
    // namespace, never reopen the replaceable public destination after the
    // rename. Keep that invariant explicit here because Container's anchored
    // API deliberately rejects ordinary readable directory descriptors.
    // SAFETY: the retained file owns a live descriptor for the fcntl call.
    let anchor_flags = unsafe { nix::libc::fcntl(staged_root_anchor.as_raw_fd(), nix::libc::F_GETFL) };
    if anchor_flags == -1 {
        return Err(Error::OpenFrozenExecutableRoot {
            path: stage.path.join("root"),
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: the retained file owns a live descriptor for the fcntl call.
    let anchor_descriptor_flags = unsafe { nix::libc::fcntl(staged_root_anchor.as_raw_fd(), nix::libc::F_GETFD) };
    if anchor_descriptor_flags == -1 {
        return Err(Error::OpenFrozenExecutableRoot {
            path: stage.path.join("root"),
            source: io::Error::last_os_error(),
        });
    }
    if anchor_identity != staged_identity
        || anchor_flags & (nix::libc::O_PATH | nix::libc::O_DIRECTORY) != nix::libc::O_PATH | nix::libc::O_DIRECTORY
        || anchor_descriptor_flags & nix::libc::FD_CLOEXEC == 0
    {
        return Err(Error::FrozenPublicationNamespaceMismatch {
            stage: stage.path.clone(),
            destination: destination.root_path.clone(),
            reason: "the retained activation anchor is not the exact close-on-exec staged O_PATH directory",
        });
    }
    if staged_identity.device != destination.parent_identity.device || staged_identity.device != stage.identity.device {
        return Err(Error::FrozenPublicationNamespaceMismatch {
            stage: stage.path.clone(),
            destination: destination.root_path.clone(),
            reason: "the retained root and publication parents are not on one filesystem",
        });
    }
    match frozen_publication_name_state(
        &stage.file,
        c"root",
        &stage.path.join("root"),
        staged_identity,
        deadline,
    )? {
        FrozenPublicationNameState::Expected => {}
        FrozenPublicationNameState::Absent | FrozenPublicationNameState::Foreign => {
            return Err(Error::FrozenPublicationNamespaceMismatch {
                stage: stage.path.clone(),
                destination: destination.root_path.clone(),
                reason: "the private stage name does not identify the retained root",
            });
        }
    }
    if !matches!(
        frozen_publication_name_state(
            &destination.parent,
            &destination.name,
            &destination.root_path,
            staged_identity,
            deadline,
        )?,
        FrozenPublicationNameState::Absent
    ) {
        return Err(Error::FrozenRootDestinationExists(destination.root_path.clone()));
    }

    sync_frozen_publication_file(staged_root, &stage.path.join("root"), "sync staged root", deadline)?;
    sync_filesystem_until(staged_root.file(), deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::SyncFrozenPublication {
            path: stage.path.join("root"),
            operation: "sync staged filesystem before publication",
            source,
        })
    })?;
    sync_frozen_publication_file(&stage.file, &stage.path, "sync stage wrapper", deadline)?;
    sync_frozen_publication_file(
        &destination.parent,
        &destination.parent_path,
        "sync destination parent before publication",
        deadline,
    )?;

    // Repeat the complete namespace proof at the last userspace boundary.
    // The advisory parent lock serializes cooperating Forge writers; Linux has
    // no rename primitive which can additionally compare the source inode.
    require_frozen_destination_parent(destination)?;
    require_frozen_private_directory_named(stage, destination, deadline)?;
    require_frozen_private_directory_entries(stage, &[b"root"], deadline)?;
    if !matches!(
        frozen_publication_name_state(
            &stage.file,
            c"root",
            &stage.path.join("root"),
            staged_identity,
            deadline,
        )?,
        FrozenPublicationNameState::Expected
    ) || !matches!(
        frozen_publication_name_state(
            &destination.parent,
            &destination.name,
            &destination.root_path,
            staged_identity,
            deadline,
        )?,
        FrozenPublicationNameState::Absent
    ) {
        return Err(Error::FrozenPublicationNamespaceMismatch {
            stage: stage.path.clone(),
            destination: destination.root_path.clone(),
            reason: "the source or destination name changed before the publication syscall",
        });
    }

    let rename_error = rename(&stage.file, c"root", &destination.parent, &destination.name).err();
    // Namespace reconciliation is mandatory after every attempted rename,
    // even when the ordinary work budget expired during the syscall. The
    // caller reserves a separate tail of the same total materialization
    // budget exclusively for reconciliation and bounded cleanup.
    let recovery_deadline = frozen_namespace_recovery_deadline();
    let source_state = frozen_publication_name_state(
        &stage.file,
        c"root",
        &stage.path.join("root"),
        staged_identity,
        recovery_deadline,
    )?;
    let destination_state = frozen_publication_name_state(
        &destination.parent,
        &destination.name,
        &destination.root_path,
        staged_identity,
        recovery_deadline,
    )?;

    if source_state == FrozenPublicationNameState::Absent && destination_state == FrozenPublicationNameState::Expected {
        // Some filesystems may report an error after the namespace operation
        // has already taken effect. Exact two-name reconciliation is stronger
        // evidence than the syscall return value, so adopt the applied move.
        sync_frozen_publication_file(
            staged_root,
            &destination.root_path,
            "sync published root",
            recovery_deadline,
        )?;
        sync_frozen_publication_file(
            &stage.file,
            &stage.path,
            "sync emptied stage wrapper",
            recovery_deadline,
        )?;
        sync_frozen_publication_file(
            &destination.parent,
            &destination.parent_path,
            "sync destination parent after publication",
            recovery_deadline,
        )?;
        sync_filesystem_until(destination.parent.file(), recovery_deadline).map_err(|source| {
            frozen_materialization_io_error(recovery_deadline, source, |source| Error::SyncFrozenPublication {
                path: destination.parent_path.clone(),
                operation: "sync published frozen-root namespace",
                source,
            })
        })?;
        require_frozen_destination_parent(destination)?;
        require_frozen_private_directory_named(stage, destination, recovery_deadline)?;
        require_frozen_private_directory_entries(stage, &[], recovery_deadline)?;
        if frozen_publication_name_state(
            &stage.file,
            c"root",
            &stage.path.join("root"),
            staged_identity,
            recovery_deadline,
        )? != FrozenPublicationNameState::Absent
            || frozen_publication_name_state(
                &destination.parent,
                &destination.name,
                &destination.root_path,
                staged_identity,
                recovery_deadline,
            )? != FrozenPublicationNameState::Expected
            || frozen_root_identity(staged_root, &destination.root_path)? != staged_identity
            || frozen_root_identity(&staged_root_anchor, &destination.root_path)? != staged_identity
        {
            return Err(Error::FrozenPublicationNamespaceMismatch {
                stage: stage.path.clone(),
                destination: destination.root_path.clone(),
                reason: "the published root changed during the durability barrier",
            });
        }
        let materialized = MaterializedFrozenRoot {
            root_path: destination.root_path.clone(),
            root: staged_root_anchor,
            identity: staged_identity,
        };
        materialized.revalidate()?;
        return Ok(materialized);
    }

    match (source_state, destination_state, rename_error) {
        (FrozenPublicationNameState::Expected, FrozenPublicationNameState::Absent, Some(source)) => {
            Err(Error::PublishFrozenRoot {
                stage: stage.path.join("root"),
                destination: destination.root_path.clone(),
                source,
            })
        }
        (FrozenPublicationNameState::Expected, FrozenPublicationNameState::Foreign, Some(source))
            if source.kind() == io::ErrorKind::AlreadyExists =>
        {
            Err(Error::FrozenRootDestinationExists(destination.root_path.clone()))
        }
        _ => Err(Error::FrozenPublicationNamespaceMismatch {
            stage: stage.path.clone(),
            destination: destination.root_path.clone(),
            reason: "publication did not leave the retained root at exactly one authoritative name",
        }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrozenPublicationNameState {
    Absent,
    Expected,
    Foreign,
}

fn frozen_publication_name_state(
    parent: &fs::File,
    name: &CStr,
    path: &Path,
    expected: FrozenRootIdentity,
    deadline: Instant,
) -> Result<FrozenPublicationNameState, Error> {
    Ok(match frozen_named_identity_until(parent, name, path, deadline)? {
        None => FrozenPublicationNameState::Absent,
        Some(actual) if actual == expected => FrozenPublicationNameState::Expected,
        Some(_) => FrozenPublicationNameState::Foreign,
    })
}

fn require_frozen_private_directory_named(
    directory: &FrozenPrivateDirectory,
    destination: &FrozenRootDestination,
    deadline: Instant,
) -> Result<(), Error> {
    if frozen_root_identity(&directory.file, &directory.path)? != directory.identity
        || frozen_named_identity_until(&destination.parent, &directory.name, &directory.path, deadline)?
            != Some(directory.identity)
    {
        return Err(Error::FrozenPrivateDirectoryChanged {
            path: directory.path.clone(),
        });
    }
    Ok(())
}

fn require_frozen_private_directory_entries(
    directory: &FrozenPrivateDirectory,
    expected: &[&[u8]],
    deadline: Instant,
) -> Result<(), Error> {
    let mut entries = 0usize;
    let mut actual = frozen_discard_entry_names(directory.file.as_raw_fd(), &mut entries, deadline)?
        .into_iter()
        .map(|name| name.into_bytes())
        .collect::<Vec<_>>();
    let mut expected = expected.iter().map(|name| name.to_vec()).collect::<Vec<_>>();
    actual.sort();
    expected.sort();
    if actual == expected {
        Ok(())
    } else {
        Err(Error::FrozenPrivateDirectoryChanged {
            path: directory.path.clone(),
        })
    }
}

fn sync_frozen_publication_file(
    file: &fs::File,
    path: &Path,
    operation: &'static str,
    deadline: Instant,
) -> Result<(), Error> {
    let mut interruptions = 0usize;
    loop {
        require_frozen_materialization_deadline(deadline)?;
        match file.sync_all() {
            Ok(()) => break,
            Err(source)
                if source.kind() == io::ErrorKind::Interrupted
                    && interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS =>
            {
                interruptions += 1;
            }
            Err(source) => {
                return Err(frozen_materialization_io_error(deadline, source, |source| {
                    Error::SyncFrozenPublication {
                        path: path.to_owned(),
                        operation,
                        source,
                    }
                }));
            }
        }
    }
    require_frozen_materialization_deadline(deadline)
}

fn discard_retained_frozen_stage(
    stage: &FrozenPrivateDirectory,
    destination: &FrozenRootDestination,
    staged_root: &fs::File,
    deadline: Instant,
) -> Result<(), Error> {
    require_frozen_destination_parent(destination)?;
    require_frozen_private_directory_named(stage, destination, deadline)?;
    require_frozen_private_directory_entries(stage, &[b"root"], deadline)?;
    let expected = frozen_root_identity(staged_root, &stage.path.join("root"))?;
    if frozen_publication_name_state(&stage.file, c"root", &stage.path.join("root"), expected, deadline)?
        != FrozenPublicationNameState::Expected
    {
        return Err(Error::FrozenRetainedStageChanged {
            stage: stage.path.clone(),
        });
    }

    let mode = staged_root.metadata()?.mode() & 0o7777;
    chmod_path_descriptor_until(staged_root.file(), mode | 0o700, deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenPrivateDirectory {
            path: stage.path.join("root"),
            source,
        })
    })?;
    let readable = openat2_frozen_until(
        stage.file.as_raw_fd(),
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
        deadline,
    )
    .map_err(|source| Error::OpenFrozenPrivateDirectory {
        path: stage.path.join("root"),
        source,
    })?;
    let expected_after_chmod = frozen_root_identity(staged_root, &stage.path.join("root"))?;
    if frozen_root_identity(&readable, &stage.path.join("root"))? != expected_after_chmod {
        return Err(Error::FrozenRetainedStageChanged {
            stage: stage.path.clone(),
        });
    }
    let mut entries = 1usize;
    discard_frozen_directory(&readable, &stage.path.join("root"), 0, &mut entries, deadline)?;
    drop(readable);
    if frozen_publication_name_state(
        &stage.file,
        c"root",
        &stage.path.join("root"),
        expected_after_chmod,
        deadline,
    )? != FrozenPublicationNameState::Expected
    {
        return Err(Error::FrozenRetainedStageChanged {
            stage: stage.path.clone(),
        });
    }
    unlink_frozen_discard_entry_until(
        &stage.file,
        c"root",
        &stage.path.join("root"),
        expected_after_chmod,
        UnlinkatFlags::RemoveDir,
        deadline,
    )?;
    require_frozen_private_directory_entries(stage, &[], deadline)?;
    sync_frozen_publication_file(&stage.file, &stage.path, "sync discarded private stage", deadline)
}

fn remove_empty_frozen_private_directory(
    directory: &FrozenPrivateDirectory,
    destination: &FrozenRootDestination,
    deadline: Instant,
) -> Result<(), Error> {
    require_frozen_destination_parent(destination)?;
    require_frozen_private_directory_named(directory, destination, deadline)?;
    require_frozen_private_directory_entries(directory, &[], deadline)?;
    let mut interruptions = 0usize;
    loop {
        require_frozen_materialization_deadline(deadline)?;
        let result = unlinkat(
            Some(destination.parent.as_raw_fd()),
            directory.name.as_c_str(),
            UnlinkatFlags::RemoveDir,
        );
        let recovery_deadline = frozen_namespace_recovery_deadline();
        let named =
            frozen_named_identity_until(&destination.parent, &directory.name, &directory.path, recovery_deadline)?;
        match (result, named) {
            (_, None) => {
                sync_frozen_publication_file(
                    &destination.parent,
                    &destination.parent_path,
                    "sync removed private frozen-root directory",
                    recovery_deadline,
                )?;
                return Ok(());
            }
            (Err(Errno::EINTR), Some(identity))
                if identity == directory.identity && interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS =>
            {
                interruptions += 1;
            }
            (Err(source), Some(identity)) if identity == directory.identity => {
                return Err(frozen_materialization_io_error(
                    deadline,
                    io::Error::from_raw_os_error(source as i32),
                    Error::Io,
                ));
            }
            _ => {
                return Err(Error::FrozenPrivateDirectoryChanged {
                    path: directory.path.clone(),
                });
            }
        }
    }
}

struct FrozenDiscardDirectoryStream(NonNull<nix::libc::DIR>);

impl Drop for FrozenDiscardDirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the stream returned by fdopendir.
        unsafe {
            nix::libc::closedir(self.0.as_ptr());
        }
    }
}
