/// Enumerate without retaining directory descriptors. `directories` contains
/// only indices into `entries`; each directory stream is closed before any
/// child is opened, so traversal descriptor use is constant rather than
/// proportional to attacker-controlled depth.
fn scan_tree(
    root: &RootHandle,
    require_normalized_modes: bool,
    limits: MaterializationLimits,
    deadline: &Deadline,
    expected: Option<&[Entry]>,
) -> Result<Vec<Entry>, Error> {
    let mut context = ScanContext::new(limits, deadline);
    let root_entry = root.inspect_entry(Vec::new(), &mut context, expected)?;
    let mut entries = Vec::new();
    reserve(&mut entries, 1, "materialization entries")?;
    entries.push(root_entry);
    let mut directories = Vec::new();
    reserve(&mut directories, 1, "pending materialization directories")?;
    directories.push((0_usize, 0_usize));

    while let Some((directory_index, directory_depth)) = directories.pop() {
        let names = {
            let directory = &entries[directory_index];
            read_directory_names(root, directory, directory_depth, require_normalized_modes, &mut context)?
        };
        reserve(&mut entries, names.len(), "materialization entries")?;
        reserve(&mut directories, names.len(), "pending materialization directories")?;
        for name in names {
            let directory = &entries[directory_index];
            let path = root.display_path(&directory.relative).join(OsStr::from_bytes(&name));
            let relative = join_relative(&directory.relative, &name, &path)?;
            let entry = root.inspect_entry(relative, &mut context, expected)?;
            if require_normalized_modes {
                let entry_path = root.display_path(&entry.relative);
                let metadata = root.open_inspection(&entry)?.metadata().map_err(|source| Error::Io {
                    operation: "verify Git materialization entry mode",
                    path: entry_path.clone(),
                    source,
                })?;
                require_normalized_mode(&entry_path, &metadata, &entry.kind)?;
            }
            let is_directory = entry.kind.is_directory();
            let index = entries.len();
            entries.push(entry);
            if is_directory {
                let child_depth = directory_depth.checked_add(1).ok_or(Error::ArithmeticOverflow {
                    resource: "entry depth",
                    path,
                })?;
                directories.push((index, child_depth));
            }
        }
    }

    deadline.check(&root.path)?;
    entries.sort_unstable_by(|left, right| left.relative.cmp(&right.relative));
    deadline.check(&root.path)?;
    for adjacent in entries.windows(2) {
        deadline.check(&root.display_path(&adjacent[1].relative))?;
        if adjacent[0].relative == adjacent[1].relative {
            return Err(Error::DuplicatePath(root.display_path(&adjacent[0].relative)));
        }
    }
    Ok(entries)
}

#[derive(Debug)]
struct AdministrationEntry {
    relative: Vec<u8>,
    identity: Identity,
    stamp: MetadataStamp,
    directory: bool,
    remove: bool,
}

/// Discover administration entries without following symlinks or retaining a
/// descriptor stack. Each pending directory is represented by an index into a
/// bounded vector, so even a depth-256 tree keeps only the root plus one
/// transient directory stream live.
fn collect_git_administration(
    root: &RootHandle,
    limits: MaterializationLimits,
    deadline: &Deadline,
) -> Result<Vec<AdministrationEntry>, Error> {
    let root_metadata = root.file.metadata().map_err(|source| Error::Io {
        operation: "inspect Git administration removal root",
        path: root.path.clone(),
        source,
    })?;
    let mut nodes = Vec::new();
    reserve(&mut nodes, 1, "Git administration traversal entries")?;
    nodes.push(AdministrationEntry {
        relative: Vec::new(),
        identity: Identity::from_metadata(&root_metadata),
        stamp: MetadataStamp::from_metadata(&root_metadata),
        directory: true,
        remove: false,
    });
    let mut directories = Vec::new();
    reserve(&mut directories, 1, "pending Git administration directories")?;
    directories.push((0_usize, 0_usize));
    let mut context = ScanContext::new(limits, deadline);
    let mut candidate_count = 0_usize;

    while let Some((directory_index, directory_depth)) = directories.pop() {
        let names = {
            let directory = &nodes[directory_index];
            read_administration_directory_names(root, directory, directory_depth, &mut context)?
        };
        reserve(&mut nodes, names.len(), "Git administration traversal entries")?;
        reserve(&mut directories, names.len(), "pending Git administration directories")?;
        for name in names {
            let parent = &nodes[directory_index];
            let path = root.display_path(&parent.relative).join(OsStr::from_bytes(&name));
            let relative = join_relative(&parent.relative, &name, &path)?;
            let remove = parent.remove || name == b".git";
            let node = inspect_administration_entry(root, relative, remove, &mut context)?;
            let directory = node.directory;
            let index = nodes.len();
            nodes.push(node);
            if remove {
                candidate_count = candidate_count
                    .checked_add(1)
                    .ok_or_else(|| Error::ArithmeticOverflow {
                        resource: "Git administration entries",
                        path: path.clone(),
                    })?;
            }
            if directory {
                let child_depth = directory_depth.checked_add(1).ok_or(Error::ArithmeticOverflow {
                    resource: "entry depth",
                    path,
                })?;
                directories.push((index, child_depth));
            }
        }
    }

    let mut candidates = Vec::new();
    reserve(&mut candidates, candidate_count, "Git administration removal entries")?;
    for node in nodes {
        if node.remove {
            candidates.push(node);
        }
    }
    deadline.check(&root.path)?;
    Ok(candidates)
}

fn inspect_administration_entry(
    root: &RootHandle,
    relative: Vec<u8>,
    remove: bool,
    context: &mut ScanContext<'_>,
) -> Result<AdministrationEntry, Error> {
    let path = root.display_path(&relative);
    context.check_time(&path)?;
    let file = root.open_relative(
        &relative,
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        "open Git administration traversal entry",
    )?;
    let metadata = file.metadata().map_err(|source| Error::Io {
        operation: "inspect Git administration traversal entry",
        path: path.clone(),
        source,
    })?;
    let file_type = metadata.file_type();
    let directory = file_type.is_dir();
    if file_type.is_file() {
        context.admit_kind(
            &EntryKind::Regular {
                executable: metadata.mode() & 0o111 != 0,
                length: metadata.len(),
            },
            &path,
        )?;
    } else if file_type.is_symlink() {
        let target = read_symlink_handle(&file, &path, context.limits, context.deadline)?;
        context.admit_kind(&EntryKind::Symlink { target }, &path)?;
    }
    context.check_time(&path)?;
    Ok(AdministrationEntry {
        relative,
        identity: Identity::from_metadata(&metadata),
        stamp: MetadataStamp::from_metadata(&metadata),
        directory,
        remove,
    })
}

fn read_administration_directory_names(
    root: &RootHandle,
    directory: &AdministrationEntry,
    directory_depth: usize,
    context: &mut ScanContext<'_>,
) -> Result<Vec<Vec<u8>>, Error> {
    let path = root.display_path(&directory.relative);
    context.check_time(&path)?;
    let file = root.open_relative(
        &directory.relative,
        data_open_flags(true),
        "open Git administration traversal directory",
    )?;
    require_administration_directory(directory, &file, &path)?;
    let mut names =
        DirectoryStream::from_file(file, &path)?.names(root, &directory.relative, directory_depth, context)?;
    context.check_time(&path)?;
    names.sort_unstable();
    context.check_time(&path)?;
    let reopened = root.open_relative(
        &directory.relative,
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        "reopen Git administration traversal directory",
    )?;
    require_administration_directory(directory, &reopened, &path)?;
    Ok(names)
}

fn require_administration_directory(
    directory: &AdministrationEntry,
    file: &fs::File,
    path: &Path,
) -> Result<(), Error> {
    let metadata = file.metadata().map_err(|source| Error::Io {
        operation: "verify Git administration traversal directory",
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_dir()
        || Identity::from_metadata(&metadata) != directory.identity
        || MetadataStamp::from_metadata(&metadata) != directory.stamp
    {
        Err(Error::EntryChanged(path.to_owned()))
    } else {
        Ok(())
    }
}

fn remove_admin_candidates(
    root: &RootHandle,
    candidates: &[AdministrationEntry],
    deadline: &Deadline,
) -> Result<(), Error> {
    for candidate in candidates.iter().rev() {
        let path = root.display_path(&candidate.relative);
        deadline.check(&path)?;
        let (parent_relative, name) = split_relative(&candidate.relative).ok_or_else(|| Error::InvalidPath {
            path: path.clone(),
            detail: "administration candidate has no file name",
        })?;
        let parent = root.open_relative(
            parent_relative,
            data_open_flags(true),
            "open Git administration entry parent",
        )?;
        let current = openat2_file(
            parent.as_raw_fd(),
            name,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV,
        )
        .map_err(|source| Error::Io {
            operation: "reopen Git administration entry before removal",
            path: path.clone(),
            source,
        })?;
        let metadata = current.metadata().map_err(|source| Error::Io {
            operation: "inspect Git administration entry before removal",
            path: path.clone(),
            source,
        })?;
        if Identity::from_metadata(&metadata) != candidate.identity
            || metadata.file_type().is_dir() != candidate.directory
        {
            return Err(Error::EntryChanged(path));
        }
        let name = CString::new(name).map_err(|_| Error::InvalidPath {
            path: path.clone(),
            detail: "administration entry name contains NUL",
        })?;
        let flags = if candidate.directory { libc::AT_REMOVEDIR } else { 0 };
        // SAFETY: the parent descriptor and NUL-terminated single-component
        // name are live. `unlinkat` never follows a symlink in the final
        // component; directories require the explicit AT_REMOVEDIR flag.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) } == -1 {
            return Err(Error::Io {
                operation: "remove Git administration entry",
                path,
                source: io::Error::last_os_error(),
            });
        }
        deadline.check(&root.path)?;
    }
    Ok(())
}

fn split_relative(relative: &[u8]) -> Option<(&[u8], &[u8])> {
    if relative.is_empty() {
        return None;
    }
    match relative.iter().rposition(|byte| *byte == b'/') {
        Some(separator) => Some((&relative[..separator], &relative[separator + 1..])),
        None => Some((b"", relative)),
    }
}

struct DirectoryStream(std::ptr::NonNull<libc::DIR>);

impl DirectoryStream {
    fn from_file(file: fs::File, path: &Path) -> Result<Self, Error> {
        let descriptor = file.into_raw_fd();
        // SAFETY: `descriptor` is an owned directory descriptor. On success
        // fdopendir takes ownership; on failure we close it below.
        let stream = unsafe { libc::fdopendir(descriptor) };
        match std::ptr::NonNull::new(stream) {
            Some(stream) => Ok(Self(stream)),
            None => {
                let source = io::Error::last_os_error();
                // SAFETY: fdopendir failed and therefore did not consume the
                // descriptor.
                unsafe { libc::close(descriptor) };
                Err(Error::Io {
                    operation: "open Git materialization directory stream",
                    path: path.to_owned(),
                    source,
                })
            }
        }
    }

    fn names(
        &mut self,
        root: &RootHandle,
        directory_relative: &[u8],
        directory_depth: usize,
        context: &mut ScanContext<'_>,
    ) -> Result<Vec<Vec<u8>>, Error> {
        let directory_path = root.display_path(directory_relative);
        let mut names = Vec::new();
        loop {
            context.check_time(&directory_path)?;
            Errno::clear();
            // SAFETY: the DIR pointer is live and exclusively borrowed for
            // this iteration.
            let entry = unsafe { libc::readdir64(self.0.as_ptr()) };
            if entry.is_null() {
                let error = Errno::last();
                if error == Errno::UnknownErrno {
                    return Ok(names);
                }
                return Err(Error::Io {
                    operation: "read Git materialization directory",
                    path: directory_path,
                    source: io::Error::from_raw_os_error(error as i32),
                });
            }
            // SAFETY: readdir64 returned a live dirent whose d_name is
            // NUL-terminated for the duration of this loop iteration.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                let separator = usize::from(!directory_relative.is_empty());
                let relative_length = directory_relative
                    .len()
                    .checked_add(separator)
                    .and_then(|length| length.checked_add(name.len()))
                    .ok_or_else(|| Error::ArithmeticOverflow {
                        resource: "relative path bytes",
                        path: directory_path.clone(),
                    })?;
                let depth = directory_depth
                    .checked_add(1)
                    .ok_or_else(|| Error::ArithmeticOverflow {
                        resource: "entry depth",
                        path: directory_path.clone(),
                    })?;
                let path = directory_path.join(OsStr::from_bytes(name));
                context.admit_entry_bytes(name.len(), relative_length, depth, &path)?;
                reserve(&mut names, 1, "materialization directory names")?;
                names.push(copy_bytes(name, "materialization entry name")?);
            }
        }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the stream returned by fdopendir.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}

fn read_directory_names(
    root: &RootHandle,
    directory: &Entry,
    directory_depth: usize,
    require_normalized_modes: bool,
    context: &mut ScanContext<'_>,
) -> Result<Vec<Vec<u8>>, Error> {
    let path = root.display_path(&directory.relative);
    context.check_time(&path)?;
    let file = root.open_data(directory)?;
    let metadata = file.metadata().map_err(|source| Error::Io {
        operation: "inspect opened Git materialization directory",
        path: path.clone(),
        source,
    })?;
    require_handle_matches(directory, &metadata, require_normalized_modes, &path)?;
    require_stamp(directory, &metadata, &path)?;
    let mut names =
        DirectoryStream::from_file(file, &path)?.names(root, &directory.relative, directory_depth, context)?;
    context.check_time(&path)?;
    names.sort_unstable();
    context.check_time(&path)?;

    // Reopen through the pinned root after enumeration. The stamp comparison
    // catches additions/removals in the same directory inode, while identity
    // comparison rejects a path replacement.
    let reopened = root.open_inspection(directory)?;
    let metadata = reopened.metadata().map_err(|source| Error::Io {
        operation: "verify enumerated Git materialization directory",
        path: path.clone(),
        source,
    })?;
    require_handle_matches(directory, &metadata, require_normalized_modes, &path)?;
    require_stamp(directory, &metadata, &path)?;
    Ok(names)
}

fn classify_handle(
    path: &Path,
    file: &fs::File,
    metadata: &std::fs::Metadata,
    relative: &[u8],
    identity: Identity,
    expected: Option<&[Entry]>,
    context: &ScanContext<'_>,
) -> Result<EntryKind, Error> {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        Ok(EntryKind::Directory)
    } else if file_type.is_file() {
        Ok(EntryKind::Regular {
            executable: metadata.mode() & 0o111 != 0,
            length: metadata.len(),
        })
    } else if file_type.is_symlink() {
        let reused = expected
            .and_then(|entries| {
                entries
                    .binary_search_by(|entry| entry.relative.as_slice().cmp(relative))
                    .ok()
                    .map(|index| &entries[index])
            })
            .and_then(|entry| (entry.identity == identity).then_some(&entry.kind))
            .and_then(|kind| match kind {
                EntryKind::Symlink { target } => Some(target.as_slice()),
                _ => None,
            });
        let target = match reused {
            Some(target) => copy_bytes(target, "symlink target bytes")?,
            None => read_symlink_handle(file, path, context.limits, context.deadline)?,
        };
        Ok(EntryKind::Symlink { target })
    } else {
        Err(Error::UnsupportedFileType {
            path: path.to_owned(),
            kind: special_file_type(&file_type),
        })
    }
}
fn read_symlink_handle(
    file: &fs::File,
    path: &Path,
    limits: MaterializationLimits,
    deadline: &Deadline,
) -> Result<Vec<u8>, Error> {
    deadline.check(path)?;
    let capacity = limits
        .max_symlink_target_bytes
        .checked_add(1)
        .ok_or_else(|| Error::ArithmeticOverflow {
            resource: "symlink target bytes",
            path: path.to_owned(),
        })?;
    let mut target = Vec::new();
    target.try_reserve_exact(capacity).map_err(|source| Error::Allocation {
        resource: "symlink target bytes",
        requested: capacity,
        source,
    })?;
    target.resize(capacity, 0);
    // Linux readlinkat with an empty path reads the exact symlink pinned by
    // the O_PATH|O_NOFOLLOW descriptor. The extra byte distinguishes an exact
    // limit-sized target from a truncated over-limit target.
    // SAFETY: the descriptor is live and `target` exposes `capacity` writable
    // bytes for the duration of the syscall.
    let read = unsafe { libc::readlinkat(file.as_raw_fd(), c"".as_ptr(), target.as_mut_ptr().cast(), target.len()) };
    if read == -1 {
        return Err(Error::Io {
            operation: "read opened Git materialization symlink",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let read = usize::try_from(read).map_err(|_| Error::ArithmeticOverflow {
        resource: "symlink target bytes",
        path: path.to_owned(),
    })?;
    enforce_usize_limit("symlink target bytes", limits.max_symlink_target_bytes, read, path)?;
    target.truncate(read);
    deadline.check(path)?;
    Ok(target)
}

fn special_file_type(file_type: &std::fs::FileType) -> &'static str {
    if file_type.is_fifo() {
        "FIFO"
    } else if file_type.is_socket() {
        "socket"
    } else if file_type.is_block_device() {
        "block device"
    } else if file_type.is_char_device() {
        "character device"
    } else {
        "unknown special inode"
    }
}

fn require_single_link(path: &Path, metadata: &std::fs::Metadata, kind: &EntryKind) -> Result<(), Error> {
    if !kind.is_directory() && metadata.nlink() != 1 {
        Err(Error::UnexpectedLinkCount {
            path: path.to_owned(),
            links: metadata.nlink(),
        })
    } else {
        Ok(())
    }
}

fn require_normalized_mode(path: &Path, metadata: &std::fs::Metadata, kind: &EntryKind) -> Result<(), Error> {
    if matches!(kind, EntryKind::Symlink { .. }) {
        return Ok(());
    }
    let expected = kind.normalized_mode();
    let actual = metadata.mode() & 0o7777;
    if actual == expected {
        Ok(())
    } else {
        Err(Error::ModeNotNormalized {
            path: path.to_owned(),
            expected,
            actual,
        })
    }
}
