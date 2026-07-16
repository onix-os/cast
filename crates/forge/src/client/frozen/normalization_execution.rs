/// Prove and normalize the complete declarative frozen tree through retained
/// descriptors. A preparation walk makes mode-zero directories and regular
/// files temporarily owner-accessible; a second full walk authenticates
/// content and seals leaves and directories bottom-up. The filesystem must
/// match `expected` exactly: no missing, extra, type-changed, mode-changed,
/// hardlinked, POSIX access/default-ACL-bearing, or cross-mount entry is
/// eligible for publication.
///
/// This is a proof over Forge's private, quiescent staging tree, not a kernel
/// filesystem freeze. An uncooperative process with the same effective UID can
/// still mutate an ordinary inode after its last check; publication therefore
/// must not claim adversarial same-UID snapshot atomicity.
fn chmod_frozen_normalization_entry(
    file: &std::fs::File,
    path: &Path,
    mode: u32,
    deadline: Instant,
) -> Result<(), Error> {
    chmod_path_descriptor_until(file, mode, deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenEntryMode {
            path: path.to_owned(),
            source,
        })
    })
}

fn open_frozen_normalization_readonly(
    file: &std::fs::File,
    path: &Path,
    deadline: Instant,
) -> Result<std::fs::File, Error> {
    open_path_descriptor_readonly_until(file, deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::OpenFrozenNormalizationEntry {
            path: path.to_owned(),
            source,
        })
    })
}

fn require_frozen_normalization_access_acl(file: &std::fs::File, path: &Path, deadline: Instant) -> Result<(), Error> {
    require_no_access_acl_until(file, path, deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::FrozenNormalizationAcl {
            path: path.to_owned(),
            source,
        })
    })
}

fn require_frozen_normalization_default_acl(file: &std::fs::File, path: &Path, deadline: Instant) -> Result<(), Error> {
    require_no_default_acl_until(file, path, deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::FrozenNormalizationAcl {
            path: path.to_owned(),
            source,
        })
    })
}

fn normalize_frozen_tree(
    root: &fs::File,
    display_path: &Path,
    expected: &FrozenExpectedTree,
    timestamp: FileTime,
    deadline: Instant,
) -> Result<(), Error> {
    normalize_frozen_tree_with(
        root,
        display_path,
        expected,
        timestamp,
        deadline,
        FrozenNormalizationLimits::PRODUCTION,
        |_, _| {},
    )
}

fn normalize_frozen_tree_with<F>(
    root: &fs::File,
    display_path: &Path,
    expected: &FrozenExpectedTree,
    timestamp: FileTime,
    deadline: Instant,
    limits: FrozenNormalizationLimits,
    mut checkpoint: F,
) -> Result<(), Error>
where
    F: FnMut(FrozenNormalizationCheckpoint, &Path),
{
    require_frozen_materialization_deadline(deadline)?;
    if limits.inodes == 0 {
        return Err(Error::FrozenNormalizationInodeLimit { limit: 0, actual: 1 });
    }
    let expected_path = Path::new("/");
    let declaration = expected.entry(expected_path)?;
    let witness = frozen_normalization_witness(root, expected_path)?;
    let root_device = witness.device;
    require_frozen_normalization_declaration(expected_path, witness, declaration, root_device)?;
    require_named_frozen_normalization_root(display_path, root, witness, deadline)?;

    let mut inodes = 1usize;
    normalize_frozen_directory(
        root,
        root,
        expected_path,
        expected,
        declaration,
        witness,
        deadline,
        limits,
        root_device,
        &mut inodes,
        &mut checkpoint,
    )?;
    if inodes != expected.entries.len() {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: expected_path.to_owned(),
            reason: "the runtime walk did not account for the complete declarative tree",
        });
    }
    checkpoint(
        FrozenNormalizationCheckpoint::BeforeFinalTreeConfirmation,
        expected_path,
    );
    let final_witness = seal_frozen_directory(
        root,
        root,
        expected_path,
        expected,
        declaration,
        timestamp,
        deadline,
        root_device,
        &mut checkpoint,
    )?;
    require_named_frozen_normalization_root_final(display_path, root, final_witness, deadline)?;
    require_frozen_normalization_access_acl(root.file(), expected_path, deadline)?;
    require_frozen_normalization_default_acl(root.file(), expected_path, deadline)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn normalize_frozen_directory<F>(
    anchor: &fs::File,
    directory: &fs::File,
    expected_path: &Path,
    expected: &FrozenExpectedTree,
    declaration: &FrozenExpectedEntry,
    original: FrozenNormalizationWitness,
    deadline: Instant,
    limits: FrozenNormalizationLimits,
    root_device: u64,
    inodes: &mut usize,
    checkpoint: &mut F,
) -> Result<(), Error>
where
    F: FnMut(FrozenNormalizationCheckpoint, &Path),
{
    require_frozen_materialization_deadline(deadline)?;
    let FrozenExpectedKind::Directory = declaration.kind else {
        return Err(Error::InvalidFrozenNormalizationDeclaration {
            path: expected_path.to_owned(),
            reason: "a non-directory declaration reached directory traversal",
        });
    };
    let traversal_mode = declaration.mode | 0o700;
    if traversal_mode != declaration.mode {
        chmod_frozen_normalization_entry(anchor.file(), expected_path, traversal_mode, deadline)?;
    }
    checkpoint(
        FrozenNormalizationCheckpoint::DirectoryTraversalModeApplied,
        expected_path,
    );
    require_frozen_normalization_witness(expected_path, directory, original.with_permissions(traversal_mode))?;
    require_frozen_normalization_access_acl(directory.file(), expected_path, deadline)?;
    require_frozen_normalization_default_acl(directory.file(), expected_path, deadline)?;

    let declared_children = frozen_normalization_declared_children(expected, expected_path)?;
    let inventory = frozen_normalization_inventory(
        directory,
        expected_path,
        declared_children.len(),
        Some((inodes, limits.inodes)),
        deadline,
    )?;
    require_frozen_normalization_inventory(expected_path, &inventory, &declared_children, expected, root_device)?;
    checkpoint(FrozenNormalizationCheckpoint::DirectoryEnumerated, expected_path);

    for (entry, (_, child_path)) in inventory.iter().zip(declared_children.iter()) {
        normalize_frozen_entry(
            directory,
            entry,
            child_path,
            expected,
            deadline,
            limits,
            root_device,
            inodes,
            checkpoint,
        )?;
    }

    require_frozen_materialization_deadline(deadline)?;
    let confirmed = frozen_normalization_inventory(directory, expected_path, inventory.len(), None, deadline)?;
    require_frozen_normalization_active_inventory(
        expected_path,
        &inventory,
        &confirmed,
        &declared_children,
        expected,
        root_device,
    )?;
    require_frozen_normalization_witness(expected_path, anchor, original.with_permissions(traversal_mode))?;
    require_frozen_normalization_access_acl(directory.file(), expected_path, deadline)?;
    require_frozen_normalization_default_acl(directory.file(), expected_path, deadline)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn normalize_frozen_entry<F>(
    parent: &fs::File,
    inventory: &FrozenNormalizationInventoryEntry,
    expected_path: &Path,
    expected: &FrozenExpectedTree,
    deadline: Instant,
    limits: FrozenNormalizationLimits,
    root_device: u64,
    inodes: &mut usize,
    checkpoint: &mut F,
) -> Result<(), Error>
where
    F: FnMut(FrozenNormalizationCheckpoint, &Path),
{
    require_frozen_materialization_deadline(deadline)?;
    let depth =
        frozen_normalization_path_depth(expected_path).ok_or_else(|| Error::InvalidFrozenNormalizationDeclaration {
            path: expected_path.to_owned(),
            reason: "the declarative path is not normalized and absolute",
        })?;
    if depth > limits.depth {
        return Err(Error::FrozenNormalizationDepthLimit {
            limit: limits.depth,
            actual: depth,
        });
    }
    let declaration = expected.entry(expected_path)?;
    let pinned = open_frozen_normalization_entry(
        parent,
        &inventory.name,
        expected_path,
        FrozenNormalizationOpen::Anchor,
        deadline,
    )?;
    require_frozen_normalization_witness(expected_path, &pinned, inventory.witness)?;
    require_frozen_normalization_declaration(expected_path, inventory.witness, declaration, root_device)?;
    checkpoint(FrozenNormalizationCheckpoint::EntryPinned, expected_path);
    require_named_frozen_normalization_entry(parent, &inventory.name, expected_path, inventory.witness, deadline)?;

    let active_witness = match &declaration.kind {
        FrozenExpectedKind::Directory => {
            let traversal_mode = declaration.mode | 0o700;
            if traversal_mode != declaration.mode {
                chmod_frozen_normalization_entry(pinned.file(), expected_path, traversal_mode, deadline)?;
            }
            checkpoint(
                FrozenNormalizationCheckpoint::DirectoryTraversalModeApplied,
                expected_path,
            );
            let traversal_witness = inventory.witness.with_permissions(traversal_mode);
            require_named_frozen_normalization_entry(
                parent,
                &inventory.name,
                expected_path,
                traversal_witness,
                deadline,
            )?;
            let directory = open_frozen_normalization_entry(
                parent,
                &inventory.name,
                expected_path,
                FrozenNormalizationOpen::Directory,
                deadline,
            )?;
            require_frozen_normalization_witness(expected_path, &directory, traversal_witness)?;
            normalize_frozen_directory(
                &pinned,
                &directory,
                expected_path,
                expected,
                declaration,
                inventory.witness,
                deadline,
                limits,
                root_device,
                inodes,
                checkpoint,
            )?;
            traversal_witness
        }
        FrozenExpectedKind::Regular { .. } => {
            let readable_mode = declaration.mode | 0o400;
            if readable_mode != declaration.mode {
                chmod_frozen_normalization_entry(pinned.file(), expected_path, readable_mode, deadline)?;
            }
            let readable_witness = inventory.witness.with_permissions(readable_mode);
            require_named_frozen_normalization_entry(
                parent,
                &inventory.name,
                expected_path,
                readable_witness,
                deadline,
            )?;
            let readable = match open_frozen_normalization_readonly(pinned.file(), expected_path, deadline) {
                Ok(readable) => fs::File::from_parts(readable, expected_path.to_owned()),
                Err(primary) => {
                    if readable_mode != declaration.mode {
                        chmod_frozen_normalization_entry(pinned.file(), expected_path, declaration.mode, deadline)?;
                    }
                    return Err(primary);
                }
            };
            require_frozen_normalization_witness(expected_path, &readable, readable_witness)?;
            if let Err(primary) = require_frozen_normalization_access_acl(readable.file(), expected_path, deadline) {
                if readable_mode != declaration.mode {
                    chmod_frozen_normalization_entry(pinned.file(), expected_path, declaration.mode, deadline)?;
                }
                return Err(primary);
            }
            readable_witness
        }
        FrozenExpectedKind::Symlink { target } => {
            let actual = read_frozen_normalization_symlink(&pinned, expected_path, deadline)?;
            if &actual != target {
                return Err(Error::FrozenNormalizationSymlinkTargetMismatch {
                    path: expected_path.to_owned(),
                    expected: OsString::from_vec(target.clone()),
                    actual: OsString::from_vec(actual),
                });
            }
            inventory.witness
        }
    };

    checkpoint(FrozenNormalizationCheckpoint::BeforeEntryRevalidation, expected_path);
    require_named_frozen_normalization_entry(parent, &inventory.name, expected_path, active_witness, deadline)
}

#[allow(clippy::too_many_arguments)]
fn seal_frozen_directory<F>(
    anchor: &fs::File,
    directory: &fs::File,
    expected_path: &Path,
    expected: &FrozenExpectedTree,
    declaration: &FrozenExpectedEntry,
    timestamp: FileTime,
    deadline: Instant,
    root_device: u64,
    checkpoint: &mut F,
) -> Result<FrozenNormalizationFinalWitness, Error>
where
    F: FnMut(FrozenNormalizationCheckpoint, &Path),
{
    require_frozen_materialization_deadline(deadline)?;
    let FrozenExpectedKind::Directory = declaration.kind else {
        return Err(Error::InvalidFrozenNormalizationDeclaration {
            path: expected_path.to_owned(),
            reason: "a non-directory declaration reached final directory sealing",
        });
    };
    let active = frozen_normalization_witness(anchor, expected_path)?;
    require_frozen_normalization_active_declaration(expected_path, active, declaration, root_device)?;
    require_frozen_normalization_witness(expected_path, directory, active)?;
    require_frozen_normalization_access_acl(directory.file(), expected_path, deadline)?;
    require_frozen_normalization_default_acl(directory.file(), expected_path, deadline)?;

    let declared_children = frozen_normalization_declared_children(expected, expected_path)?;
    let inventory = frozen_normalization_inventory(directory, expected_path, declared_children.len(), None, deadline)?;
    require_frozen_normalization_active_declarations(
        expected_path,
        &inventory,
        &declared_children,
        expected,
        root_device,
    )?;

    let mut sealed = Vec::new();
    sealed
        .try_reserve_exact(inventory.len())
        .map_err(|source| Error::ReserveFrozenNormalizationInventory {
            path: expected_path.to_owned(),
            source,
        })?;
    for (entry, (_, child_path)) in inventory.iter().zip(declared_children.iter()) {
        let witness = seal_frozen_entry(
            directory,
            entry,
            child_path,
            expected,
            timestamp,
            deadline,
            root_device,
            checkpoint,
        )?;
        sealed.push((entry.name.clone(), (*child_path).clone(), witness));
    }

    // Normalize the directory itself before its last child inventory. The
    // O_NOATIME inventory below must leave this full witness untouched, so a
    // concurrent add/remove cannot be hidden by a later Forge utimens call.
    set_frozen_normalization_times(anchor, expected_path, timestamp, deadline)?;
    require_frozen_normalization_times(expected_path, anchor, timestamp)?;
    let active_final_witness = frozen_normalization_final_witness(anchor, expected_path)?;
    checkpoint(
        FrozenNormalizationCheckpoint::BeforeDirectoryFinalInventory,
        expected_path,
    );
    if expected_path == Path::new("/") {
        checkpoint(FrozenNormalizationCheckpoint::BeforeRootRevalidation, expected_path);
    }
    let confirmed = frozen_normalization_final_inventory(directory, expected_path, sealed.len(), deadline)?;
    require_frozen_normalization_final_inventory(expected_path, &confirmed, &sealed)?;
    checkpoint(
        FrozenNormalizationCheckpoint::AfterDirectoryFinalInventory,
        expected_path,
    );
    if frozen_normalization_final_witness(anchor, expected_path)? != active_final_witness {
        return Err(Error::FrozenNormalizationEntryChanged(expected_path.to_owned()));
    }

    let active_mode = declaration.mode | 0o700;
    if active_mode != declaration.mode {
        chmod_frozen_normalization_entry(anchor.file(), expected_path, declaration.mode, deadline)?;
    }
    let final_stable = frozen_normalization_witness(anchor, expected_path)?;
    require_frozen_normalization_declaration(expected_path, final_stable, declaration, root_device)?;
    require_frozen_normalization_times(expected_path, anchor, timestamp)?;
    require_frozen_normalization_access_acl(directory.file(), expected_path, deadline)?;
    require_frozen_normalization_default_acl(directory.file(), expected_path, deadline)?;
    frozen_normalization_final_witness(anchor, expected_path)
}

#[allow(clippy::too_many_arguments)]
fn seal_frozen_entry<F>(
    parent: &fs::File,
    inventory: &FrozenNormalizationInventoryEntry,
    expected_path: &Path,
    expected: &FrozenExpectedTree,
    timestamp: FileTime,
    deadline: Instant,
    root_device: u64,
    checkpoint: &mut F,
) -> Result<FrozenNormalizationFinalWitness, Error>
where
    F: FnMut(FrozenNormalizationCheckpoint, &Path),
{
    require_frozen_materialization_deadline(deadline)?;
    let declaration = expected.entry(expected_path)?;
    require_frozen_normalization_active_declaration(expected_path, inventory.witness, declaration, root_device)?;
    let pinned = open_frozen_normalization_entry(
        parent,
        &inventory.name,
        expected_path,
        FrozenNormalizationOpen::Anchor,
        deadline,
    )?;
    require_frozen_normalization_witness(expected_path, &pinned, inventory.witness)?;
    require_named_frozen_normalization_entry(parent, &inventory.name, expected_path, inventory.witness, deadline)?;

    let mut final_acl_check: Option<fs::File> = None;
    let final_witness = match &declaration.kind {
        FrozenExpectedKind::Directory => {
            let directory = open_frozen_normalization_entry(
                parent,
                &inventory.name,
                expected_path,
                FrozenNormalizationOpen::Directory,
                deadline,
            )?;
            require_frozen_normalization_witness(expected_path, &directory, inventory.witness)?;
            seal_frozen_directory(
                &pinned,
                &directory,
                expected_path,
                expected,
                declaration,
                timestamp,
                deadline,
                root_device,
                checkpoint,
            )?
        }
        FrozenExpectedKind::Regular { digest } => {
            let readable = open_frozen_normalization_readonly(pinned.file(), expected_path, deadline)?;
            let readable = fs::File::from_parts(readable, expected_path.to_owned());
            require_frozen_normalization_witness(expected_path, &readable, inventory.witness)?;
            require_frozen_normalization_access_acl(readable.file(), expected_path, deadline)?;

            let active_mode = declaration.mode | 0o400;
            if active_mode != declaration.mode {
                chmod_frozen_normalization_entry(pinned.file(), expected_path, declaration.mode, deadline)?;
            }
            set_frozen_normalization_times(&pinned, expected_path, timestamp, deadline)?;
            let final_stable = frozen_normalization_witness(&pinned, expected_path)?;
            require_frozen_normalization_declaration(expected_path, final_stable, declaration, root_device)?;
            require_frozen_normalization_times(expected_path, &pinned, timestamp)?;
            let final_witness = frozen_normalization_final_witness(&pinned, expected_path)?;
            require_frozen_normalization_regular_digest(&readable, expected_path, *digest, final_witness, deadline)?;
            checkpoint(FrozenNormalizationCheckpoint::AfterRegularDigest, expected_path);
            final_acl_check = Some(readable);
            final_witness
        }
        FrozenExpectedKind::Symlink { target } => {
            let actual = read_frozen_normalization_symlink(&pinned, expected_path, deadline)?;
            if &actual != target {
                return Err(Error::FrozenNormalizationSymlinkTargetMismatch {
                    path: expected_path.to_owned(),
                    expected: OsString::from_vec(target.clone()),
                    actual: OsString::from_vec(actual),
                });
            }
            set_frozen_normalization_times(&pinned, expected_path, timestamp, deadline)?;
            let final_stable = frozen_normalization_witness(&pinned, expected_path)?;
            require_frozen_normalization_declaration(expected_path, final_stable, declaration, root_device)?;
            require_frozen_normalization_times(expected_path, &pinned, timestamp)?;
            frozen_normalization_final_witness(&pinned, expected_path)?
        }
    };

    checkpoint(FrozenNormalizationCheckpoint::BeforeEntryRevalidation, expected_path);
    require_named_frozen_normalization_entry_final(
        parent,
        &inventory.name,
        expected_path,
        &pinned,
        final_witness,
        deadline,
    )?;
    if let Some(file) = final_acl_check {
        require_frozen_normalization_access_acl(file.file(), expected_path, deadline)?;
    }
    Ok(final_witness)
}
