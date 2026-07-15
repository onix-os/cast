#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallOutcome {
    Installed,
    AlreadyExists,
}

fn require_regular(
    role: &'static str,
    path: &Path,
    metadata: &Metadata,
    maximum: u64,
    expected_mtime: Option<i64>,
) -> Result<(), PublishError> {
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(PublishError::UnexpectedEntry {
            role,
            path: path.to_owned(),
        });
    }
    require_effective_owner(role, path, metadata)?;
    require_mode(role, path, metadata, PUBLISHED_ARTEFACT_MODE)?;
    if metadata.len() > maximum {
        return Err(PublishError::ArtifactTooLarge {
            path: path.to_owned(),
            maximum,
            found: metadata.len(),
        });
    }
    if let Some(expected) = expected_mtime
        && (metadata.mtime() != expected || metadata.mtime_nsec() != 0)
    {
        return Err(PublishError::TimestampMismatch {
            path: path.to_owned(),
            expected,
            seconds: metadata.mtime(),
            nanoseconds: metadata.mtime_nsec(),
        });
    }
    Ok(())
}

fn require_effective_owner(role: &'static str, path: &Path, metadata: &Metadata) -> Result<(), PublishError> {
    // SAFETY: geteuid has no preconditions and does not dereference memory.
    let expected = unsafe { libc::geteuid() };
    let found = metadata.uid();
    if found == expected {
        Ok(())
    } else {
        Err(PublishError::OwnerMismatch {
            role,
            path: path.to_owned(),
            expected,
            found,
        })
    }
}

pub(super) fn reference_owner_is_trusted(found: u32, effective: u32) -> bool {
    found == effective || found == 0
}

fn require_reference_owner(path: &Path, metadata: &Metadata) -> Result<(), PublishError> {
    // SAFETY: geteuid has no preconditions and does not dereference memory.
    let effective = unsafe { libc::geteuid() };
    let found = metadata.uid();
    if reference_owner_is_trusted(found, effective) {
        Ok(())
    } else {
        Err(PublishError::ReferenceOwnerMismatch {
            path: path.to_owned(),
            effective,
            found,
        })
    }
}

fn require_protected_root_mode(role: &'static str, path: &Path, metadata: &Metadata) -> Result<(), PublishError> {
    let found = metadata.mode() & 0o7777;
    if found & 0o022 == 0 {
        Ok(())
    } else {
        Err(PublishError::WritableRoot {
            role,
            path: path.to_owned(),
            found,
        })
    }
}

fn require_mode(role: &'static str, path: &Path, metadata: &Metadata, expected: u32) -> Result<(), PublishError> {
    let found = metadata.mode() & 0o7777;
    if found == expected {
        Ok(())
    } else {
        Err(PublishError::ModeMismatch {
            role,
            path: path.to_owned(),
            expected,
            found,
        })
    }
}

fn require_directory_timestamp(
    path: &Path,
    metadata: &Metadata,
    expected_mtime: Option<i64>,
) -> Result<(), PublishError> {
    if let Some(expected) = expected_mtime
        && (metadata.mtime() != expected || metadata.mtime_nsec() != 0)
    {
        return Err(PublishError::TimestampMismatch {
            path: path.to_owned(),
            expected,
            seconds: metadata.mtime(),
            nanoseconds: metadata.mtime_nsec(),
        });
    }
    Ok(())
}

fn with_cleanup(primary: PublishError, cleanup: Result<(), PublishError>) -> PublishError {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => PublishError::Rollback {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        },
    }
}

fn set_mode(file: &File, path: &Path, mode: u32, role: &'static str) -> Result<(), PublishError> {
    // SAFETY: file is a live descriptor for an authenticated owned inode.
    if unsafe { libc::fchmod(file.as_raw_fd(), mode) } == -1 {
        return Err(PublishError::NormalizeMode {
            role,
            path: path.to_owned(),
            mode,
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

fn set_timestamp(file: &File, path: &Path, seconds: i64) -> Result<(), PublishError> {
    let seconds = libc::time_t::try_from(seconds).map_err(|_| PublishError::InvalidTimestamp { seconds })?;
    let times = [
        libc::timespec {
            tv_sec: seconds,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: seconds,
            tv_nsec: 0,
        },
    ];
    // SAFETY: file and the two initialized timespec values remain live.
    if unsafe { libc::futimens(file.as_raw_fd(), times.as_ptr()) } == -1 {
        return Err(PublishError::Io {
            operation: "normalize publication timestamp",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

// Linux has no unlinkat variant that accepts an expected inode. Publication
// runs beneath the derivation execution lock after every build/analyzer mutator
// has stopped; the emitted root is freshly recreated and the private child is
// mode 0700. Within that documented single-mutator boundary this check avoids
// stale/foreign deletion. Never use it in a concurrently same-UID-writable
// directory without kernel support for conditional unlink.
fn remove_owned_entry(
    directory: &DirectoryHandle,
    name: &[u8],
    identity: Identity,
    directory_entry: bool,
    operation: &'static str,
) -> Result<(), PublishError> {
    let path = directory.display(name);
    let Some((metadata, found)) = directory.inspect(name, operation)? else {
        return Err(PublishError::OwnershipChanged { path });
    };
    if found != identity || metadata.file_type().is_dir() != directory_entry {
        return Err(PublishError::OwnershipChanged { path });
    }
    require_effective_owner("owned publication cleanup", &path, &metadata)?;
    let name = c_name(name, &path)?;
    let flags = if directory_entry { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: descriptor/name remain live; unlinkat does not follow final links.
    if unsafe { libc::unlinkat(directory.file.as_raw_fd(), name.as_ptr(), flags) } == -1 {
        return Err(PublishError::Io {
            operation,
            path,
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

fn rename_noreplace_at(
    source_parent: &DirectoryHandle,
    source_name: &[u8],
    target_parent: &DirectoryHandle,
    target_name: &[u8],
    operation: &'static str,
) -> Result<(), PublishError> {
    let source_path = source_parent.display(source_name);
    let target_path = target_parent.display(target_name);
    let source_name = c_name(source_name, &source_path)?;
    let target_name = c_name(target_name, &target_path)?;
    // SAFETY: both pinned descriptors and both C strings remain live.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source_parent.file.as_raw_fd(),
            source_name.as_ptr(),
            target_parent.file.as_raw_fd(),
            target_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == -1 {
        Err(PublishError::Io {
            operation,
            path: target_path,
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn test_rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    let parent = source
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no parent"))?;
    if target.parent() != Some(parent) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "test paths need one parent",
        ));
    }
    let root = DirectoryHandle::open_root(parent, "test").map_err(io::Error::other)?;
    match rename_noreplace_at(
        &root,
        source.file_name().unwrap().as_bytes(),
        &root,
        target.file_name().unwrap().as_bytes(),
        "test rename",
    ) {
        Ok(()) => Ok(()),
        Err(PublishError::Io { source, .. }) => Err(source),
        Err(error) => Err(io::Error::other(error)),
    }
}

fn descendant_resolution() -> u64 {
    libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV
}

fn openat2_file(dirfd: RawFd, path: &[u8], flags: i32, mode: u32, resolve: u64) -> io::Result<File> {
    let path = cstring_io(path)?;
    // SAFETY: zero initializes all current and future-compatible open_how fields.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: arguments remain live and successful syscall returns a fresh fd.
    let result = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            size_of::<libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat2 returned a fresh owned descriptor.
    Ok(File::from(unsafe { OwnedFd::from_raw_fd(result as RawFd) }))
}

fn validate_component(name: &[u8], role: &'static str) -> Result<(), PublishError> {
    if name.is_empty() || name.len() > 255 || matches!(name, b"." | b"..") || name.contains(&b'/') || name.contains(&0)
    {
        return Err(PublishError::InvalidName {
            role,
            name: OsString::from_vec(copy_bytes(name, "invalid publication name")?),
        });
    }
    Ok(())
}

fn c_name(name: &[u8], path: &Path) -> Result<CString, PublishError> {
    cstring_io(name).map_err(|source| PublishError::Io {
        operation: "encode publication component",
        path: path.to_owned(),
        source,
    })
}

fn cstring_io(bytes: &[u8]) -> io::Result<CString> {
    if bytes.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"));
    }
    let requested = bytes
        .len()
        .checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::OutOfMemory, "path byte count overflow"))?;
    let mut terminated = Vec::new();
    terminated
        .try_reserve_exact(requested)
        .map_err(|source| io::Error::new(io::ErrorKind::OutOfMemory, source))?;
    terminated.extend_from_slice(bytes);
    terminated.push(0);
    CString::from_vec_with_nul(terminated).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn copy_bytes(bytes: &[u8], resource: &'static str) -> Result<Vec<u8>, PublishError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|source| PublishError::Allocation {
            resource,
            requested: bytes.len(),
            detail: source.to_string(),
        })?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

fn os_names(names: &[Vec<u8>]) -> Result<Vec<OsString>, PublishError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(names.len())
        .map_err(|source| PublishError::Allocation {
            resource: "publication error inventory",
            requested: names.len(),
            detail: source.to_string(),
        })?;
    for name in names {
        result.push(OsString::from_vec(copy_bytes(name, "publication error name")?));
    }
    Ok(result)
}

fn hex_prefix(bytes: &[u8]) -> String {
    bytes.iter().take(12).map(|byte| format!("{byte:02x}")).collect()
}
