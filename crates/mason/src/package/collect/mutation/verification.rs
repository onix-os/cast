use super::*;

pub(super) fn require_named_regular(
    parent: &File,
    name: &OsStr,
    lineage: FileSnapshot,
    links: u64,
    operation: &'static str,
    path: &Path,
) -> Result<(File, FileSnapshot), Error> {
    let file = open_entry(
        parent,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
        path,
    )?;
    let current = metadata(&file, operation, path)?;
    require_regular_lineage(&current, lineage, links, path, "regular-file replacement inode changed")?;
    let handle = open_entry_handle(parent, name, path)?;
    let named = metadata(&handle, "reauthenticate named regular-file replacement", path)?;
    let snapshot = FileSnapshot::from_metadata(&current);
    require_exact_snapshot(
        path,
        snapshot,
        &named,
        "named regular-file replacement changed between descriptor opens",
    )?;
    Ok((file, snapshot))
}

pub(super) fn require_regular_lineage(
    metadata: &std::fs::Metadata,
    lineage: FileSnapshot,
    links: u64,
    path: &Path,
    detail: &'static str,
) -> Result<(), Error> {
    let actual = FileSnapshot::from_metadata(metadata);
    if metadata.file_type().is_file()
        && actual.node == lineage.node
        && actual.size == lineage.size
        && actual.mode == lineage.mode
        && actual.uid == lineage.uid
        && actual.gid == lineage.gid
        && actual.links == links
    {
        Ok(())
    } else {
        Err(changed(path, detail))
    }
}

pub(super) fn require_exact_snapshot(
    path: &Path,
    expected: FileSnapshot,
    metadata: &std::fs::Metadata,
    detail: &'static str,
) -> Result<(), Error> {
    if FileSnapshot::from_metadata(metadata) == expected {
        Ok(())
    } else {
        Err(changed(path, detail))
    }
}

pub(super) fn hash_open_regular(
    file: &mut File,
    expected: FileSnapshot,
    deadline: Option<&Deadline>,
    path: &Path,
) -> Result<u128, Error> {
    require_exact_snapshot(
        path,
        expected,
        &metadata(file, "inspect regular-file replacement before hashing", path)?,
        "regular-file replacement changed before hashing",
    )?;
    let mut hasher = StoneDigestWriterHasher::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; HASH_BUFFER_BYTES];
    loop {
        if let Some(deadline) = deadline {
            deadline.check(path)?;
        }
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(Error::Io {
                    operation: "verify regular-file replacement bytes",
                    path: path.to_owned(),
                    source,
                });
            }
        };
        if read == 0 {
            break;
        }
        bytes = bytes.checked_add(read as u64).ok_or(Error::ArithmeticOverflow {
            resource: "regular file bytes",
            path: path.to_owned(),
        })?;
        if bytes > expected.size {
            return Err(changed(path, "regular-file replacement grew while being verified"));
        }
        hasher.update(&buffer[..read]);
    }
    if bytes != expected.size {
        return Err(changed(
            path,
            "regular-file replacement changed size while being verified",
        ));
    }
    require_exact_snapshot(
        path,
        expected,
        &metadata(file, "reinspect hashed regular-file replacement", path)?,
        "regular-file replacement metadata changed while hashing",
    )?;
    if let Some(deadline) = deadline {
        deadline.check(path)?;
    }
    Ok(hasher.digest128())
}

pub(super) fn sync_parent(parent: &File, operation: &'static str, path: &Path) -> Result<(), Error> {
    parent.sync_all().map_err(|source| Error::Io {
        operation,
        path: path.to_owned(),
        source,
    })
}

// Linux does not expose a conditional unlink which accepts an expected inode.
// This transaction runs under Mason's derivation execution lock after analyzer
// children are reaped; the identity check therefore protects against stale
// names inside the collector's single-mutator boundary. Do not reuse this in a
// directory writable by a concurrent same-UID mutator.
pub(super) fn unlink_owned(
    parent: &File,
    name: &OsStr,
    identity: NodeIdentity,
    operation: &'static str,
    path: &Path,
) -> Result<(), Error> {
    let handle = open_entry_handle(parent, name, path)?;
    let current = metadata(&handle, "authenticate regular-file replacement cleanup", path)?;
    if current.file_type().is_dir() || NodeIdentity::from_metadata(&current) != identity {
        return Err(changed(path, "regular-file replacement cleanup name changed ownership"));
    }
    let name = c_name(name, path)?;
    // SAFETY: the authenticated parent and single-component name remain live;
    // unlinkat does not follow the final component.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == -1 {
        return Err(Error::Io {
            operation,
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

pub(super) fn require_exact_membership(
    parent: &File,
    expected: &[WitnessChild],
    temporary: Option<&OsStr>,
    deadline: &Deadline,
    path: &Path,
) -> Result<FileSnapshot, Error> {
    deadline.check(path)?;
    let before = FileSnapshot::from_metadata(&metadata(
        parent,
        "inspect regular-file replacement parent membership",
        path,
    )?);
    let cursor = open_entry(
        parent,
        OsStr::new("."),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        path,
    )?;
    let descriptor = cursor.into_raw_fd();
    // SAFETY: descriptor is a fresh owned directory descriptor. fdopendir
    // consumes it on success; it remains ours on failure.
    let stream = unsafe { libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed without consuming descriptor.
        unsafe { libc::close(descriptor) };
        return Err(Error::Io {
            operation: "enumerate regular-file replacement parent",
            path: path.to_owned(),
            source,
        });
    };
    let stream = DirectoryStream(stream);
    let mut found = 0usize;
    let mut temporary_found = false;
    loop {
        deadline.check(path)?;
        // SAFETY: errno is thread-local on Linux.
        unsafe { *libc::__errno_location() = 0 };
        // SAFETY: stream is live and exclusively owned by this loop.
        let entry = unsafe { libc::readdir(stream.0.as_ptr()) };
        if entry.is_null() {
            // SAFETY: errno was cleared immediately before readdir.
            let errno = unsafe { *libc::__errno_location() };
            if errno == 0 {
                break;
            }
            return Err(Error::Io {
                operation: "enumerate regular-file replacement parent",
                path: path.to_owned(),
                source: io::Error::from_raw_os_error(errno),
            });
        }
        // SAFETY: readdir returned a NUL-terminated name valid until the next
        // operation on this stream.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        let name = OsStr::from_bytes(name);
        if temporary.is_some_and(|temporary| temporary == name) {
            if temporary_found {
                return Err(changed(path, "regular-file replacement temporary name was duplicated"));
            }
            temporary_found = true;
        } else if expected
            .binary_search_by(|child| child.name.as_os_str().cmp(name))
            .is_ok()
        {
            found = found.checked_add(1).ok_or(Error::ArithmeticOverflow {
                resource: "regular-file replacement parent entries",
                path: path.to_owned(),
            })?;
        } else {
            return Err(changed(path, "regular-file replacement parent membership changed"));
        }
    }
    drop(stream);
    if found != expected.len() || temporary.is_some() != temporary_found {
        return Err(changed(path, "regular-file replacement parent membership changed"));
    }
    let after = FileSnapshot::from_metadata(&metadata(
        parent,
        "reinspect regular-file replacement parent membership",
        path,
    )?);
    if before != after {
        return Err(changed(
            path,
            "regular-file replacement parent changed during enumeration",
        ));
    }
    deadline.check(path)?;
    Ok(after)
}

struct DirectoryStream(NonNull<libc::DIR>);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the fdopendir stream.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}
