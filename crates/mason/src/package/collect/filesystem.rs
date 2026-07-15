use std::{
    ffi::{CStr, CString, OsStr, OsString},
    fs::{File, Metadata},
    io::{self, Read},
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::FileTypeExt,
        },
    },
    path::{Component, Path, PathBuf},
    ptr::NonNull,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use nix::libc;
use stone::StoneDigestWriterHasher;

use super::{
    AdmissionDelta, AdmissionDraft, CollectionLimits, DirectoryHandle, DirectoryId, DirectoryWitness, EntryWitness,
    Error, FileSnapshot, HASH_BUFFER_BYTES, WitnessChild, WitnessChildKind, WitnessEntryKind, WitnessPhase,
    WitnessState,
};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct CollectionUsage {
    pub(super) entries: u64,
    name_bytes: u64,
    path_bytes: u64,
    symlink_target_bytes: u64,
    pub(super) regular_bytes: u64,
}

pub(super) struct CollectionContext {
    limits: CollectionLimits,
    usage: Option<Arc<Mutex<CollectionUsage>>>,
    pub(super) deadline: Arc<Deadline>,
}

impl CollectionContext {
    pub(super) fn new(limits: CollectionLimits, usage: Arc<Mutex<CollectionUsage>>, deadline: Arc<Deadline>) -> Self {
        Self {
            limits,
            usage: Some(usage),
            deadline,
        }
    }

    pub(super) fn detached(limits: CollectionLimits, deadline: Arc<Deadline>) -> Self {
        Self {
            limits,
            usage: None,
            deadline,
        }
    }

    pub(super) fn check_time(&self, path: &Path) -> Result<(), Error> {
        self.deadline.check(path)
    }

    pub(super) fn check_depth(&self, depth: usize, path: &Path) -> Result<(), Error> {
        enforce_usize_limit("path depth", self.limits.max_depth, depth, path)
    }

    pub(super) fn admit_entry(&self, relative: &Path, depth: usize, display_path: &Path) -> Result<(), Error> {
        self.check_time(display_path)?;
        self.check_depth(depth, display_path)?;
        let path_bytes = relative.as_os_str().as_bytes().len();
        let name_bytes = relative
            .file_name()
            .map(|name| name.as_bytes().len())
            .unwrap_or_default();
        enforce_usize_limit("entry name bytes", self.limits.max_name_bytes, name_bytes, display_path)?;
        enforce_usize_limit("entry path bytes", self.limits.max_path_bytes, path_bytes, display_path)?;
        let Some(usage) = &self.usage else {
            return Ok(());
        };
        let mut usage = usage.lock().map_err(|_| Error::StatePoisoned)?;
        update_entry_usage(&mut usage, self.limits, name_bytes, path_bytes, display_path)?;
        Ok(())
    }

    pub(super) fn admit_regular(&self, bytes: u64, path: &Path) -> Result<(), Error> {
        self.check_time(path)?;
        enforce_u64_limit("regular file bytes", self.limits.max_file_bytes, bytes, path)?;
        let Some(usage) = &self.usage else {
            return Ok(());
        };
        let mut usage = usage.lock().map_err(|_| Error::StatePoisoned)?;
        usage.regular_bytes = checked_add_limit(
            "total regular file bytes",
            usage.regular_bytes,
            bytes,
            self.limits.max_total_regular_bytes,
            path,
        )?;
        Ok(())
    }

    pub(super) fn admit_symlink_target(&self, bytes: usize, path: &Path) -> Result<(), Error> {
        self.check_time(path)?;
        enforce_usize_limit(
            "symlink target bytes",
            self.limits.max_symlink_target_bytes,
            bytes,
            path,
        )?;
        let Some(usage) = &self.usage else {
            return Ok(());
        };
        let mut usage = usage.lock().map_err(|_| Error::StatePoisoned)?;
        usage.symlink_target_bytes = checked_add_limit(
            "total symlink target bytes",
            usage.symlink_target_bytes,
            bytes as u64,
            self.limits.max_total_symlink_target_bytes,
            path,
        )?;
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct Deadline {
    started: Instant,
    limit: Duration,
}

impl Deadline {
    pub(super) fn new(limit: Duration) -> Self {
        Self {
            started: Instant::now(),
            limit,
        }
    }

    pub(super) fn check(&self, path: &Path) -> Result<(), Error> {
        if self.started.elapsed() >= self.limit {
            Err(Error::DurationExceeded {
                path: path.to_owned(),
                limit: self.limit,
            })
        } else {
            Ok(())
        }
    }

    pub(super) fn remaining(&self, path: &Path) -> Result<Duration, Error> {
        let elapsed = self.started.elapsed();
        if elapsed >= self.limit {
            Err(Error::DurationExceeded {
                path: path.to_owned(),
                limit: self.limit,
            })
        } else {
            Ok(self.limit - elapsed)
        }
    }
}

pub(super) fn hash_inventory_regular(
    context: &CollectionContext,
    parent: &DirectoryHandle,
    name: &OsStr,
    expected: FileSnapshot,
    display_path: &Path,
    hasher: &mut StoneDigestWriterHasher,
) -> Result<u128, Error> {
    let mut file = open_entry(
        &parent.file,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
        display_path,
    )?;
    let opened = metadata(&file, "stat inventory package file", display_path)?;
    if !opened.file_type().is_file() {
        return Err(changed(
            display_path,
            "inventory entry stopped being a regular file before hashing",
        ));
    }
    require_snapshot(display_path, expected, &opened)?;
    hasher.reset();
    let mut buffer = [0u8; HASH_BUFFER_BYTES];
    let mut bytes = 0u64;
    loop {
        context.check_time(display_path)?;
        let read = file.read(&mut buffer).map_err(|source| Error::Io {
            operation: "hash inventory package file",
            path: display_path.to_owned(),
            source,
        })?;
        if read == 0 {
            break;
        }
        bytes = bytes.checked_add(read as u64).ok_or(Error::ArithmeticOverflow {
            resource: "regular file bytes",
            path: display_path.to_owned(),
        })?;
        if bytes > expected.size {
            return Err(changed(display_path, "inventory regular file grew while hashing"));
        }
        hasher.update(&buffer[..read]);
    }
    if bytes != expected.size {
        return Err(changed(
            display_path,
            "inventory regular file changed size while hashing",
        ));
    }
    require_snapshot(
        display_path,
        expected,
        &metadata(&file, "restat inventory package file", display_path)?,
    )?;
    verify_entry_collection(parent, name, expected, display_path)?;
    Ok(hasher.digest128())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn capture_entry_witness(
    context: &CollectionContext,
    parent: &DirectoryHandle,
    name: &OsStr,
    handle: &File,
    entry_metadata: &Metadata,
    snapshot: FileSnapshot,
    path: &Path,
    hasher: &mut StoneDigestWriterHasher,
) -> Result<EntryWitness, Error> {
    let file_type = entry_metadata.file_type();
    let kind = if file_type.is_symlink() {
        let target = read_symlink_handle(handle, path, context)?;
        verify_entry_collection(parent, name, snapshot, path)?;
        WitnessEntryKind::Symlink { target }
    } else if file_type.is_file() {
        context.admit_regular(snapshot.size, path)?;
        WitnessEntryKind::Regular {
            hash: hash_inventory_regular(context, parent, name, snapshot, path, hasher)?,
        }
    } else if is_supported_special(&file_type) {
        verify_entry_collection(parent, name, snapshot, path)?;
        WitnessEntryKind::Special
    } else {
        return Err(Error::UnsupportedFileType {
            path: path.to_owned(),
            kind: "unknown special inode",
        });
    };
    Ok(EntryWitness { snapshot, kind })
}

pub(super) fn read_directory_names(
    directory: &DirectoryHandle,
    context: &CollectionContext,
) -> Result<Vec<OsString>, Error> {
    verify_directory_collection(directory)?;
    let cursor = open_entry(
        &directory.file,
        OsStr::new("."),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        &directory.display_path,
    )?;
    let descriptor = cursor.into_raw_fd();
    // SAFETY: descriptor is a fresh owned directory descriptor. fdopendir
    // consumes it on success; on failure it remains ours and is closed below.
    let stream = unsafe { libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume descriptor.
        unsafe { libc::close(descriptor) };
        return Err(Error::Io {
            operation: "open package directory stream",
            path: directory.display_path.clone(),
            source,
        });
    };
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        context.check_time(&directory.display_path)?;
        // SAFETY: Linux exposes thread-local errno through this pointer.
        unsafe { *libc::__errno_location() = 0 };
        // SAFETY: stream is live and exclusively used by this loop.
        let entry = unsafe { libc::readdir(stream.0.as_ptr()) };
        if entry.is_null() {
            // SAFETY: errno was cleared immediately before readdir.
            let errno = unsafe { *libc::__errno_location() };
            if errno == 0 {
                break;
            }
            return Err(Error::Io {
                operation: "enumerate package directory",
                path: directory.display_path.clone(),
                source: io::Error::from_raw_os_error(errno),
            });
        }
        // SAFETY: readdir returned a NUL-terminated name valid until the next
        // operation on this stream.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let relative_len = directory
            .relative
            .as_os_str()
            .as_bytes()
            .len()
            .checked_add(usize::from(!directory.relative.as_os_str().is_empty()))
            .and_then(|length| length.checked_add(bytes.len()))
            .ok_or(Error::ArithmeticOverflow {
                resource: "entry path bytes",
                path: directory.display_path.clone(),
            })?;
        let display_path = directory.display_path.join(OsStr::from_bytes(bytes));
        let depth = directory
            .relative
            .components()
            .count()
            .checked_add(1)
            .ok_or(Error::ArithmeticOverflow {
                resource: "path depth",
                path: display_path.clone(),
            })?;
        context.admit_entry_bytes(bytes.len(), relative_len, depth, &display_path)?;
        reserve(&mut names, 1, "directory entry names")?;
        names.push(copy_os_string(bytes, &display_path)?);
    }
    names.sort_unstable();
    context.check_time(&directory.display_path)?;
    verify_directory_collection(directory)?;
    Ok(names)
}

impl CollectionContext {
    fn admit_entry_bytes(
        &self,
        name_bytes: usize,
        path_bytes: usize,
        depth: usize,
        display_path: &Path,
    ) -> Result<(), Error> {
        self.check_time(display_path)?;
        self.check_depth(depth, display_path)?;
        enforce_usize_limit("entry name bytes", self.limits.max_name_bytes, name_bytes, display_path)?;
        enforce_usize_limit("entry path bytes", self.limits.max_path_bytes, path_bytes, display_path)?;
        let Some(usage) = &self.usage else {
            return Ok(());
        };
        let mut usage = usage.lock().map_err(|_| Error::StatePoisoned)?;
        update_entry_usage(&mut usage, self.limits, name_bytes, path_bytes, display_path)?;
        Ok(())
    }
}

fn update_entry_usage(
    usage: &mut CollectionUsage,
    limits: CollectionLimits,
    name_bytes: usize,
    path_bytes: usize,
    display_path: &Path,
) -> Result<(), Error> {
    let name_bytes = u64::try_from(name_bytes).map_err(|_| Error::ArithmeticOverflow {
        resource: "total entry name bytes",
        path: display_path.to_owned(),
    })?;
    let path_bytes = u64::try_from(path_bytes).map_err(|_| Error::ArithmeticOverflow {
        resource: "total entry path bytes",
        path: display_path.to_owned(),
    })?;
    let entries = checked_add_limit("total entries", usage.entries, 1, limits.max_entries, display_path)?;
    let names = checked_add_limit(
        "total entry name bytes",
        usage.name_bytes,
        name_bytes,
        limits.max_total_name_bytes,
        display_path,
    )?;
    let paths = checked_add_limit(
        "total entry path bytes",
        usage.path_bytes,
        path_bytes,
        limits.max_total_path_bytes,
        display_path,
    )?;
    usage.entries = entries;
    usage.name_bytes = names;
    usage.path_bytes = paths;
    Ok(())
}

struct DirectoryStream(NonNull<libc::DIR>);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the stream returned by fdopendir.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}

pub(super) fn verify_directory_collection(directory: &DirectoryHandle) -> Result<(), Error> {
    require_snapshot(
        &directory.display_path,
        directory.snapshot,
        &metadata(
            &directory.file,
            "verify package directory descriptor",
            &directory.display_path,
        )?,
    )?;
    let reopened = directory.anchor.open_directory(&directory.relative)?;
    require_snapshot(
        &directory.display_path,
        directory.snapshot,
        &metadata(&reopened, "verify package directory path", &directory.display_path)?,
    )
}

pub(super) fn verify_entry_collection(
    parent: &DirectoryHandle,
    name: &OsStr,
    expected: FileSnapshot,
    path: &Path,
) -> Result<(), Error> {
    verify_directory_collection(parent)?;
    let reopened = open_entry_handle(&parent.file, name, path)?;
    require_snapshot(path, expected, &metadata(&reopened, "verify package entry path", path)?)
}

pub(super) fn read_symlink_handle(handle: &File, path: &Path, context: &CollectionContext) -> Result<String, Error> {
    let capacity = context
        .limits
        .max_symlink_target_bytes
        .checked_add(1)
        .ok_or(Error::ArithmeticOverflow {
            resource: "symlink target bytes",
            path: path.to_owned(),
        })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity).map_err(|source| Error::Allocation {
        resource: "symlink target bytes",
        requested: capacity,
        detail: source.to_string(),
    })?;
    bytes.resize(capacity, 0);
    // Linux readlinkat with an empty path reads the symlink pinned by an
    // O_PATH|O_NOFOLLOW descriptor, rather than a replaceable pathname.
    // SAFETY: the descriptor is live and bytes is writable for capacity bytes.
    let read = unsafe { libc::readlinkat(handle.as_raw_fd(), c"".as_ptr(), bytes.as_mut_ptr().cast(), bytes.len()) };
    if read == -1 {
        return Err(Error::Io {
            operation: "read package symlink target",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let read = usize::try_from(read).map_err(|_| Error::ArithmeticOverflow {
        resource: "symlink target bytes",
        path: path.to_owned(),
    })?;
    context.admit_symlink_target(read, path)?;
    bytes.truncate(read);
    String::from_utf8(bytes).map_err(|_| Error::NonUtf8SymlinkTarget { path: path.to_owned() })
}

pub(super) fn open_entry_handle(parent: &File, name: &OsStr, path: &Path) -> Result<File, Error> {
    open_entry(
        parent,
        name,
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        path,
    )
}

pub(super) fn open_entry(parent: &File, name: &OsStr, flags: i32, path: &Path) -> Result<File, Error> {
    let name = c_name(name, path)?;
    // SAFETY: name is NUL-terminated, parent is live, and successful openat
    // returns a fresh descriptor owned below.
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, 0) };
    if descriptor == -1 {
        return Err(Error::Io {
            operation: "open package tree entry",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: openat returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) }.into())
}

pub(super) fn openat2_file(
    dirfd: RawFd,
    path: &[u8],
    flags: i32,
    resolve: u64,
    display_path: &Path,
) -> Result<File, Error> {
    let path_c = CString::new(path).map_err(|_| Error::InvalidPath {
        path: display_path.to_owned(),
        detail: "path contains a NUL byte",
    })?;
    // SAFETY: all-zero open_how is valid before the public fields are set.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = 0;
    how.resolve = resolve;
    // SAFETY: path_c and how remain live; successful openat2 returns a fresh
    // descriptor owned below.
    let result = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            path_c.as_ptr(),
            &how,
            size_of::<libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(Error::Io {
            operation: "open descriptor-anchored package path",
            path: display_path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let descriptor = RawFd::try_from(result).map_err(|_| Error::ArithmeticOverflow {
        resource: "file descriptor",
        path: display_path.to_owned(),
    })?;
    // SAFETY: successful openat2 returned a fresh descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) }.into())
}

pub(super) fn c_name(name: &OsStr, path: &Path) -> Result<CString, Error> {
    if name.is_empty() || name.as_bytes().contains(&b'/') {
        return Err(Error::InvalidPath {
            path: path.to_owned(),
            detail: "entry name is not one normal path component",
        });
    }
    CString::new(name.as_bytes()).map_err(|_| Error::InvalidPath {
        path: path.to_owned(),
        detail: "entry name contains a NUL byte",
    })
}

pub(super) fn relative_to_root(root: &Path, path: &Path) -> Result<PathBuf, Error> {
    let relative = path.strip_prefix(root).map_err(|_| Error::OutsideRoot {
        root: root.to_owned(),
        path: path.to_owned(),
    })?;
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(component) => {
                if component.to_str().is_none() {
                    return Err(Error::NonUtf8Path { path: path.to_owned() });
                }
                normalized.push(component);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::InvalidPath {
                    path: path.to_owned(),
                    detail: "path is not a normalized relative descendant",
                });
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(Error::InvalidPath {
            path: path.to_owned(),
            detail: "collector root itself is not a package entry",
        });
    }
    Ok(normalized)
}

pub(super) fn split_parent_name(relative: &Path, display_path: &Path) -> Result<(PathBuf, OsString), Error> {
    let name = relative.file_name().ok_or_else(|| Error::InvalidPath {
        path: display_path.to_owned(),
        detail: "package entry has no file name",
    })?;
    Ok((
        relative.parent().unwrap_or_else(|| Path::new("")).to_owned(),
        name.to_owned(),
    ))
}

pub(super) fn join_relative(parent: &Path, name: &OsStr) -> PathBuf {
    let mut relative = parent.to_owned();
    relative.push(name);
    relative
}

pub(super) fn require_usable_phase(state: &WitnessState, operation: &'static str) -> Result<(), Error> {
    match state.phase {
        WitnessPhase::AdmissionsOpen | WitnessPhase::Sealed => Ok(()),
        WitnessPhase::Poisoned => Err(Error::InventoryPoisoned),
        phase => Err(Error::InvalidInventoryPhase {
            operation,
            phase: phase.name(),
        }),
    }
}

pub(super) fn find_child<'a>(
    directories: &'a [DirectoryWitness],
    parent: DirectoryId,
    name: &OsStr,
) -> Option<&'a WitnessChild> {
    let children = &directories.get(parent)?.children;
    children
        .binary_search_by(|child| child.name.as_os_str().cmp(name))
        .ok()
        .map(|position| &children[position])
}

pub(super) fn lookup_directory(directories: &[DirectoryWitness], relative: &Path) -> Option<DirectoryId> {
    let mut id = 0;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return None;
        };
        let child = find_child(directories, id, name)?;
        let WitnessChildKind::Directory(child_id) = &child.kind else {
            return None;
        };
        id = *child_id;
    }
    Some(id)
}

pub(super) fn directory_relative(
    directories: &[DirectoryWitness],
    mut id: DirectoryId,
    display_root: &Path,
) -> Result<PathBuf, Error> {
    let mut lineage = Vec::new();
    loop {
        let directory = directories.get(id).ok_or_else(|| Error::UnwitnessedPath {
            path: display_root.to_owned(),
        })?;
        let Some(parent) = directory.parent else {
            break;
        };
        reserve(&mut lineage, 1, "witnessed directory lineage")?;
        lineage.push(directory.name.as_os_str());
        id = parent;
    }
    let mut relative = PathBuf::new();
    for name in lineage.into_iter().rev() {
        relative.push(name);
    }
    Ok(relative)
}

pub(super) fn compare_exact_inventory(
    expected: &[DirectoryWitness],
    actual: &[DirectoryWitness],
    root: &Path,
    deadline: &Deadline,
) -> Result<(), Error> {
    if expected.is_empty() || actual.is_empty() {
        return Err(changed(root, "complete witnessed package inventory root changed"));
    }
    let mut tasks = Vec::new();
    reserve(&mut tasks, 1, "package inventory comparison tasks")?;
    tasks.push((0usize, 0usize));
    while let Some((expected_id, actual_id)) = tasks.pop() {
        deadline.check(root)?;
        let expected_directory = expected
            .get(expected_id)
            .ok_or_else(|| changed(root, "invalid witnessed directory edge"))?;
        let actual_directory = actual
            .get(actual_id)
            .ok_or_else(|| changed(root, "invalid scanned directory edge"))?;
        if expected_directory.snapshot != actual_directory.snapshot
            || expected_directory.children.len() != actual_directory.children.len()
        {
            return Err(changed(root, "complete witnessed package directory changed"));
        }
        for (expected_child, actual_child) in expected_directory.children.iter().zip(&actual_directory.children) {
            deadline.check(root)?;
            if expected_child.name != actual_child.name {
                return Err(changed(root, "complete witnessed package membership changed"));
            }
            match (&expected_child.kind, &actual_child.kind) {
                (WitnessChildKind::Directory(expected), WitnessChildKind::Directory(actual)) => {
                    reserve(&mut tasks, 1, "package inventory comparison tasks")?;
                    tasks.push((*expected, *actual));
                }
                (WitnessChildKind::Entry(expected), WitnessChildKind::Entry(actual)) if expected == actual => {}
                _ => return Err(changed(root, "complete witnessed package entry changed")),
            }
        }
    }
    Ok(())
}

pub(super) fn stable_directory_snapshot(expected: FileSnapshot, actual: FileSnapshot) -> bool {
    expected.node == actual.node
        && expected.mode == actual.mode
        && expected.uid == actual.uid
        && expected.gid == actual.gid
}

pub(super) fn add_admission_delta_for_relative(
    relative: &Path,
    child: &WitnessChild,
    display_path: &Path,
    delta: &mut AdmissionDelta,
) -> Result<(), Error> {
    delta.entries = delta.entries.checked_add(1).ok_or(Error::ArithmeticOverflow {
        resource: "generated package entries",
        path: display_path.to_owned(),
    })?;
    delta.name_bytes =
        delta
            .name_bytes
            .checked_add(child.name.as_bytes().len() as u64)
            .ok_or(Error::ArithmeticOverflow {
                resource: "generated package entry name bytes",
                path: display_path.to_owned(),
            })?;
    delta.path_bytes = delta
        .path_bytes
        .checked_add(relative.as_os_str().as_bytes().len() as u64)
        .ok_or(Error::ArithmeticOverflow {
            resource: "generated package entry path bytes",
            path: display_path.to_owned(),
        })?;
    if let WitnessChildKind::Entry(entry) = &child.kind {
        match &entry.kind {
            WitnessEntryKind::Regular { .. } => {
                delta.regular_bytes =
                    delta
                        .regular_bytes
                        .checked_add(entry.snapshot.size)
                        .ok_or(Error::ArithmeticOverflow {
                            resource: "generated package regular bytes",
                            path: display_path.to_owned(),
                        })?;
            }
            WitnessEntryKind::Symlink { target } => {
                delta.symlink_target_bytes =
                    delta
                        .symlink_target_bytes
                        .checked_add(target.len() as u64)
                        .ok_or(Error::ArithmeticOverflow {
                            resource: "generated package symlink target bytes",
                            path: display_path.to_owned(),
                        })?;
            }
            WitnessEntryKind::Special => {}
        }
    }
    Ok(())
}

pub(super) fn usage_after_admission(
    usage: &CollectionUsage,
    delta: &AdmissionDelta,
    limits: CollectionLimits,
    path: &Path,
) -> Result<CollectionUsage, Error> {
    let mut updated = usage.clone();
    updated.entries = checked_add_limit(
        "total entries",
        updated.entries,
        delta.entries,
        limits.max_entries,
        path,
    )?;
    updated.name_bytes = checked_add_limit(
        "total entry name bytes",
        updated.name_bytes,
        delta.name_bytes,
        limits.max_total_name_bytes,
        path,
    )?;
    updated.path_bytes = checked_add_limit(
        "total entry path bytes",
        updated.path_bytes,
        delta.path_bytes,
        limits.max_total_path_bytes,
        path,
    )?;
    updated.symlink_target_bytes = checked_add_limit(
        "total symlink target bytes",
        updated.symlink_target_bytes,
        delta.symlink_target_bytes,
        limits.max_total_symlink_target_bytes,
        path,
    )?;
    updated.regular_bytes = checked_add_limit(
        "total regular file bytes",
        updated.regular_bytes,
        delta.regular_bytes,
        limits.max_total_regular_bytes,
        path,
    )?;
    Ok(updated)
}

pub(super) fn validate_usage(usage: &CollectionUsage, limits: CollectionLimits, path: &Path) -> Result<(), Error> {
    enforce_u64_limit("total entries", limits.max_entries, usage.entries, path)?;
    enforce_u64_limit(
        "total entry name bytes",
        limits.max_total_name_bytes,
        usage.name_bytes,
        path,
    )?;
    enforce_u64_limit(
        "total entry path bytes",
        limits.max_total_path_bytes,
        usage.path_bytes,
        path,
    )?;
    enforce_u64_limit(
        "total symlink target bytes",
        limits.max_total_symlink_target_bytes,
        usage.symlink_target_bytes,
        path,
    )?;
    enforce_u64_limit(
        "total regular file bytes",
        limits.max_total_regular_bytes,
        usage.regular_bytes,
        path,
    )
}

pub(super) fn reserve_admission_commit(
    directories: &mut Vec<DirectoryWitness>,
    draft: &AdmissionDraft,
) -> Result<(), Error> {
    reserve(
        directories,
        draft.new_directories.len(),
        "generated package directories",
    )?;
    for update in &draft.existing {
        reserve(
            &mut directories[update.id].children,
            update.additions.len(),
            "generated package child edges",
        )?;
    }
    Ok(())
}

pub(super) fn commit_admission(directories: &mut Vec<DirectoryWitness>, draft: AdmissionDraft) {
    for update in draft.existing {
        let directory = &mut directories[update.id];
        directory.snapshot = update.snapshot;
        for child in update.additions {
            let position = directory
                .children
                .binary_search_by(|candidate| candidate.name.cmp(&child.name))
                .expect_err("generated child was proven absent before commit");
            directory.children.insert(position, child);
        }
    }
    directories.extend(draft.new_directories);
}

pub(super) fn copy_os_string(bytes: &[u8], path: &Path) -> Result<OsString, Error> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(bytes.len())
        .map_err(|source| Error::Allocation {
            resource: "directory entry name bytes",
            requested: bytes.len(),
            detail: source.to_string(),
        })?;
    owned.extend_from_slice(bytes);
    let _ = path;
    Ok(OsString::from_vec(owned))
}

pub(super) fn copy_string(value: &str, resource: &'static str) -> Result<String, Error> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|source| Error::Allocation {
            resource,
            requested: value.len(),
            detail: source.to_string(),
        })?;
    owned.push_str(value);
    Ok(owned)
}

pub(super) fn reserve<T>(items: &mut Vec<T>, additional: usize, resource: &'static str) -> Result<(), Error> {
    items.try_reserve(additional).map_err(|source| Error::Allocation {
        resource,
        requested: additional,
        detail: source.to_string(),
    })
}

pub(super) fn metadata(file: &File, operation: &'static str, path: &Path) -> Result<Metadata, Error> {
    file.metadata().map_err(|source| Error::Io {
        operation,
        path: path.to_owned(),
        source,
    })
}

pub(super) fn require_snapshot(path: &Path, expected: FileSnapshot, metadata: &Metadata) -> Result<(), Error> {
    let actual = FileSnapshot::from_metadata(metadata);
    if actual == expected {
        Ok(())
    } else {
        Err(changed(path, "entry identity or metadata changed"))
    }
}

pub(super) fn changed(path: &Path, detail: &'static str) -> Error {
    Error::TreeChanged {
        path: path.to_owned(),
        detail,
    }
}

pub(super) fn is_supported_special(file_type: &std::fs::FileType) -> bool {
    file_type.is_char_device() || file_type.is_block_device() || file_type.is_fifo() || file_type.is_socket()
}

pub(super) fn enforce_usize_limit(
    resource: &'static str,
    limit: usize,
    actual: usize,
    path: &Path,
) -> Result<(), Error> {
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

pub(super) fn enforce_u64_limit(resource: &'static str, limit: u64, actual: u64, path: &Path) -> Result<(), Error> {
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

pub(super) fn checked_add_limit(
    resource: &'static str,
    current: u64,
    additional: u64,
    limit: u64,
    path: &Path,
) -> Result<u64, Error> {
    let actual = current.checked_add(additional).ok_or(Error::ArithmeticOverflow {
        resource,
        path: path.to_owned(),
    })?;
    enforce_u64_limit(resource, limit, actual, path)?;
    Ok(actual)
}
