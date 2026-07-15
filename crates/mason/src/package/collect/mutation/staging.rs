use super::{verification::require_exact_snapshot, witness::ReplacementWitness, *};

pub(super) fn open_original(verified: &VerifiedPath, parent: &File, path: &Path) -> Result<File, Error> {
    let original = open_entry(
        parent,
        &verified.name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
        path,
    )?;
    let original_metadata = metadata(&original, "authenticate original regular file", path)?;
    if !original_metadata.file_type().is_file() {
        return Err(changed(path, "regular-file replacement target changed type"));
    }
    require_exact_snapshot(
        path,
        verified.snapshot,
        &original_metadata,
        "original regular file changed before replacement",
    )?;
    let handle = open_entry_handle(parent, &verified.name, path)?;
    require_exact_snapshot(
        path,
        verified.snapshot,
        &metadata(&handle, "authenticate named original regular file", path)?,
        "named original regular file changed before replacement",
    )?;
    Ok(original)
}

pub(super) fn open_private_stage(parent: &File, path: &Path) -> Result<File, Error> {
    // `O_TMPFILE` keeps incomplete bytes unreachable. Deliberately do not fall
    // back to a named O_EXCL file: that would expose partial analyzer output in
    // the witnessed tree and would not be an equally strong primitive.
    // SAFETY: parent and the static directory component remain live; a
    // successful openat returns a fresh descriptor owned below.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            c".".as_ptr(),
            libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if descriptor == -1 {
        return Err(Error::Io {
            operation: "create private regular-file replacement",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: openat returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) }.into())
}

pub(super) fn write_private_stage(
    staged: &mut File,
    replacement: &[u8],
    original: FileSnapshot,
    transition: &ReplacementWitness<'_>,
    path: &Path,
) -> Result<StagedContent, Error> {
    let initial = metadata(staged, "inspect private regular-file replacement", path)?;
    if !initial.file_type().is_file() || initial.nlink() != 0 {
        return Err(changed(
            path,
            "private regular-file replacement is not an anonymous regular inode",
        ));
    }
    let staged_node = NodeIdentity::from_metadata(&initial);
    if staged_node == original.node {
        return Err(changed(
            path,
            "private replacement unexpectedly reused the original inode",
        ));
    }

    let mut hasher = StoneDigestWriterHasher::new();
    let bytes = u64::try_from(replacement.len()).map_err(|_| Error::ArithmeticOverflow {
        resource: "regular file bytes",
        path: path.to_owned(),
    })?;
    transition.projected_regular_bytes(bytes, path)?;
    for chunk in replacement.chunks(HASH_BUFFER_BYTES) {
        transition.deadline.check(path)?;
        staged.write_all(chunk).map_err(|source| Error::Io {
            operation: "write private regular-file replacement",
            path: path.to_owned(),
            source,
        })?;
        hasher.update(chunk);
    }
    transition.deadline.check(path)?;

    if initial.uid() != original.uid || initial.gid() != original.gid {
        // SAFETY: staged is the still-anonymous inode owned by this transaction.
        if unsafe { libc::fchown(staged.as_raw_fd(), original.uid, original.gid) } == -1 {
            return Err(Error::Io {
                operation: "preserve regular-file replacement ownership",
                path: path.to_owned(),
                source: io::Error::last_os_error(),
            });
        }
    }
    // Ownership is normalized before mode because chown may clear set-ID bits.
    // SAFETY: staged is a live descriptor for the private inode.
    if unsafe { libc::fchmod(staged.as_raw_fd(), original.mode & 0o7777) } == -1 {
        return Err(Error::Io {
            operation: "preserve regular-file replacement mode",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    staged.sync_all().map_err(|source| Error::Io {
        operation: "sync private regular-file replacement",
        path: path.to_owned(),
        source,
    })?;
    transition.deadline.check(path)?;
    let normalized = metadata(staged, "verify private regular-file replacement", path)?;
    if !normalized.file_type().is_file()
        || normalized.nlink() != 0
        || NodeIdentity::from_metadata(&normalized) != staged_node
        || normalized.len() != bytes
        || normalized.uid() != original.uid
        || normalized.gid() != original.gid
        || normalized.mode() != original.mode
    {
        return Err(changed(
            path,
            "private regular-file replacement metadata did not normalize exactly",
        ));
    }
    Ok(StagedContent {
        snapshot: FileSnapshot::from_metadata(&normalized),
        hash: hasher.digest128(),
    })
}

pub(super) fn link_private_stage(
    staged: &File,
    parent: &File,
    deadline: &Deadline,
    path: &Path,
) -> Result<OsString, Error> {
    let process = unsafe { libc::getpid() } as u32;
    let first = NEXT_STAGE_NAME.fetch_add(STAGE_NAME_ATTEMPTS, Ordering::Relaxed);
    let mut last_collision = None;
    for attempt in 0..STAGE_NAME_ATTEMPTS {
        deadline.check(path)?;
        let sequence = first.wrapping_add(attempt);
        let name = OsString::from(format!("{STAGE_NAME_PREFIX}{process:08x}-{sequence:016x}"));
        let c_name = c_name(&name, path)?;
        // Linking the anonymous inode is the first point at which it becomes
        // visible. `AT_EMPTY_PATH` names the exact retained descriptor; the
        // target is rooted in the authenticated parent descriptor.
        // SAFETY: all descriptors and C strings remain live for linkat.
        if unsafe {
            libc::linkat(
                staged.as_raw_fd(),
                c"".as_ptr(),
                parent.as_raw_fd(),
                c_name.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        } == 0
        {
            return Ok(name);
        }
        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::AlreadyExists {
            last_collision = Some(source);
            continue;
        }
        return Err(Error::Io {
            operation: "link complete private regular-file replacement",
            path: path.to_owned(),
            source,
        });
    }
    Err(Error::Io {
        operation: "allocate private regular-file replacement name",
        path: path.to_owned(),
        source: last_collision.unwrap_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists)),
    })
}

pub(super) fn rename_exchange(parent: &File, temporary: &OsStr, target: &OsStr, path: &Path) -> Result<(), Error> {
    let temporary = c_name(temporary, path)?;
    let target = c_name(target, path)?;
    // SAFETY: parent and both names remain live. RENAME_EXCHANGE either swaps
    // the two existing names atomically or changes neither name.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent.as_raw_fd(),
            temporary.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == -1 {
        Err(Error::Io {
            operation: "atomically exchange regular-file replacement",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}
