fn normalize_entries(
    root: &RootHandle,
    entries: &[Entry],
    source_date_epoch: i64,
    limits: MaterializationLimits,
    deadline: &Deadline,
) -> Result<(), Error> {
    let timestamp = filetime::FileTime::from_unix_time(source_date_epoch, 0);

    // Children precede their directories, so directory timestamps are the
    // final metadata operation for each subtree.
    for entry in entries.iter().rev() {
        let path = root.display_path(&entry.relative);
        deadline.check(&path)?;
        match &entry.kind {
            EntryKind::Symlink { .. } => {
                let file = root.open_inspection(entry)?;
                require_symlink_handle_matches(entry, &file, true, &path, limits, deadline)?;
                set_symlink_handle_times(&file, &path, source_date_epoch)?;
                // Replacing a symlink requires a new inode, so fstat identity
                // and type are sufficient after the timestamp write.
                let metadata = file.metadata().map_err(|source| Error::Io {
                    operation: "verify timestamped Git materialization symlink",
                    path: path.clone(),
                    source,
                })?;
                if Identity::from_metadata(&metadata) != entry.identity || !metadata.file_type().is_symlink() {
                    return Err(Error::EntryChanged(path));
                }
                require_single_link(&path, &metadata, &entry.kind)?;
            }
            EntryKind::Directory | EntryKind::Regular { .. } => {
                let file = root.open_data(entry)?;
                require_handle_matches(
                    entry,
                    &file.metadata().map_err(|source| Error::Io {
                        operation: "inspect opened Git materialization entry",
                        path: path.clone(),
                        source,
                    })?,
                    false,
                    &path,
                )?;
                file.set_permissions(Permissions::from_mode(entry.kind.normalized_mode()))
                    .map_err(|source| Error::Io {
                        operation: "normalize Git materialization mode",
                        path: path.clone(),
                        source,
                    })?;
                filetime::set_file_handle_times(file.file(), Some(timestamp), Some(timestamp)).map_err(|source| {
                    Error::Io {
                        operation: "normalize Git materialization timestamp",
                        path: path.clone(),
                        source,
                    }
                })?;
                require_handle_matches(
                    entry,
                    &file.metadata().map_err(|source| Error::Io {
                        operation: "verify opened Git materialization entry",
                        path: path.clone(),
                        source,
                    })?,
                    true,
                    &path,
                )?;
            }
        }
        deadline.check(&path)?;
    }
    Ok(())
}

fn set_symlink_handle_times(file: &fs::File, path: &Path, source_date_epoch: i64) -> Result<(), Error> {
    let seconds = libc::time_t::try_from(source_date_epoch).map_err(|_| Error::Io {
        operation: "represent Git materialization symlink timestamp",
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "timestamp is outside time_t"),
    })?;
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
    // SAFETY: the descriptor is live, the empty path is NUL-terminated, and
    // `times` contains the two initialized timespec values required by Linux.
    let result = unsafe {
        libc::utimensat(
            file.as_raw_fd(),
            c"".as_ptr(),
            times.as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == -1 {
        Err(Error::Io {
            operation: "normalize opened Git materialization symlink timestamp",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

fn require_symlink_handle_matches(
    entry: &Entry,
    file: &fs::File,
    verify_target: bool,
    path: &Path,
    limits: MaterializationLimits,
    deadline: &Deadline,
) -> Result<(), Error> {
    deadline.check(path)?;
    let metadata = file.metadata().map_err(|source| Error::Io {
        operation: "inspect opened Git materialization symlink",
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_symlink() || Identity::from_metadata(&metadata) != entry.identity {
        return Err(Error::EntryChanged(path.to_owned()));
    }
    require_single_link(path, &metadata, &entry.kind)?;
    if verify_target {
        let EntryKind::Symlink { target } = &entry.kind else {
            return Err(Error::EntryChanged(path.to_owned()));
        };
        if read_symlink_handle(file, path, limits, deadline)? != *target {
            return Err(Error::EntryChanged(path.to_owned()));
        }
    }
    deadline.check(path)?;
    Ok(())
}

fn require_handle_matches(
    entry: &Entry,
    metadata: &std::fs::Metadata,
    require_normalized: bool,
    path: &Path,
) -> Result<(), Error> {
    let kind = if metadata.file_type().is_dir() {
        EntryKind::Directory
    } else if metadata.file_type().is_file() {
        EntryKind::Regular {
            executable: metadata.mode() & 0o111 != 0,
            length: metadata.len(),
        }
    } else {
        return Err(Error::EntryChanged(path.to_owned()));
    };
    require_single_link(path, metadata, &kind)?;
    if require_normalized {
        require_normalized_mode(path, metadata, &kind)?;
    }
    if Identity::from_metadata(metadata) != entry.identity || kind != entry.kind {
        return Err(Error::EntryChanged(path.to_owned()));
    }
    Ok(())
}

fn require_stamp(entry: &Entry, metadata: &std::fs::Metadata, path: &Path) -> Result<(), Error> {
    if MetadataStamp::from_metadata(metadata) == entry.stamp {
        Ok(())
    } else {
        Err(Error::EntryChanged(path.to_owned()))
    }
}

fn require_same_tree(
    root: &RootHandle,
    audited: &[Entry],
    normalized: &[Entry],
    deadline: &Deadline,
) -> Result<(), Error> {
    if audited.len() != normalized.len() {
        return Err(Error::TreeChanged);
    }
    for (expected, actual) in audited.iter().zip(normalized) {
        let path = root.display_path(&actual.relative);
        deadline.check(&path)?;
        if expected.relative != actual.relative || expected.identity != actual.identity || expected.kind != actual.kind
        {
            return Err(Error::EntryChanged(path));
        }
    }
    Ok(())
}

fn require_stable_tree(
    root: &RootHandle,
    expected: &[Entry],
    actual: &[Entry],
    deadline: &Deadline,
) -> Result<(), Error> {
    require_same_tree(root, expected, actual, deadline)?;
    for (expected, actual) in expected.iter().zip(actual) {
        let path = root.display_path(&actual.relative);
        deadline.check(&path)?;
        if expected.stamp != actual.stamp {
            return Err(Error::EntryChanged(path));
        }
    }
    Ok(())
}

fn hash_tree(
    root: &RootHandle,
    entries: &[Entry],
    limits: MaterializationLimits,
    deadline: &Deadline,
) -> Result<[u8; 32], Error> {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hash_length(&mut hasher, entries.len(), "entry count", &root.path)?;

    for entry in entries {
        let path = root.display_path(&entry.relative);
        deadline.check(&path)?;
        match &entry.kind {
            EntryKind::Directory => {
                let file = root.open_data(entry)?;
                let metadata = file.metadata().map_err(|source| Error::Io {
                    operation: "inspect Git materialization directory while hashing",
                    path: path.clone(),
                    source,
                })?;
                require_handle_matches(entry, &metadata, true, &path)?;
            }
            EntryKind::Regular { .. } => {}
            EntryKind::Symlink { .. } => {
                let file = root.open_inspection(entry)?;
                require_symlink_handle_matches(entry, &file, false, &path, limits, deadline)?;
            }
        }
        hasher.update([entry.kind.tag()]);
        hash_length(&mut hasher, entry.relative.len(), "relative path", &path)?;
        hasher.update(&entry.relative);
        hasher.update(entry.kind.normalized_mode().to_le_bytes());

        match &entry.kind {
            EntryKind::Directory => {}
            EntryKind::Regular { length, .. } => {
                hash_regular(root, entry, *length, &mut hasher, &path, deadline)?;
            }
            EntryKind::Symlink { target } => hash_symlink(target, &mut hasher, &path)?,
        }
    }

    Ok(hasher.finalize().into())
}

fn hash_length(hasher: &mut Sha256, length: usize, field: &'static str, path: &Path) -> Result<(), Error> {
    let length = u64::try_from(length).map_err(|_| Error::LengthNotRepresentable {
        field,
        path: path.to_owned(),
    })?;
    hasher.update(length.to_le_bytes());
    Ok(())
}

fn hash_regular(
    root: &RootHandle,
    entry: &Entry,
    expected_length: u64,
    hasher: &mut Sha256,
    path: &Path,
    deadline: &Deadline,
) -> Result<(), Error> {
    deadline.check(path)?;
    let mut file = root.open_data(entry)?;
    let before = file.metadata().map_err(|source| Error::Io {
        operation: "inspect Git materialization file before hashing",
        path: path.to_owned(),
        source,
    })?;
    require_handle_matches(entry, &before, true, path)?;
    if before.len() != expected_length {
        return Err(Error::FileLengthChanged {
            path: path.to_owned(),
            expected: expected_length,
            actual: before.len(),
        });
    }

    hasher.update(expected_length.to_le_bytes());
    let mut read_length = 0_u64;
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    while read_length < expected_length {
        deadline.check(path)?;
        let remaining = expected_length - read_length;
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).expect("buffer length fits usize");
        let read = match file.read(&mut buffer[..limit]) {
            Ok(0) => {
                return Err(Error::FileLengthChanged {
                    path: path.to_owned(),
                    expected: expected_length,
                    actual: read_length,
                });
            }
            Ok(read) => read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(Error::Io {
                    operation: "read Git materialization file",
                    path: path.to_owned(),
                    source,
                });
            }
        };
        hasher.update(&buffer[..read]);
        read_length += u64::try_from(read).expect("buffer read length fits u64");
    }

    let mut extra = [0_u8; 1];
    let extra_read = loop {
        deadline.check(path)?;
        match file.read(&mut extra) {
            Ok(read) => break read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                return Err(Error::Io {
                    operation: "verify Git materialization file length",
                    path: path.to_owned(),
                    source,
                });
            }
        }
    };
    let after = file.metadata().map_err(|source| Error::Io {
        operation: "inspect Git materialization file after hashing",
        path: path.to_owned(),
        source,
    })?;
    if extra_read != 0 || after.len() != expected_length {
        let observed_extra = if extra_read == 0 { 0 } else { 1 };
        return Err(Error::FileLengthChanged {
            path: path.to_owned(),
            expected: expected_length,
            actual: after.len().max(expected_length + observed_extra),
        });
    }
    if content_stamp(&before) != content_stamp(&after) {
        return Err(Error::FileChangedDuringHash(path.to_owned()));
    }
    require_handle_matches(entry, &after, true, path)?;
    deadline.check(path)?;
    Ok(())
}

fn content_stamp(metadata: &std::fs::Metadata) -> (u64, i64, i64, i64, i64) {
    (
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn hash_symlink(expected_target: &[u8], hasher: &mut Sha256, path: &Path) -> Result<(), Error> {
    hash_length(hasher, expected_target.len(), "symlink target", path)?;
    hasher.update(expected_target);
    Ok(())
}

fn verify_normalized_tree(
    root: &RootHandle,
    entries: &[Entry],
    source_date_epoch: i64,
    deadline: &Deadline,
) -> Result<(), Error> {
    for entry in entries {
        let path = root.display_path(&entry.relative);
        deadline.check(&path)?;
        if entry.stamp.atime != source_date_epoch
            || entry.stamp.atime_nsec != 0
            || entry.stamp.mtime != source_date_epoch
            || entry.stamp.mtime_nsec != 0
        {
            return Err(Error::TimestampNotNormalized {
                path,
                expected: source_date_epoch,
                atime: entry.stamp.atime,
                atime_nsec: entry.stamp.atime_nsec,
                mtime: entry.stamp.mtime,
                mtime_nsec: entry.stamp.mtime_nsec,
            });
        }
    }
    Ok(())
}

fn join_relative(parent: &[u8], name: &[u8], path: &Path) -> Result<Vec<u8>, Error> {
    let separator = usize::from(!parent.is_empty());
    let capacity = parent
        .len()
        .checked_add(separator)
        .and_then(|length| length.checked_add(name.len()))
        .ok_or_else(|| Error::ArithmeticOverflow {
            resource: "relative path bytes",
            path: path.to_owned(),
        })?;
    let mut relative = Vec::new();
    relative
        .try_reserve_exact(capacity)
        .map_err(|source| Error::Allocation {
            resource: "relative path bytes",
            requested: capacity,
            source,
        })?;
    relative.extend_from_slice(parent);
    if separator != 0 {
        relative.push(b'/');
    }
    relative.extend_from_slice(name);
    Ok(relative)
}

fn copy_bytes(bytes: &[u8], resource: &'static str) -> Result<Vec<u8>, Error> {
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(bytes.len())
        .map_err(|source| Error::Allocation {
            resource,
            requested: bytes.len(),
            source,
        })?;
    copied.extend_from_slice(bytes);
    Ok(copied)
}

fn reserve<T>(items: &mut Vec<T>, additional: usize, resource: &'static str) -> Result<(), Error> {
    items.try_reserve(additional).map_err(|source| Error::Allocation {
        resource,
        requested: additional,
        source,
    })
}

fn enforce_usize_limit(resource: &'static str, limit: usize, actual: usize, path: &Path) -> Result<(), Error> {
    if actual > limit {
        Err(Error::LimitExceeded {
            resource,
            limit: limit as u64,
            actual: actual as u64,
            path: path.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn enforce_u64_limit(resource: &'static str, limit: u64, actual: u64, path: &Path) -> Result<(), Error> {
    if actual > limit {
        Err(Error::LimitExceeded {
            resource,
            limit,
            actual,
            path: path.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn checked_add_limit(
    resource: &'static str,
    current: u64,
    additional: u64,
    limit: u64,
    path: &Path,
) -> Result<u64, Error> {
    let actual = current
        .checked_add(additional)
        .ok_or_else(|| Error::ArithmeticOverflow {
            resource,
            path: path.to_owned(),
        })?;
    enforce_u64_limit(resource, limit, actual, path)?;
    Ok(actual)
}

fn usize_to_u64(value: usize, resource: &'static str, path: &Path) -> Result<u64, Error> {
    u64::try_from(value).map_err(|_| Error::ArithmeticOverflow {
        resource,
        path: path.to_owned(),
    })
}
