fn discard_frozen_directory(
    directory: &fs::File,
    directory_path: &Path,
    depth: usize,
    entries: &mut usize,
    deadline: Instant,
) -> Result<(), Error> {
    require_frozen_materialization_deadline(deadline)?;
    if depth > MAX_FROZEN_LAYOUT_PATH_COMPONENTS {
        return Err(Error::FrozenDiscardDepthLimit {
            limit: MAX_FROZEN_LAYOUT_PATH_COMPONENTS,
            actual: depth,
        });
    }

    let names = frozen_discard_entry_names(directory.as_raw_fd(), entries, deadline)?;
    for name in names {
        require_frozen_materialization_deadline(deadline)?;
        let child_name = Path::new(OsStr::from_bytes(name.as_bytes()));
        let child_path = directory_path.join(child_name);
        let resolution = nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV;
        let anchor = openat2_frozen_until(
            directory.as_raw_fd(),
            child_name,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            resolution,
            deadline,
        )
        .map_err(|source| Error::OpenFrozenDiscardEntry {
            path: child_path.clone(),
            source,
        })?;
        let anchored_before = anchor.metadata()?;
        if anchored_before.mode() & nix::libc::S_IFMT == nix::libc::S_IFDIR {
            // Prove this name is an ordinary directory on the same filesystem
            // before chmod touches it. In particular, a hostile mount point
            // fails RESOLVE_NO_XDEV without changing the mounted root's mode.
            chmod_path_descriptor_until(anchor.file(), anchored_before.mode() & 0o7777 | 0o700, deadline).map_err(
                |source| {
                    frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenPrivateDirectory {
                        path: child_path.clone(),
                        source,
                    })
                },
            )?;
            let expected = frozen_root_identity(&anchor, &child_path)?;
            let child = openat2_frozen_until(
                directory.as_raw_fd(),
                child_name,
                nix::libc::O_RDONLY
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK,
                resolution,
                deadline,
            )
            .map_err(|source| Error::OpenFrozenDiscardEntry {
                path: child_path.clone(),
                source,
            })?;
            if frozen_root_identity(&child, &child_path)? != expected {
                return Err(Error::FrozenDiscardEntryChanged);
            }
            discard_frozen_directory(&child, &child_path, depth + 1, entries, deadline)?;
            drop(child);
            if frozen_root_identity(&anchor, &child_path)? != expected {
                return Err(Error::FrozenDiscardEntryChanged);
            }
            // Linux has no inode-conditional unlink. The private 0700 wrapper
            // and cooperating-writer lock are therefore the final-component
            // race boundary; post-syscall reconciliation still refuses to
            // retry against a foreign replacement.
            unlink_frozen_discard_entry_until(
                directory,
                name.as_c_str(),
                &child_path,
                expected,
                UnlinkatFlags::RemoveDir,
                deadline,
            )?;
        } else {
            let expected = frozen_root_identity(&anchor, &child_path)?;
            unlink_frozen_discard_entry_until(
                directory,
                name.as_c_str(),
                &child_path,
                expected,
                UnlinkatFlags::NoRemoveDir,
                deadline,
            )?;
        }
    }
    Ok(())
}

fn frozen_discard_entry_names(directory: RawFd, entries: &mut usize, deadline: Instant) -> Result<Vec<CString>, Error> {
    require_frozen_materialization_deadline(deadline)?;
    let cursor = openat2_frozen_until(
        directory,
        Path::new("."),
        nix::libc::O_CLOEXEC
            | nix::libc::O_DIRECTORY
            | nix::libc::O_RDONLY
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    )
    .map_err(|source| Error::OpenFrozenDiscardDirectory { source })?;
    let descriptor = cursor.into_raw_fd();
    // SAFETY: fdopendir consumes this fresh owned descriptor on success. On
    // failure it remains ours and is closed explicitly below.
    let stream = unsafe { nix::libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume descriptor.
        unsafe {
            nix::libc::close(descriptor);
        }
        return Err(Error::ReadFrozenDiscardDirectory { source });
    };
    let stream = FrozenDiscardDirectoryStream(stream);
    let mut names = Vec::new();
    let mut interruptions = 0usize;
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
            if errno == nix::libc::EINTR && interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS {
                interruptions += 1;
                continue;
            }
            return Err(Error::ReadFrozenDiscardDirectory {
                source: io::Error::from_raw_os_error(errno),
            });
        }
        // SAFETY: readdir returned a NUL-terminated name valid until the next
        // call; copy it before advancing the stream.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let actual = entries.saturating_add(1);
        if actual > MAX_FROZEN_NORMALIZED_INODES {
            return Err(Error::FrozenDiscardEntryLimit {
                limit: MAX_FROZEN_NORMALIZED_INODES,
                actual,
            });
        }
        *entries = actual;
        names.push(CString::new(bytes).expect("directory entry names contain no interior NUL"));
    }
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    require_frozen_materialization_deadline(deadline)?;
    Ok(names)
}
