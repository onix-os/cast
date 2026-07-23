#[derive(Debug)]
struct GluonPath {
    logical_name: String,
    source_root_path: PathBuf,
    source_root: SourceRoot,
    relative_path: PathBuf,
    path: PathBuf,
    collection: Option<CollectionIdentity>,
}

#[derive(Debug, Clone)]
struct CollectionIdentity {
    path: PathBuf,
    identity: FileSnapshot,
}

fn collect_gluon_paths(scope: &super::Scope, domain: &str) -> Result<Vec<GluonPath>, LoadGluonError> {
    let mut paths = Vec::new();
    for (entry, resolve) in scope.load_with() {
        let remaining = MAX_GLUON_FRAGMENTS.saturating_sub(paths.len());
        let layer = enumerate_gluon_paths(entry, resolve, domain, remaining)?;
        if paths.len().saturating_add(layer.len()) > MAX_GLUON_FRAGMENTS {
            return Err(LoadGluonError::FragmentLimit {
                limit: MAX_GLUON_FRAGMENTS,
            });
        }
        paths.extend(layer);
    }
    Ok(paths)
}

fn enumerate_gluon_paths(
    entry: Entry,
    resolve: Resolve<'_>,
    domain: &str,
    remaining: usize,
) -> Result<Vec<GluonPath>, LoadGluonError> {
    let source_root_path = resolve.config_dir();
    let source_root = match fs::symlink_metadata(&source_root_path) {
        Ok(_) => SourceRoot::new(&source_root_path).map_err(|source| LoadGluonError::Evaluation {
            path: source_root_path.clone(),
            source,
        })?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(LoadGluonError::Enumerate {
                path: source_root_path,
                source,
            });
        }
    };
    match entry {
        Entry::File => {
            let relative_path = PathBuf::from(format!("{domain}.glu"));
            let path = resolve.file(domain, "glu");
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(source) => return Err(LoadGluonError::Enumerate { path, source }),
            };
            require_regular_fragment(&path, &metadata)?;
            if remaining == 0 {
                return Err(LoadGluonError::FragmentLimit {
                    limit: MAX_GLUON_FRAGMENTS,
                });
            }
            verify_source_root(&source_root_path, &source_root)?;
            Ok(vec![GluonPath {
                logical_name: domain.to_owned(),
                source_root_path,
                source_root,
                relative_path,
                path,
                collection: None,
            }])
        }
        Entry::Directory => {
            let relative_dir = PathBuf::from(format!("{domain}.d"));
            let dir = source_root_path.join(&relative_dir);
            let metadata = match fs::symlink_metadata(&dir) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(source) => return Err(LoadGluonError::Enumerate { path: dir, source }),
            };
            if !metadata.file_type().is_dir() {
                return Err(invalid_entry(&dir, "Gluon fragment collection is not a real directory"));
            }
            let directory = FragmentDirectory::open(&dir).map_err(|source| LoadGluonError::Enumerate {
                path: dir.clone(),
                source,
            })?;
            let opened_metadata = directory.file.metadata().map_err(|source| LoadGluonError::Enumerate {
                path: dir.clone(),
                source,
            })?;
            let collection = CollectionIdentity {
                path: dir.clone(),
                identity: FileSnapshot::from_metadata(&metadata),
            };
            if !opened_metadata.file_type().is_dir()
                || FileSnapshot::from_metadata(&opened_metadata) != collection.identity
            {
                return Err(invalid_entry(
                    &dir,
                    "Gluon fragment collection changed while its descriptor was being opened",
                ));
            }
            let entries = match directory.entry_names(MAX_GLUON_DIRECTORY_ENTRIES).map_err(|source| {
                LoadGluonError::Enumerate {
                    path: dir.clone(),
                    source,
                }
            })? {
                BoundedDirectoryEntries::Complete(entries) => entries,
                BoundedDirectoryEntries::LimitExceeded => {
                    return Err(LoadGluonError::DirectoryEntryLimit {
                        path: dir,
                        limit: MAX_GLUON_DIRECTORY_ENTRIES,
                    });
                }
            };
            let mut paths = Vec::new();
            for name in entries {
                let path = dir.join(&name);
                if Path::new(&name).extension() != Some(OsStr::new("glu")) {
                    continue;
                }
                let entry_metadata = directory
                    .metadata_at(&name)
                    .map_err(|source| LoadGluonError::Enumerate {
                        path: path.clone(),
                        source,
                    })?;
                if !entry_metadata.file_type().is_file() {
                    return Err(invalid_entry(
                        &path,
                        "matching Gluon fragment is not a real regular file",
                    ));
                }
                let logical_name = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| LoadGluonError::Enumerate {
                        path: dir.clone(),
                        source: io::Error::new(io::ErrorKind::InvalidData, "Gluon fragment name is not UTF-8"),
                    })?
                    .to_owned();
                if !is_safe_fragment_name(&logical_name) {
                    return Err(invalid_entry(
                        &path,
                        "Gluon fragment name is not a safe normalized component",
                    ));
                }
                if paths.len() == remaining {
                    return Err(LoadGluonError::FragmentLimit {
                        limit: MAX_GLUON_FRAGMENTS,
                    });
                }
                let relative_path = relative_dir.join(&name);
                paths.push(GluonPath {
                    logical_name,
                    source_root_path: source_root_path.clone(),
                    source_root: source_root.clone(),
                    relative_path,
                    path,
                    collection: Some(collection.clone()),
                });
            }
            paths.sort_by(|left, right| left.logical_name.cmp(&right.logical_name));
            verify_collection(Some(&collection))?;
            verify_source_root(&source_root_path, &source_root)?;
            Ok(paths)
        }
    }
}

fn verify_source_root(path: &Path, expected: &SourceRoot) -> Result<(), LoadGluonError> {
    let current = SourceRoot::new(path).map_err(|source| LoadGluonError::Evaluation {
        path: path.to_owned(),
        source,
    })?;
    if &current == expected {
        Ok(())
    } else {
        Err(LoadGluonError::Enumerate {
            path: path.to_owned(),
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Gluon source root changed while fragments were being loaded",
            ),
        })
    }
}

fn verify_collection(collection: Option<&CollectionIdentity>) -> Result<(), LoadGluonError> {
    let Some(collection) = collection else {
        return Ok(());
    };
    let metadata = fs::symlink_metadata(&collection.path).map_err(|source| LoadGluonError::Enumerate {
        path: collection.path.clone(),
        source,
    })?;
    if metadata.file_type().is_dir() && FileSnapshot::from_metadata(&metadata) == collection.identity {
        Ok(())
    } else {
        Err(LoadGluonError::Enumerate {
            path: collection.path.clone(),
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Gluon fragment collection changed while fragments were being loaded",
            ),
        })
    }
}

fn require_regular_fragment(path: &Path, metadata: &Metadata) -> Result<(), LoadGluonError> {
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(invalid_entry(
            path,
            "matching Gluon fragment is not a real regular file",
        ))
    }
}

fn invalid_entry(path: &Path, message: &'static str) -> LoadGluonError {
    LoadGluonError::Enumerate {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidData, message),
    }
}

pub(super) fn is_safe_fragment_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > MAX_GLUON_FRAGMENT_NAME_BYTES
        || name.contains('\\')
        || name.chars().any(char::is_control)
    {
        return false;
    }
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(component)), None) if component == OsStr::new(name)
    )
}

#[derive(Debug)]
struct FragmentDirectory {
    path: PathBuf,
    file: fs::File,
    identity: NodeIdentity,
}

impl FragmentDirectory {
    fn open(path: &Path) -> io::Result<Self> {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Gluon config path is not a real directory",
            ));
        }
        Ok(Self {
            path: path.to_owned(),
            file,
            identity: NodeIdentity::from_metadata(&metadata),
        })
    }

    fn verify_path(&self) -> io::Result<()> {
        let metadata = fs::symlink_metadata(&self.path)?;
        if metadata.file_type().is_dir() && NodeIdentity::from_metadata(&metadata) == self.identity {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Gluon config directory changed while it was being managed",
            ))
        }
    }

    fn entry_names(&self, limit: usize) -> io::Result<BoundedDirectoryEntries> {
        // fdopendir owns and closes its descriptor. Open `.` relative to the
        // held directory instead of duplicating the descriptor: dup would
        // share a directory-stream offset, making a second enumeration start
        // at the previous end position.
        // SAFETY: the base descriptor is valid and the C string is static.
        let descriptor = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                c".".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                0,
            )
        };
        if descriptor == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: descriptor is a fresh duplicate referring to a directory.
        let stream = unsafe { libc::fdopendir(descriptor) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: fdopendir failed and therefore did not consume the
            // duplicated descriptor.
            unsafe {
                libc::close(descriptor);
            }
            return Err(error);
        }
        let stream = DirectoryStream(stream);
        let mut names = Vec::new();
        loop {
            // POSIX distinguishes end-of-directory from failure through
            // errno. This stream is private to this call.
            // SAFETY: Linux exposes thread-local errno through this pointer.
            unsafe {
                *libc::__errno_location() = 0;
            }
            // SAFETY: stream remains live and exclusively used until the end
            // of this loop iteration.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                // SAFETY: errno is thread-local and was cleared immediately
                // before readdir.
                let errno = unsafe { *libc::__errno_location() };
                if errno == 0 {
                    break;
                }
                return Err(io::Error::from_raw_os_error(errno));
            }
            // SAFETY: readdir returned a dirent whose d_name is NUL-terminated
            // and remains valid until the next operation on this stream.
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            if names.len() == limit {
                return Ok(BoundedDirectoryEntries::LimitExceeded);
            }
            names.push(OsString::from_vec(bytes.to_vec()));
        }
        Ok(BoundedDirectoryEntries::Complete(names))
    }

    fn metadata_at(&self, name: &OsStr) -> io::Result<Metadata> {
        self.open_at(name, libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW, 0)?
            .metadata()
    }

    fn open_at(&self, name: &OsStr, flags: i32, mode: libc::mode_t) -> io::Result<fs::File> {
        let diagnostic_path = self.display_path(name);
        let name = c_name(name)?;
        // SAFETY: name is NUL-terminated, the directory descriptor remains
        // owned by self, and a successful openat returns a fresh descriptor.
        let descriptor = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), flags, mode) };
        if descriptor == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh owned descriptor.
        let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
        Ok(fs::File::from_parts(descriptor.into(), diagnostic_path))
    }

    fn rename(&self, from: &OsStr, to: &OsStr, no_replace: bool) -> io::Result<()> {
        let from = c_name(from)?;
        let to = c_name(to)?;
        let result = if no_replace {
            // Cast targets Linux (glibc and musl). Invoke renameat2 directly
            // so a concurrently-created authored target is never replaced.
            // SAFETY: both names are NUL-terminated and both directory FDs
            // remain valid for the duration of the syscall.
            unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    self.file.as_raw_fd(),
                    from.as_ptr(),
                    self.file.as_raw_fd(),
                    to.as_ptr(),
                    1_u32, // RENAME_NOREPLACE
                )
            }
        } else {
            // SAFETY: both names are NUL-terminated and both directory FDs
            // remain valid for the duration of the call.
            unsafe {
                libc::c_long::from(libc::renameat(
                    self.file.as_raw_fd(),
                    from.as_ptr(),
                    self.file.as_raw_fd(),
                    to.as_ptr(),
                ))
            }
        };
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn unlink(&self, name: &OsStr) -> io::Result<()> {
        let name = c_name(name)?;
        // SAFETY: name is NUL-terminated and the directory descriptor remains
        // valid for the duration of unlinkat.
        let result = unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) };
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    fn display_path(&self, name: &OsStr) -> PathBuf {
        self.path.join(name)
    }
}

enum BoundedDirectoryEntries {
    Complete(Vec<OsString>),
    LimitExceeded,
}

struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: fdopendir returned this stream and it is closed exactly once
        // here. closedir also closes the duplicate descriptor it owns.
        unsafe {
            libc::closedir(self.0);
        }
    }
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "fragment name contains a NUL byte"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NodeIdentity {
    device: u64,
    inode: u64,
}

impl NodeIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

/// Identity plus the kernel-maintained generation data available to an
/// unprivileged writer. Unlike modification time, ctime cannot be restored by
/// `utimensat`, so it detects in-place edits as well as inode replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileSnapshot {
    node: NodeIdentity,
    size: u64,
    ctime: i64,
    ctime_nsec: i64,
}

impl FileSnapshot {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            node: NodeIdentity::from_metadata(metadata),
            size: metadata.size(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ExistingFragment {
    identity: FileSnapshot,
    generated: bool,
}

fn inspect_existing_fragment(
    directory: &FragmentDirectory,
    name: &OsStr,
    limit: usize,
) -> io::Result<Option<ExistingFragment>> {
    let mut file = match directory.open_at(
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
        0,
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "managed Gluon fragment is not a real regular file",
        ));
    }
    if metadata.len() > u64::try_from(limit).unwrap_or(u64::MAX) {
        return Err(fragment_too_large(metadata.len(), limit));
    }

    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(limit).min(limit));
    io::Read::by_ref(&mut file)
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(fragment_too_large(
            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            limit,
        ));
    }
    let final_metadata = file.metadata()?;
    let initial = FileSnapshot::from_metadata(&metadata);
    let final_snapshot = FileSnapshot::from_metadata(&final_metadata);
    if initial != final_snapshot {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed Gluon fragment changed while its marker was being read",
        ));
    }
    Ok(Some(ExistingFragment {
        identity: final_snapshot,
        generated: bytes.starts_with(GENERATED_GLUON_MARKER.as_bytes()),
    }))
}
