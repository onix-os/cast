fn frozen_normalization_inventory(
    directory: &fs::File,
    expected_path: &Path,
    expected_entries: usize,
    mut accounting: Option<(&mut usize, usize)>,
    deadline: Instant,
) -> Result<Vec<FrozenNormalizationInventoryEntry>, Error> {
    require_frozen_materialization_deadline(deadline)?;
    let cursor = openat_owned(
        directory.as_raw_fd(),
        ".",
        OFlag::O_CLOEXEC
            | OFlag::O_DIRECTORY
            | OFlag::O_RDONLY
            | OFlag::O_NOFOLLOW
            | OFlag::O_NONBLOCK
            | OFlag::O_NOATIME,
        Mode::empty(),
    )?;
    let descriptor = cursor.into_raw_fd();
    // SAFETY: fdopendir consumes this fresh descriptor on success. On failure
    // it remains ours and is closed explicitly below.
    let stream = unsafe { nix::libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume descriptor.
        unsafe {
            nix::libc::close(descriptor);
        }
        return Err(Error::ReadFrozenNormalizationDirectory {
            path: expected_path.to_owned(),
            source,
        });
    };
    let stream = FrozenDiscardDirectoryStream(stream);
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(expected_entries.saturating_add(1))
        .map_err(|source| Error::ReserveFrozenNormalizationInventory {
            path: expected_path.to_owned(),
            source,
        })?;
    loop {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: errno is thread-local and readdir uses null for both EOF and
        // failure, so clear it immediately before the call.
        unsafe {
            *nix::libc::__errno_location() = 0;
        }
        // SAFETY: stream is live and exclusively used by this loop.
        let entry = unsafe { nix::libc::readdir(stream.0.as_ptr()) };
        if entry.is_null() {
            // SAFETY: errno was cleared immediately before readdir.
            let errno = unsafe { *nix::libc::__errno_location() };
            if errno == 0 {
                break;
            }
            return Err(Error::ReadFrozenNormalizationDirectory {
                path: expected_path.to_owned(),
                source: io::Error::from_raw_os_error(errno),
            });
        }
        // SAFETY: readdir returned a NUL-terminated name valid until the next
        // call; copy it before advancing the stream.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let name = CString::new(bytes).expect("directory entry names contain no interior NUL");
        if entries.len() >= expected_entries {
            return Err(Error::FrozenNormalizationInventoryMismatch {
                path: expected_path.join(OsStr::from_bytes(name.as_bytes())),
                reason: "the filesystem contains an undeclared entry",
            });
        }
        if let Some((inodes, limit)) = accounting.as_mut() {
            let actual = inodes.saturating_add(1);
            if actual > *limit {
                return Err(Error::FrozenNormalizationInodeLimit { limit: *limit, actual });
            }
            **inodes = actual;
        }
        let witness = fstatat_frozen_normalization_entry(directory.as_raw_fd(), &name, expected_path, deadline)?;
        entries.push(FrozenNormalizationInventoryEntry { name, witness });
    }
    entries.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    require_frozen_materialization_deadline(deadline)?;
    Ok(entries)
}

fn fstatat_frozen_normalization_entry(
    directory: RawFd,
    name: &CStr,
    parent: &Path,
    deadline: Instant,
) -> Result<FrozenNormalizationWitness, Error> {
    let mut metadata = MaybeUninit::<nix::libc::stat>::uninit();
    loop {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: directory and name are live, metadata points to writable
        // storage, and AT_SYMLINK_NOFOLLOW prevents target traversal.
        if unsafe {
            nix::libc::fstatat(
                directory,
                name.as_ptr(),
                metadata.as_mut_ptr(),
                nix::libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            break;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(Error::InspectFrozenNormalizationEntry {
                path: parent.join(OsStr::from_bytes(name.to_bytes())),
                source,
            });
        }
    }
    // SAFETY: successful fstatat initialized the complete stat value.
    let metadata = unsafe { metadata.assume_init() };
    Ok(FrozenNormalizationWitness {
        device: metadata.st_dev,
        inode: metadata.st_ino,
        mode: metadata.st_mode,
        owner: metadata.st_uid,
        group: metadata.st_gid,
        links: metadata.st_nlink,
        length: u64::try_from(metadata.st_size).unwrap_or(0),
    })
}

fn require_frozen_normalization_inventory(
    parent: &Path,
    actual: &[FrozenNormalizationInventoryEntry],
    expected_children: &[(&OsString, &PathBuf)],
    expected: &FrozenExpectedTree,
    root_device: u64,
) -> Result<(), Error> {
    if actual.len() != expected_children.len() {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: parent.to_owned(),
            reason: "the filesystem is missing a declared entry",
        });
    }
    for (actual, (expected_name, expected_path)) in actual.iter().zip(expected_children) {
        if actual.name.as_bytes() != expected_name.as_bytes() {
            return Err(Error::FrozenNormalizationInventoryMismatch {
                path: parent.join(OsStr::from_bytes(actual.name.as_bytes())),
                reason: "the filesystem entry name is not declared",
            });
        }
        require_frozen_normalization_declaration(
            expected_path,
            actual.witness,
            expected.entry(expected_path)?,
            root_device,
        )?;
    }
    Ok(())
}

fn require_frozen_normalization_active_inventory(
    parent: &Path,
    original: &[FrozenNormalizationInventoryEntry],
    actual: &[FrozenNormalizationInventoryEntry],
    expected_children: &[(&OsString, &PathBuf)],
    expected: &FrozenExpectedTree,
    root_device: u64,
) -> Result<(), Error> {
    if original.len() != expected_children.len() || actual.len() != expected_children.len() {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: parent.to_owned(),
            reason: "the active filesystem inventory differs from its declaration",
        });
    }
    for ((original, actual), (expected_name, expected_path)) in original.iter().zip(actual).zip(expected_children) {
        if original.name.as_bytes() != expected_name.as_bytes() || actual.name != original.name {
            return Err(Error::FrozenNormalizationInventoryMismatch {
                path: parent.join(OsStr::from_bytes(actual.name.as_bytes())),
                reason: "an active filesystem entry name differs from its declaration",
            });
        }
        let declaration = expected.entry(expected_path)?;
        let expected_witness = original
            .witness
            .with_permissions(frozen_normalization_active_mode(declaration));
        if actual.witness != expected_witness {
            return Err(Error::FrozenNormalizationEntryChanged((*expected_path).clone()));
        }
        require_frozen_normalization_active_declaration(expected_path, actual.witness, declaration, root_device)?;
    }
    Ok(())
}

fn require_frozen_normalization_active_declarations(
    parent: &Path,
    actual: &[FrozenNormalizationInventoryEntry],
    expected_children: &[(&OsString, &PathBuf)],
    expected: &FrozenExpectedTree,
    root_device: u64,
) -> Result<(), Error> {
    if actual.len() != expected_children.len() {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: parent.to_owned(),
            reason: "the final pass found a missing declared entry",
        });
    }
    for (actual, (expected_name, expected_path)) in actual.iter().zip(expected_children) {
        if actual.name.as_bytes() != expected_name.as_bytes() {
            return Err(Error::FrozenNormalizationInventoryMismatch {
                path: parent.join(OsStr::from_bytes(actual.name.as_bytes())),
                reason: "the final pass found an undeclared filesystem name",
            });
        }
        require_frozen_normalization_active_declaration(
            expected_path,
            actual.witness,
            expected.entry(expected_path)?,
            root_device,
        )?;
    }
    Ok(())
}

fn frozen_normalization_active_mode(expected: &FrozenExpectedEntry) -> u32 {
    match expected.kind {
        FrozenExpectedKind::Directory => expected.mode | 0o700,
        FrozenExpectedKind::Regular { .. } => expected.mode | 0o400,
        FrozenExpectedKind::Symlink { .. } => expected.mode,
    }
}

fn require_frozen_normalization_active_declaration(
    path: &Path,
    witness: FrozenNormalizationWitness,
    expected: &FrozenExpectedEntry,
    root_device: u64,
) -> Result<(), Error> {
    let mut active = expected.clone();
    active.mode = frozen_normalization_active_mode(expected);
    require_frozen_normalization_declaration(path, witness, &active, root_device)
}

fn frozen_normalization_final_inventory(
    directory: &fs::File,
    expected_path: &Path,
    expected_entries: usize,
    deadline: Instant,
) -> Result<Vec<(CString, FrozenNormalizationFinalWitness)>, Error> {
    let inventory = frozen_normalization_inventory(directory, expected_path, expected_entries, None, deadline)?;
    let mut final_inventory = Vec::new();
    final_inventory.try_reserve_exact(inventory.len()).map_err(|source| {
        Error::ReserveFrozenNormalizationInventory {
            path: expected_path.to_owned(),
            source,
        }
    })?;
    for entry in inventory {
        require_frozen_materialization_deadline(deadline)?;
        let child_path = expected_path.join(OsStr::from_bytes(entry.name.as_bytes()));
        let pinned = open_frozen_normalization_entry(
            directory,
            &entry.name,
            &child_path,
            FrozenNormalizationOpen::Anchor,
            deadline,
        )?;
        require_frozen_normalization_witness(&child_path, &pinned, entry.witness)?;
        final_inventory.push((entry.name, frozen_normalization_final_witness(&pinned, &child_path)?));
    }
    Ok(final_inventory)
}

fn require_frozen_normalization_final_inventory(
    parent: &Path,
    actual: &[(CString, FrozenNormalizationFinalWitness)],
    expected: &[(CString, PathBuf, FrozenNormalizationFinalWitness)],
) -> Result<(), Error> {
    if actual.len() != expected.len() {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: parent.to_owned(),
            reason: "the sealed filesystem inventory changed before parent sealing",
        });
    }
    for ((actual_name, actual_witness), (expected_name, expected_path, expected_witness)) in actual.iter().zip(expected)
    {
        if actual_name != expected_name {
            return Err(Error::FrozenNormalizationInventoryMismatch {
                path: parent.join(OsStr::from_bytes(actual_name.as_bytes())),
                reason: "a sealed filesystem name changed before parent sealing",
            });
        }
        if actual_witness != expected_witness {
            return Err(Error::FrozenNormalizationEntryChanged(expected_path.clone()));
        }
    }
    Ok(())
}

fn open_frozen_normalization_entry(
    parent: &fs::File,
    name: &CStr,
    expected_path: &Path,
    open: FrozenNormalizationOpen,
    deadline: Instant,
) -> Result<fs::File, Error> {
    let mut flags = nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW;
    flags |= match open {
        FrozenNormalizationOpen::Anchor => nix::libc::O_PATH,
        FrozenNormalizationOpen::Directory => nix::libc::O_RDONLY | nix::libc::O_DIRECTORY | nix::libc::O_NONBLOCK,
    };
    require_frozen_materialization_deadline(deadline)?;
    openat2_frozen_until(
        parent.as_raw_fd(),
        Path::new(OsStr::from_bytes(name.to_bytes())),
        flags,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    )
    .map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::OpenFrozenNormalizationEntry {
            path: expected_path.to_owned(),
            source,
        })
    })
}

fn require_named_frozen_normalization_entry(
    parent: &fs::File,
    name: &CStr,
    expected_path: &Path,
    expected: FrozenNormalizationWitness,
    deadline: Instant,
) -> Result<(), Error> {
    let named =
        open_frozen_normalization_entry(parent, name, expected_path, FrozenNormalizationOpen::Anchor, deadline)?;
    require_frozen_normalization_witness(expected_path, &named, expected)
}

fn require_named_frozen_normalization_entry_final(
    parent: &fs::File,
    name: &CStr,
    expected_path: &Path,
    pinned: &fs::File,
    expected: FrozenNormalizationFinalWitness,
    deadline: Instant,
) -> Result<(), Error> {
    let named =
        open_frozen_normalization_entry(parent, name, expected_path, FrozenNormalizationOpen::Anchor, deadline)?;
    let retained = frozen_normalization_final_witness(pinned, expected_path)?;
    let named = frozen_normalization_final_witness(&named, expected_path)?;
    if retained == expected && named == expected {
        Ok(())
    } else {
        Err(Error::FrozenNormalizationEntryChanged(expected_path.to_owned()))
    }
}

fn require_named_frozen_normalization_root(
    path: &Path,
    root: &fs::File,
    expected: FrozenNormalizationWitness,
    deadline: Instant,
) -> Result<(), Error> {
    let retained = frozen_normalization_witness(root, Path::new("/"))?;
    let named = match open_frozen_root_anchor_until(path, deadline) {
        Ok(named) => named,
        Err(error @ Error::FrozenMaterializationTimeout { .. }) => return Err(error),
        Err(_) => return Err(Error::FrozenNormalizationRootChanged(path.to_owned())),
    };
    let named = frozen_normalization_witness(&named, Path::new("/"))?;
    if retained != expected || named != expected {
        return Err(Error::FrozenNormalizationRootChanged(path.to_owned()));
    }
    Ok(())
}

fn require_named_frozen_normalization_root_final(
    path: &Path,
    root: &fs::File,
    expected: FrozenNormalizationFinalWitness,
    deadline: Instant,
) -> Result<(), Error> {
    let retained = frozen_normalization_final_witness(root, Path::new("/"))?;
    let named = match open_frozen_root_anchor_until(path, deadline) {
        Ok(named) => named,
        Err(error @ Error::FrozenMaterializationTimeout { .. }) => return Err(error),
        Err(_) => return Err(Error::FrozenNormalizationRootChanged(path.to_owned())),
    };
    let named = frozen_normalization_final_witness(&named, Path::new("/"))?;
    if retained == expected && named == expected {
        Ok(())
    } else {
        Err(Error::FrozenNormalizationRootChanged(path.to_owned()))
    }
}

fn frozen_normalization_witness(file: &fs::File, path: &Path) -> Result<FrozenNormalizationWitness, Error> {
    file.metadata()
        .map(|metadata| FrozenNormalizationWitness::from_metadata(&metadata))
        .map_err(|source| Error::InspectFrozenNormalizationEntry {
            path: path.to_owned(),
            source,
        })
}

fn frozen_normalization_final_witness(file: &fs::File, path: &Path) -> Result<FrozenNormalizationFinalWitness, Error> {
    file.metadata()
        .map(|metadata| FrozenNormalizationFinalWitness::from_metadata(&metadata))
        .map_err(|source| Error::InspectFrozenNormalizationEntry {
            path: path.to_owned(),
            source,
        })
}

fn require_frozen_normalization_witness(
    path: &Path,
    file: &fs::File,
    expected: FrozenNormalizationWitness,
) -> Result<(), Error> {
    if frozen_normalization_witness(file, path)? == expected {
        Ok(())
    } else {
        Err(Error::FrozenNormalizationEntryChanged(path.to_owned()))
    }
}

fn require_frozen_normalization_declaration(
    path: &Path,
    witness: FrozenNormalizationWitness,
    expected: &FrozenExpectedEntry,
    root_device: u64,
) -> Result<(), Error> {
    // SAFETY: geteuid takes no arguments and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    if witness.owner != effective_owner {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the materialized inode is not owned by the effective user",
        });
    }
    if witness.device != root_device {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the materialized inode resides on another filesystem",
        });
    }
    let actual_kind = witness.mode & nix::libc::S_IFMT;
    let expected_kind = match expected.kind {
        FrozenExpectedKind::Directory => nix::libc::S_IFDIR,
        FrozenExpectedKind::Regular { .. } => nix::libc::S_IFREG,
        FrozenExpectedKind::Symlink { .. } => nix::libc::S_IFLNK,
    };
    if actual_kind != expected_kind {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the filesystem inode type differs from its declaration",
        });
    }
    if witness.mode & 0o7777 != expected.mode {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the filesystem mode differs from its declaration",
        });
    }
    if matches!(
        expected.kind,
        FrozenExpectedKind::Regular { .. } | FrozenExpectedKind::Symlink { .. }
    ) && witness.links != 1
    {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "a declarative regular file or symlink must have exactly one name",
        });
    }
    Ok(())
}

fn set_frozen_normalization_times(
    file: &fs::File,
    path: &Path,
    timestamp: FileTime,
    deadline: Instant,
) -> Result<(), Error> {
    set_path_descriptor_times_until(
        file.file(),
        timestamp.unix_seconds(),
        i64::from(timestamp.nanoseconds()),
        deadline,
    )
    .map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenEntryTime {
            path: path.to_owned(),
            source,
        })
    })
}

fn require_frozen_normalization_regular_digest(
    file: &fs::File,
    path: &Path,
    expected_digest: u128,
    expected_witness: FrozenNormalizationFinalWitness,
    deadline: Instant,
) -> Result<(), Error> {
    if expected_witness.stable.length > MAX_BLIT_ASSET_BYTES {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the regular file exceeds the bounded asset size",
        });
    }
    let before = file
        .metadata()
        .map_err(|source| Error::InspectFrozenNormalizationEntry {
            path: path.to_owned(),
            source,
        })?;
    if FrozenNormalizationFinalWitness::from_metadata(&before) != expected_witness {
        return Err(Error::FrozenNormalizationEntryChanged(path.to_owned()));
    }
    let mut hasher = StoneDigestWriterHasher::new();
    let mut remaining = expected_witness.stable.length;
    let mut buffer = [0_u8; ASSET_COPY_BUFFER_BYTES];
    while remaining != 0 {
        require_frozen_materialization_deadline(deadline)?;
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            Error::FrozenNormalizationInventoryMismatch {
                path: path.to_owned(),
                reason: "the regular file length is not representable",
            }
        })?;
        let offset = nix::libc::off_t::try_from(expected_witness.stable.length - remaining).map_err(|_| {
            Error::FrozenNormalizationInventoryMismatch {
                path: path.to_owned(),
                reason: "the regular file offset is not representable",
            }
        })?;
        let count = loop {
            require_frozen_materialization_deadline(deadline)?;
            // SAFETY: the retained readable descriptor and writable buffer
            // remain live, and the explicit bounded offset is representable.
            let count = unsafe { nix::libc::pread(file.as_raw_fd(), buffer.as_mut_ptr().cast(), requested, offset) };
            if count >= 0 {
                break usize::try_from(count).map_err(|_| Error::FrozenNormalizationInventoryMismatch {
                    path: path.to_owned(),
                    reason: "pread returned an invalid byte count",
                })?;
            }
            let source = Errno::last();
            if source != Errno::EINTR {
                return Err(Error::Blit(source));
            }
        };
        match count {
            0 => {
                return Err(Error::FrozenNormalizationInventoryMismatch {
                    path: path.to_owned(),
                    reason: "the regular file ended before its pinned length",
                });
            }
            _ => {}
        }
        hasher.update(&buffer[..count]);
        remaining = remaining.saturating_sub(count as u64);
    }
    let trailing_offset = nix::libc::off_t::try_from(expected_witness.stable.length).map_err(|_| {
        Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the trailing regular file offset is not representable",
        }
    })?;
    let trailing = loop {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: the retained readable descriptor and one-byte writable
        // buffer remain live for the bounded explicit offset.
        let count = unsafe { nix::libc::pread(file.as_raw_fd(), buffer.as_mut_ptr().cast(), 1, trailing_offset) };
        if count >= 0 {
            break usize::try_from(count).map_err(|_| Error::FrozenNormalizationInventoryMismatch {
                path: path.to_owned(),
                reason: "trailing pread returned an invalid byte count",
            })?;
        }
        let source = Errno::last();
        if source != Errno::EINTR {
            return Err(Error::Blit(source));
        }
    };
    if trailing != 0 {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the regular file grew beyond its pinned length",
        });
    }
    if hasher.digest128() != expected_digest {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the regular file content digest differs from its declaration",
        });
    }
    let after = file
        .metadata()
        .map_err(|source| Error::InspectFrozenNormalizationEntry {
            path: path.to_owned(),
            source,
        })?;
    if FrozenNormalizationFinalWitness::from_metadata(&after) != expected_witness {
        return Err(Error::FrozenNormalizationEntryChanged(path.to_owned()));
    }
    Ok(())
}

fn require_frozen_normalization_times(file_path: &Path, file: &fs::File, timestamp: FileTime) -> Result<(), Error> {
    let metadata = file
        .metadata()
        .map_err(|source| Error::InspectFrozenNormalizationEntry {
            path: file_path.to_owned(),
            source,
        })?;
    if metadata.atime() == timestamp.unix_seconds()
        && metadata.atime_nsec() == i64::from(timestamp.nanoseconds())
        && metadata.mtime() == timestamp.unix_seconds()
        && metadata.mtime_nsec() == i64::from(timestamp.nanoseconds())
    {
        Ok(())
    } else {
        Err(Error::FrozenNormalizationEntryChanged(file_path.to_owned()))
    }
}

fn read_frozen_normalization_symlink(file: &fs::File, path: &Path, deadline: Instant) -> Result<Vec<u8>, Error> {
    let mut target = vec![0_u8; MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1];
    loop {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: the retained O_PATH descriptor and empty path name the exact
        // symlink inode, and target is writable for its complete capacity.
        let read =
            unsafe { nix::libc::readlinkat(file.as_raw_fd(), c"".as_ptr(), target.as_mut_ptr().cast(), target.len()) };
        if read >= 0 {
            let read = usize::try_from(read).map_err(|_| Error::ReadFrozenNormalizationSymlink {
                path: path.to_owned(),
                source: io::Error::other("readlinkat returned an invalid length"),
            })?;
            if read == target.len() {
                return Err(Error::FrozenNormalizationInventoryMismatch {
                    path: path.to_owned(),
                    reason: "the symlink target exceeds the declarative byte limit",
                });
            }
            target.truncate(read);
            return Ok(target);
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(Error::ReadFrozenNormalizationSymlink {
                path: path.to_owned(),
                source,
            });
        }
    }
}

/// Restore owner traversal and mutation permissions before replacing a tree.
///
/// Frozen package metadata may legitimately make a directory read-only. The
/// next materialization still has to be able to remove children within that
/// directory. Symlinks are never followed.
#[cfg(test)]
fn make_tree_removable(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Ok(());
    }

    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    fs::set_permissions(path, permissions)?;

    let mut children = fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    children.sort();
    for child in children {
        make_tree_removable(&child)?;
    }
    Ok(())
}
