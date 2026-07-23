use super::{artifact_directory::DirectoryHandle, *};

pub(super) fn require_regular_witness(
    path: &Path,
    metadata: &Metadata,
    expected_identity: Identity,
    expected_mode: u32,
    maximum: u64,
) -> Result<(), ArtifactError> {
    if !metadata.file_type().is_file()
        || metadata.nlink() != 1
        || Identity::from_metadata(metadata) != expected_identity
    {
        return Err(ArtifactError::OwnershipChanged { path: path.to_owned() });
    }
    let mode = metadata.mode() & 0o7777;
    if mode != expected_mode {
        return Err(ArtifactError::ModeMismatch {
            role: "sealed artifact",
            path: path.to_owned(),
            expected: expected_mode,
            found: mode,
        });
    }
    if metadata.len() > maximum {
        return Err(ArtifactError::ArtifactTooLarge {
            path: path.to_owned(),
            maximum,
            found: metadata.len(),
        });
    }
    Ok(())
}

pub(super) fn digest_descriptor(file: &File, path: &Path, expected: FileWitness) -> Result<[u8; 32], ArtifactError> {
    let before = file.metadata().map_err(|source| ArtifactError::Io {
        operation: "inspect sealed artifact before hashing",
        path: path.to_owned(),
        source,
    })?;
    if FileWitness::from_metadata(&before) != expected {
        return Err(ArtifactError::ArtifactChanged { path: path.to_owned() });
    }

    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; ARTIFACT_DIGEST_BUFFER_BYTES];
    let mut offset = 0_u64;
    while offset < expected.length {
        let remaining = expected.length - offset;
        let requested = usize::try_from(remaining).unwrap_or(usize::MAX).min(buffer.len());
        let read = loop {
            // SAFETY: file and buffer remain live; requested is bounded by the
            // buffer; offset is below the 2-TiB artifact ceiling and fits off_t.
            let result = unsafe {
                libc::pread(
                    file.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    requested,
                    offset as libc::off_t,
                )
            };
            if result == -1 {
                let source = io::Error::last_os_error();
                if source.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(ArtifactError::Io {
                    operation: "hash sealed artifact",
                    path: path.to_owned(),
                    source,
                });
            }
            break result as usize;
        };
        if read == 0 || read > requested {
            return Err(ArtifactError::ArtifactChanged { path: path.to_owned() });
        }
        hasher.update(&buffer[..read]);
        offset = offset.checked_add(read as u64).ok_or(ArtifactError::ArtifactTooLarge {
            path: path.to_owned(),
            maximum: expected.length,
            found: u64::MAX,
        })?;
    }

    let trailing = loop {
        // SAFETY: the one-byte buffer and descriptor remain live and the
        // offset is bounded as above.
        let result = unsafe { libc::pread(file.as_raw_fd(), buffer.as_mut_ptr().cast(), 1, offset as libc::off_t) };
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(ArtifactError::Io {
                operation: "confirm sealed artifact end",
                path: path.to_owned(),
                source,
            });
        }
        break result;
    };
    if trailing != 0 {
        return Err(ArtifactError::ArtifactChanged { path: path.to_owned() });
    }
    let after = file.metadata().map_err(|source| ArtifactError::Io {
        operation: "inspect sealed artifact after hashing",
        path: path.to_owned(),
        source,
    })?;
    if FileWitness::from_metadata(&after) != expected {
        return Err(ArtifactError::ArtifactChanged { path: path.to_owned() });
    }
    Ok(hasher.finalize().into())
}

// Linux has no unlinkat variant that accepts an expected inode. Emission runs
// beneath the derivation execution lock after all build/analyzer processes are
// gone, and the root is a freshly recreated single-mutator directory while the
// staging child is mode 0700. Within that trust boundary this identity check
// prevents stale or foreign names from being removed. Renaming to a quarantine
// first cannot strengthen the guarantee: a racer can replace the source after
// authentication, causing renameat2 to move the foreign inode, and a later
// no-replace restoration can itself collide. Never broaden this helper to a
// directory writable by concurrent same-UID actors without kernel support for
// conditional unlink.
pub(super) fn remove_owned_entry(
    directory: &DirectoryHandle,
    name: &[u8],
    identity: Identity,
    directory_entry: bool,
    missing_ok: bool,
    operation: &'static str,
) -> Result<(), ArtifactError> {
    let path = directory.display(name);
    let Some((metadata, current_identity)) = directory.inspect(name, operation)? else {
        return if missing_ok {
            Ok(())
        } else {
            Err(ArtifactError::OwnershipChanged { path })
        };
    };
    if current_identity != identity || metadata.file_type().is_dir() != directory_entry {
        return Err(ArtifactError::OwnershipChanged { path });
    }
    let name = c_name(name, &path)?;
    let flags = if directory_entry { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: the parent descriptor and single-component NUL-terminated name
    // remain live. unlinkat never follows a final symlink.
    if unsafe { libc::unlinkat(directory.file.as_raw_fd(), name.as_ptr(), flags) } == -1 {
        return Err(ArtifactError::Io {
            operation,
            path,
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

pub(super) fn rename_noreplace_at(
    source_parent: &DirectoryHandle,
    source_name: &[u8],
    target_parent: &DirectoryHandle,
    target_name: &[u8],
) -> Result<(), ArtifactError> {
    let source_path = source_parent.display(source_name);
    let target_path = target_parent.display(target_name);
    let source_name = c_name(source_name, &source_path)?;
    let target_name = c_name(target_name, &target_path)?;
    // SAFETY: both descriptors and names remain live. Linux renameat2 with
    // RENAME_NOREPLACE atomically installs the staged inode or changes nothing.
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
        Err(ArtifactError::Io {
            operation: "atomically publish staged artifact without replacement",
            path: target_path,
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

pub(super) fn validate_artifact_name(name: &str) -> Result<(), ArtifactError> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 255
        || matches!(bytes, b"." | b"..")
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(ArtifactError::InvalidName { name: name.to_owned() });
    }
    Ok(())
}

pub(super) fn c_name(name: &[u8], path: &Path) -> Result<CString, ArtifactError> {
    if name.contains(&0) {
        return Err(ArtifactError::InvalidName {
            name: path.display().to_string(),
        });
    }
    let requested = name.len().checked_add(1).ok_or(ArtifactError::ResourceLimit {
        resource: "artifact C string bytes",
        limit: usize::MAX,
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(requested)
        .map_err(|source| ArtifactError::Allocation {
            resource: "artifact C string bytes",
            requested,
            detail: source.to_string(),
        })?;
    bytes.extend_from_slice(name);
    bytes.push(0);
    CString::from_vec_with_nul(bytes).map_err(|_| ArtifactError::InvalidName {
        name: path.display().to_string(),
    })
}

pub(super) fn copy_bytes(bytes: &[u8], resource: &'static str) -> Result<Vec<u8>, ArtifactError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|source| ArtifactError::Allocation {
            resource,
            requested: bytes.len(),
            detail: source.to_string(),
        })?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

pub(super) fn one_name(name: &[u8]) -> Result<Vec<Vec<u8>>, ArtifactError> {
    let mut names = Vec::new();
    names.try_reserve_exact(1).map_err(|source| ArtifactError::Allocation {
        resource: "single artifact inventory entry",
        requested: 1,
        detail: source.to_string(),
    })?;
    names.push(copy_bytes(name, "single artifact inventory name")?);
    Ok(names)
}

pub(super) fn copy_name_list(names: &[Vec<u8>], resource: &'static str) -> Result<Vec<Vec<u8>>, ArtifactError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(names.len())
        .map_err(|source| ArtifactError::Allocation {
            resource,
            requested: names.len(),
            detail: source.to_string(),
        })?;
    for name in names {
        copy.push(copy_bytes(name, resource)?);
    }
    Ok(copy)
}

pub(super) fn descendant_resolution() -> u64 {
    libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV
}

pub(super) fn openat2_file(
    dirfd: RawFd,
    path: &[u8],
    flags: i32,
    mode: u32,
    resolve: u64,
    display_path: &Path,
) -> io::Result<File> {
    let path = cstring_io(path)?;
    // SAFETY: zero is valid for every open_how field before the public fields
    // used by this kernel ABI are initialized below.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: path is NUL-terminated, how is initialized, and a successful
    // syscall returns a fresh descriptor owned by this process.
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
    let descriptor = unsafe { OwnedFd::from_raw_fd(result as RawFd) };
    Ok(File::from_parts(descriptor.into(), display_path))
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
    CString::from_vec_with_nul(terminated)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains an interior NUL"))
}
