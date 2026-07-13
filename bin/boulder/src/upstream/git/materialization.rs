// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Canonical normalization and hashing for a checked-out Git tree.

use std::{
    ffi::{CString, OsStr},
    fs::Permissions,
    io::{self, Read},
    mem::{size_of, zeroed},
    os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use fs_err as fs;
use nix::{errno::Errno, libc};
use sha2::{Digest, Sha256};
use thiserror::Error;

const DOMAIN: &[u8] = b"os-tools-git-materialization-v1\0";
const DIRECTORY_TAG: u8 = 0;
const REGULAR_TAG: u8 = 1;
const SYMLINK_TAG: u8 = 2;
const DIRECTORY_MODE: u32 = 0o755;
const EXECUTABLE_MODE: u32 = 0o755;
const REGULAR_MODE: u32 = 0o644;
const SYMLINK_MODE: u32 = 0o777;

/// Normalize one exported Git tree and return its canonical SHA-256 digest.
///
/// The initial scan is deliberately separate from mutation: a hard link or
/// special inode anywhere in the tree rejects the complete export before a
/// mode or timestamp is changed. The final scan is the sole source of bytes
/// admitted to the digest.
pub(super) fn normalize_and_hash(root: &Path, source_date_epoch: i64) -> Result<String, Error> {
    normalize_and_hash_with(RootHandle::open(root)?, source_date_epoch, |_| {})
}

/// Normalize a checkout reached beneath a trusted `/proc/<pid>/fd/<fd>`
/// source-root path. The magic link is an intentional reference to a held
/// directory descriptor; the checkout itself is opened without following its
/// final component and all descendant work remains fd-anchored.
pub(super) fn normalize_and_hash_descriptor_path(root: &Path, source_date_epoch: i64) -> Result<String, Error> {
    normalize_and_hash_with(RootHandle::open_descriptor_path(root)?, source_date_epoch, |_| {})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookPoint {
    AfterAudit,
    AfterCanonicalHash,
}

fn normalize_and_hash_with(
    root: RootHandle,
    source_date_epoch: i64,
    mut hook: impl FnMut(HookPoint),
) -> Result<String, Error> {
    let audited = scan_tree(&root, false, None)?;
    hook(HookPoint::AfterAudit);
    normalize_entries(&root, &audited, source_date_epoch)?;

    // This scan must read symlink targets to prove that normalization did not
    // change semantic bytes. Reset timestamps once more, then every remaining
    // scan can reuse an unchanged symlink inode's immutable target bytes.
    let normalized = scan_tree(&root, true, None)?;
    require_same_tree(&audited, &normalized)?;
    normalize_entries(&root, &normalized, source_date_epoch)?;

    let stable = scan_tree(&root, true, Some(&normalized))?;
    require_same_tree(&normalized, &stable)?;
    let digest = hash_tree(&root, &stable)?;
    hook(HookPoint::AfterCanonicalHash);

    // Re-enumerate the complete tree and hash it again. The scan catches an
    // added or removed path; the second digest catches same-length content
    // replacement even on a filesystem with coarse metadata timestamps.
    let confirmed = scan_tree(&root, true, Some(&stable))?;
    require_same_tree(&stable, &confirmed)?;
    let confirmation = hash_tree(&root, &confirmed)?;
    if confirmation != digest {
        return Err(Error::TreeChanged);
    }
    require_stable_tree(&stable, &confirmed)?;

    // Hashing and enumeration use O_NOATIME, so this final full rescan needs no
    // metadata repair afterward. Exact metadata equality detects a mutation
    // that raced the confirmation hash, and reopening the root proves that its
    // public path still names the directory held by our root descriptor.
    let final_tree = scan_tree(&root, true, Some(&confirmed))?;
    require_stable_tree(&confirmed, &final_tree)?;
    verify_normalized_tree(&final_tree, source_date_epoch)?;
    root.require_path_identity()?;

    Ok(hex::encode(digest))
}

struct RootHandle {
    path: PathBuf,
    file: fs::File,
    identity: Identity,
    descriptor_path: bool,
}

impl RootHandle {
    fn open(path: &Path) -> Result<Self, Error> {
        let path = std::path::absolute(path).map_err(|source| Error::Io {
            operation: "make Git materialization root absolute",
            path: path.to_owned(),
            source,
        })?;
        let file = openat2_file(
            libc::AT_FDCWD,
            path.as_os_str().as_bytes(),
            data_open_flags(true),
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
        )
        .map(|file| fs::File::from_parts(file, &path))
        .map_err(|source| Error::Io {
            operation: "open Git materialization root without symlinks",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| Error::Io {
            operation: "inspect opened Git materialization root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(Error::RootNotDirectory(path));
        }
        Ok(Self {
            path,
            identity: Identity::from_metadata(&metadata),
            file,
            descriptor_path: false,
        })
    }

    fn open_descriptor_path(path: &Path) -> Result<Self, Error> {
        let path = std::path::absolute(path).map_err(|source| Error::Io {
            operation: "make descriptor-rooted Git materialization path absolute",
            path: path.to_owned(),
            source,
        })?;
        let file = open_path_file(&path, data_open_flags(true))
            .map(|file| fs::File::from_parts(file, &path))
            .map_err(|source| Error::Io {
                operation: "open descriptor-rooted Git materialization root",
                path: path.clone(),
                source,
            })?;
        let metadata = file.metadata().map_err(|source| Error::Io {
            operation: "inspect descriptor-rooted Git materialization root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(Error::RootNotDirectory(path));
        }
        Ok(Self {
            path,
            identity: Identity::from_metadata(&metadata),
            file,
            descriptor_path: true,
        })
    }

    fn display_path(&self, relative: &[u8]) -> PathBuf {
        if relative.is_empty() {
            self.path.clone()
        } else {
            self.path.join(OsStr::from_bytes(relative))
        }
    }

    fn open_relative(&self, relative: &[u8], flags: i32, operation: &'static str) -> Result<fs::File, Error> {
        let path = self.display_path(relative);
        // A duplicated directory descriptor shares its directory-stream
        // offset with the original open file description. Open `.` afresh for
        // the root so every full-tree scan begins at offset zero.
        let relative = if relative.is_empty() { b".".as_slice() } else { relative };
        openat2_file(
            self.file.as_raw_fd(),
            relative,
            flags,
            libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
        )
        .map(|file| fs::File::from_parts(file, &path))
        .map_err(|source| Error::Io {
            operation,
            path,
            source,
        })
    }

    fn inspect_entry(&self, relative: &[u8], expected: Option<&[Entry]>) -> Result<Entry, Error> {
        let path = self.display_path(relative);
        let file = self.open_relative(
            relative,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            "open Git materialization entry for inspection",
        )?;
        let metadata = file.metadata().map_err(|source| Error::Io {
            operation: "inspect opened Git materialization entry",
            path: path.clone(),
            source,
        })?;
        let identity = Identity::from_metadata(&metadata);
        let kind = classify_handle(&path, &file, &metadata, relative, identity, expected)?;
        require_single_link(&path, &metadata, &kind)?;
        Ok(Entry {
            path,
            relative: relative.to_vec(),
            identity,
            kind,
            stamp: MetadataStamp::from_metadata(&metadata),
        })
    }

    fn open_data(&self, entry: &Entry) -> Result<fs::File, Error> {
        self.open_relative(
            &entry.relative,
            data_open_flags(entry.kind.is_directory()),
            "open Git materialization entry beneath root",
        )
    }

    fn open_inspection(&self, entry: &Entry) -> Result<fs::File, Error> {
        self.open_relative(
            &entry.relative,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            "reopen Git materialization entry for inspection",
        )
    }

    fn require_path_identity(&self) -> Result<(), Error> {
        let reopened = if self.descriptor_path {
            open_path_file(&self.path, data_open_flags(true))
        } else {
            openat2_file(
                libc::AT_FDCWD,
                self.path.as_os_str().as_bytes(),
                data_open_flags(true),
                libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            )
        }
        .map_err(|source| Error::Io {
            operation: "reopen Git materialization root without symlinks",
            path: self.path.clone(),
            source,
        })?;
        let metadata = reopened.metadata().map_err(|source| Error::Io {
            operation: "verify Git materialization root path",
            path: self.path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() || Identity::from_metadata(&metadata) != self.identity {
            return Err(Error::EntryChanged(self.path.clone()));
        }
        Ok(())
    }
}

fn open_path_file(path: &Path, flags: i32) -> io::Result<std::fs::File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: `path` is NUL-terminated and a successful `open` returns a new
    // descriptor. O_NOFOLLOW applies to the checkout itself while permitting
    // the intentional held-fd magic link in an ancestor component.
    let result = unsafe { libc::open(path.as_ptr(), flags) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: successful open returned a fresh owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(result) }.into())
    }
}

fn data_open_flags(directory: bool) -> i32 {
    let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOATIME;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    flags
}

fn openat2_file(dirfd: RawFd, path: &[u8], flags: i32, resolve: u64) -> io::Result<std::fs::File> {
    let path = CString::new(path).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: every field of Linux `open_how` accepts zero, after which the
    // public fields used by this ABI version are initialized explicitly.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = 0;
    how.resolve = resolve;
    // SAFETY: `path` is NUL-terminated, `how` points to an initialized
    // `open_how`, and a successful syscall returns a new owned descriptor.
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
    // SAFETY: successful openat2 returns a fresh descriptor owned by us.
    let descriptor = unsafe { OwnedFd::from_raw_fd(result as RawFd) };
    Ok(descriptor.into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    relative: Vec<u8>,
    identity: Identity,
    kind: EntryKind,
    stamp: MetadataStamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MetadataStamp {
    mode: u32,
    links: u64,
    atime: i64,
    atime_nsec: i64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

impl MetadataStamp {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            mode: metadata.mode() & 0o7777,
            links: metadata.nlink(),
            atime: metadata.atime(),
            atime_nsec: metadata.atime_nsec(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryKind {
    Directory,
    Regular { executable: bool, length: u64 },
    Symlink { target: Vec<u8> },
}

impl EntryKind {
    fn tag(&self) -> u8 {
        match self {
            Self::Directory => DIRECTORY_TAG,
            Self::Regular { .. } => REGULAR_TAG,
            Self::Symlink { .. } => SYMLINK_TAG,
        }
    }

    fn normalized_mode(&self) -> u32 {
        match self {
            Self::Directory => DIRECTORY_MODE,
            Self::Regular { executable: true, .. } => EXECUTABLE_MODE,
            Self::Regular { executable: false, .. } => REGULAR_MODE,
            Self::Symlink { .. } => SYMLINK_MODE,
        }
    }

    fn is_directory(&self) -> bool {
        matches!(self, Self::Directory)
    }
}

fn scan_tree(
    root: &RootHandle,
    require_normalized_modes: bool,
    expected: Option<&[Entry]>,
) -> Result<Vec<Entry>, Error> {
    let root_entry = root.inspect_entry(b"", expected)?;
    let mut entries = vec![root_entry.clone()];
    scan_directory(root, &root_entry, require_normalized_modes, expected, &mut entries)?;
    entries.sort_by(|left, right| left.relative.cmp(&right.relative));
    for adjacent in entries.windows(2) {
        if adjacent[0].relative == adjacent[1].relative {
            return Err(Error::DuplicatePath(adjacent[0].path.clone()));
        }
    }
    Ok(entries)
}

fn scan_directory(
    root: &RootHandle,
    directory: &Entry,
    require_normalized_modes: bool,
    expected: Option<&[Entry]>,
    entries: &mut Vec<Entry>,
) -> Result<(), Error> {
    let file = root.open_data(directory)?;
    let metadata = file.metadata().map_err(|source| Error::Io {
        operation: "inspect opened Git materialization directory",
        path: directory.path.clone(),
        source,
    })?;
    require_handle_matches(directory, &metadata, require_normalized_modes)?;
    let names = read_directory_names(file, &directory.path)?;

    for name in names {
        let mut relative = directory.relative.clone();
        if !relative.is_empty() {
            relative.push(b'/');
        }
        relative.extend_from_slice(&name);
        let entry = root.inspect_entry(&relative, expected)?;
        if require_normalized_modes {
            let metadata = root.open_inspection(&entry)?.metadata().map_err(|source| Error::Io {
                operation: "verify Git materialization entry mode",
                path: entry.path.clone(),
                source,
            })?;
            require_normalized_mode(&entry.path, &metadata, &entry.kind)?;
        }
        let is_directory = entry.kind.is_directory();
        entries.push(entry.clone());
        if is_directory {
            scan_directory(root, &entry, require_normalized_modes, expected, entries)?;
        }
    }
    Ok(())
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

    fn names(&mut self, path: &Path) -> Result<Vec<Vec<u8>>, Error> {
        let mut names = Vec::new();
        loop {
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
                    path: path.to_owned(),
                    source: io::Error::from_raw_os_error(error as i32),
                });
            }
            // SAFETY: readdir64 returned a live dirent whose d_name is
            // NUL-terminated for the duration of this loop iteration.
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                names.push(name.to_vec());
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

fn read_directory_names(file: fs::File, path: &Path) -> Result<Vec<Vec<u8>>, Error> {
    DirectoryStream::from_file(file, path)?.names(path)
}

fn classify_handle(
    path: &Path,
    file: &fs::File,
    metadata: &std::fs::Metadata,
    relative: &[u8],
    identity: Identity,
    expected: Option<&[Entry]>,
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
                EntryKind::Symlink { target } => Some(target.clone()),
                _ => None,
            });
        let target = match reused {
            Some(target) => target,
            None => read_symlink_handle(file, path)?,
        };
        Ok(EntryKind::Symlink { target })
    } else {
        Err(Error::UnsupportedFileType {
            path: path.to_owned(),
            kind: special_file_type(&file_type),
        })
    }
}

fn read_symlink_handle(file: &fs::File, path: &Path) -> Result<Vec<u8>, Error> {
    nix::fcntl::readlinkat(file.as_raw_fd(), OsStr::new(""))
        .map(|target| target.as_os_str().as_bytes().to_vec())
        .map_err(|error| Error::Io {
            operation: "read opened Git materialization symlink",
            path: path.to_owned(),
            source: io::Error::from_raw_os_error(error as i32),
        })
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

fn normalize_entries(root: &RootHandle, entries: &[Entry], source_date_epoch: i64) -> Result<(), Error> {
    let timestamp = filetime::FileTime::from_unix_time(source_date_epoch, 0);

    // Children precede their directories, so directory timestamps are the
    // final metadata operation for each subtree.
    for entry in entries.iter().rev() {
        match &entry.kind {
            EntryKind::Symlink { .. } => {
                let file = root.open_inspection(entry)?;
                require_symlink_handle_matches(entry, &file, true)?;
                set_symlink_handle_times(&file, &entry.path, source_date_epoch)?;
                // Replacing a symlink requires a new inode, so fstat identity
                // and type are sufficient after the timestamp write.
                let metadata = file.metadata().map_err(|source| Error::Io {
                    operation: "verify timestamped Git materialization symlink",
                    path: entry.path.clone(),
                    source,
                })?;
                if Identity::from_metadata(&metadata) != entry.identity || !metadata.file_type().is_symlink() {
                    return Err(Error::EntryChanged(entry.path.clone()));
                }
                require_single_link(&entry.path, &metadata, &entry.kind)?;
            }
            EntryKind::Directory | EntryKind::Regular { .. } => {
                let file = root.open_data(entry)?;
                require_handle_matches(
                    entry,
                    &file.metadata().map_err(|source| Error::Io {
                        operation: "inspect opened Git materialization entry",
                        path: entry.path.clone(),
                        source,
                    })?,
                    false,
                )?;
                file.set_permissions(Permissions::from_mode(entry.kind.normalized_mode()))
                    .map_err(|source| Error::Io {
                        operation: "normalize Git materialization mode",
                        path: entry.path.clone(),
                        source,
                    })?;
                filetime::set_file_handle_times(file.file(), Some(timestamp), Some(timestamp)).map_err(|source| {
                    Error::Io {
                        operation: "normalize Git materialization timestamp",
                        path: entry.path.clone(),
                        source,
                    }
                })?;
                require_handle_matches(
                    entry,
                    &file.metadata().map_err(|source| Error::Io {
                        operation: "verify opened Git materialization entry",
                        path: entry.path.clone(),
                        source,
                    })?,
                    true,
                )?;
            }
        }
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

fn require_symlink_handle_matches(entry: &Entry, file: &fs::File, verify_target: bool) -> Result<(), Error> {
    let metadata = file.metadata().map_err(|source| Error::Io {
        operation: "inspect opened Git materialization symlink",
        path: entry.path.clone(),
        source,
    })?;
    if !metadata.file_type().is_symlink() || Identity::from_metadata(&metadata) != entry.identity {
        return Err(Error::EntryChanged(entry.path.clone()));
    }
    require_single_link(&entry.path, &metadata, &entry.kind)?;
    if verify_target {
        let EntryKind::Symlink { target } = &entry.kind else {
            return Err(Error::EntryChanged(entry.path.clone()));
        };
        if read_symlink_handle(file, &entry.path)? != *target {
            return Err(Error::EntryChanged(entry.path.clone()));
        }
    }
    Ok(())
}

fn require_handle_matches(entry: &Entry, metadata: &std::fs::Metadata, require_normalized: bool) -> Result<(), Error> {
    let kind = if metadata.file_type().is_dir() {
        EntryKind::Directory
    } else if metadata.file_type().is_file() {
        EntryKind::Regular {
            executable: metadata.mode() & 0o111 != 0,
            length: metadata.len(),
        }
    } else {
        return Err(Error::EntryChanged(entry.path.clone()));
    };
    require_single_link(&entry.path, metadata, &kind)?;
    if require_normalized {
        require_normalized_mode(&entry.path, metadata, &kind)?;
    }
    if Identity::from_metadata(metadata) != entry.identity || kind != entry.kind {
        return Err(Error::EntryChanged(entry.path.clone()));
    }
    Ok(())
}

fn require_same_tree(audited: &[Entry], normalized: &[Entry]) -> Result<(), Error> {
    if audited.len() != normalized.len() {
        return Err(Error::TreeChanged);
    }
    for (expected, actual) in audited.iter().zip(normalized) {
        if expected.relative != actual.relative || expected.identity != actual.identity || expected.kind != actual.kind
        {
            return Err(Error::EntryChanged(actual.path.clone()));
        }
    }
    Ok(())
}

fn require_stable_tree(expected: &[Entry], actual: &[Entry]) -> Result<(), Error> {
    require_same_tree(expected, actual)?;
    for (expected, actual) in expected.iter().zip(actual) {
        if expected.stamp != actual.stamp {
            return Err(Error::EntryChanged(actual.path.clone()));
        }
    }
    Ok(())
}

fn hash_tree(root: &RootHandle, entries: &[Entry]) -> Result<[u8; 32], Error> {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hash_length(
        &mut hasher,
        entries.len(),
        "entry count",
        &entries.first().expect("a scanned tree includes its root").path,
    )?;

    for entry in entries {
        match &entry.kind {
            EntryKind::Directory => {
                let file = root.open_data(entry)?;
                let metadata = file.metadata().map_err(|source| Error::Io {
                    operation: "inspect Git materialization directory while hashing",
                    path: entry.path.clone(),
                    source,
                })?;
                require_handle_matches(entry, &metadata, true)?;
            }
            EntryKind::Regular { .. } => {}
            EntryKind::Symlink { .. } => {
                let file = root.open_inspection(entry)?;
                require_symlink_handle_matches(entry, &file, false)?;
            }
        }
        hasher.update([entry.kind.tag()]);
        hash_length(&mut hasher, entry.relative.len(), "relative path", &entry.path)?;
        hasher.update(&entry.relative);
        hasher.update(entry.kind.normalized_mode().to_le_bytes());

        match &entry.kind {
            EntryKind::Directory => {}
            EntryKind::Regular { length, .. } => hash_regular(root, entry, *length, &mut hasher)?,
            EntryKind::Symlink { target } => hash_symlink(entry, target, &mut hasher)?,
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

fn hash_regular(root: &RootHandle, entry: &Entry, expected_length: u64, hasher: &mut Sha256) -> Result<(), Error> {
    let mut file = root.open_data(entry)?;
    let before = file.metadata().map_err(|source| Error::Io {
        operation: "inspect Git materialization file before hashing",
        path: entry.path.clone(),
        source,
    })?;
    require_handle_matches(entry, &before, true)?;
    if before.len() != expected_length {
        return Err(Error::FileLengthChanged {
            path: entry.path.clone(),
            expected: expected_length,
            actual: before.len(),
        });
    }

    hasher.update(expected_length.to_le_bytes());
    let mut read_length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while read_length < expected_length {
        let remaining = expected_length - read_length;
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).expect("buffer length fits usize");
        let read = match file.read(&mut buffer[..limit]) {
            Ok(0) => {
                return Err(Error::FileLengthChanged {
                    path: entry.path.clone(),
                    expected: expected_length,
                    actual: read_length,
                });
            }
            Ok(read) => read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(Error::Io {
                    operation: "read Git materialization file",
                    path: entry.path.clone(),
                    source,
                });
            }
        };
        hasher.update(&buffer[..read]);
        read_length += u64::try_from(read).expect("buffer read length fits u64");
    }

    let mut extra = [0_u8; 1];
    let extra_read = loop {
        match file.read(&mut extra) {
            Ok(read) => break read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                return Err(Error::Io {
                    operation: "verify Git materialization file length",
                    path: entry.path.clone(),
                    source,
                });
            }
        }
    };
    let after = file.metadata().map_err(|source| Error::Io {
        operation: "inspect Git materialization file after hashing",
        path: entry.path.clone(),
        source,
    })?;
    if extra_read != 0 || after.len() != expected_length {
        let observed_extra = if extra_read == 0 { 0 } else { 1 };
        return Err(Error::FileLengthChanged {
            path: entry.path.clone(),
            expected: expected_length,
            actual: after.len().max(expected_length + observed_extra),
        });
    }
    if content_stamp(&before) != content_stamp(&after) {
        return Err(Error::FileChangedDuringHash(entry.path.clone()));
    }
    require_handle_matches(entry, &after, true)?;
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

fn hash_symlink(entry: &Entry, expected_target: &[u8], hasher: &mut Sha256) -> Result<(), Error> {
    hash_length(hasher, expected_target.len(), "symlink target", &entry.path)?;
    hasher.update(expected_target);
    Ok(())
}

fn verify_normalized_tree(entries: &[Entry], source_date_epoch: i64) -> Result<(), Error> {
    for entry in entries {
        if entry.stamp.atime != source_date_epoch
            || entry.stamp.atime_nsec != 0
            || entry.stamp.mtime != source_date_epoch
            || entry.stamp.mtime_nsec != 0
        {
            return Err(Error::TimestampNotNormalized {
                path: entry.path.clone(),
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

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("Git materialization root is not a directory: {0:?}")]
    RootNotDirectory(PathBuf),
    #[error("Git materialization entry {path:?} has unsupported type {kind}")]
    UnsupportedFileType { path: PathBuf, kind: &'static str },
    #[error("Git materialization non-directory {path:?} has link count {links}; expected exactly one")]
    UnexpectedLinkCount { path: PathBuf, links: u64 },
    #[error("Git materialization entry {0:?} changed during normalization or hashing")]
    EntryChanged(PathBuf),
    #[error("Git materialization tree changed during normalization or hashing")]
    TreeChanged,
    #[error("Git materialization tree contains duplicate path {0:?}")]
    DuplicatePath(PathBuf),
    #[error("Git materialization {field} length for {path:?} cannot be represented as u64")]
    LengthNotRepresentable { field: &'static str, path: PathBuf },
    #[error("Git materialization entry {path:?} has mode {actual:#06o}; expected {expected:#06o}")]
    ModeNotNormalized { path: PathBuf, expected: u32, actual: u32 },
    #[error("Git materialization file {path:?} changed length while hashing (expected {expected}, found {actual})")]
    FileLengthChanged { path: PathBuf, expected: u64, actual: u64 },
    #[error("Git materialization file {0:?} changed while its bytes were being hashed")]
    FileChangedDuringHash(PathBuf),
    #[error(
        "Git materialization entry {path:?} did not retain timestamp {expected}.0 (atime={atime}.{atime_nsec}, mtime={mtime}.{mtime_nsec})"
    )]
    TimestampNotNormalized {
        path: PathBuf,
        expected: i64,
        atime: i64,
        atime_nsec: i64,
        mtime: i64,
        mtime_nsec: i64,
    },
    #[error("{operation} at {path:?}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        os::unix::{
            ffi::OsStringExt,
            fs::{MetadataExt, PermissionsExt, symlink},
            net::UnixListener,
        },
    };

    use nix::{sys::stat::Mode, unistd::mkfifo};

    use super::*;

    const EPOCH: i64 = 1_700_000_000;

    #[test]
    fn empty_tree_has_a_stable_golden_digest() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            normalize_and_hash(root.path(), EPOCH).unwrap(),
            "badf0db4c0a8fd2cde62d7893df1313fcdaca41f8b9cab21c2e58f53c033c908"
        );
    }

    #[test]
    fn trusted_descriptor_root_path_is_accepted_without_weakening_descendant_traversal() {
        let temporary = tempfile::tempdir().unwrap();
        let held_root = temporary.path().join("held");
        let checkout = held_root.join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join("source"), b"descriptor rooted").unwrap();
        let held = fs::File::open(&held_root).unwrap();
        let descriptor_checkout =
            PathBuf::from(format!("/proc/{}/fd/{}/checkout", std::process::id(), held.as_raw_fd()));

        let digest = normalize_and_hash_descriptor_path(&descriptor_checkout, EPOCH).unwrap();

        assert_eq!(digest.len(), 64);
        assert_eq!(fs::read(checkout.join("source")).unwrap(), b"descriptor rooted");
    }

    #[test]
    fn order_non_utf8_permissions_and_timestamps_normalize_identically() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let raw_name = OsString::from_vec(b"non-utf8-\xff".to_vec());
        create_equivalent_tree(first.path(), &raw_name, false, 111);
        create_equivalent_tree(second.path(), &raw_name, true, 222);

        let first_hash = normalize_and_hash(first.path(), EPOCH).unwrap();
        let second_hash = normalize_and_hash(second.path(), EPOCH).unwrap();
        assert_eq!(first_hash, second_hash);

        for root in [first.path(), second.path()] {
            assert_mode(root, DIRECTORY_MODE);
            assert_mode(&root.join("nested"), DIRECTORY_MODE);
            assert_mode(&root.join(&raw_name), REGULAR_MODE);
            assert_mode(&root.join("executable"), EXECUTABLE_MODE);
            for path in [
                root.to_owned(),
                root.join("nested"),
                root.join(&raw_name),
                root.join("executable"),
                root.join("link"),
            ] {
                assert_timestamp(&path, EPOCH);
            }
        }
    }

    #[test]
    fn content_path_mode_type_and_symlink_target_are_semantic() {
        let baseline = digest_with(|_| {});
        let mutations = [
            digest_with(|root| fs::write(root.join("regular"), b"bravo").unwrap()),
            digest_with(|root| fs::write(root.join("regular"), b"longer").unwrap()),
            digest_with(|root| fs::rename(root.join("regular"), root.join("renamed")).unwrap()),
            digest_with(|root| fs::set_permissions(root.join("regular"), Permissions::from_mode(0o755)).unwrap()),
            digest_with(|root| {
                fs::remove_file(root.join("link")).unwrap();
                symlink("executable", root.join("link")).unwrap();
            }),
            digest_with(|root| {
                fs::remove_file(root.join("kind")).unwrap();
                fs::create_dir(root.join("kind")).unwrap();
            }),
        ];
        for mutation in mutations {
            assert_ne!(mutation, baseline);
        }
    }

    #[test]
    fn entry_added_after_the_canonical_hash_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("original"), b"original").unwrap();

        let result = normalize_and_hash_with(RootHandle::open(root.path()).unwrap(), EPOCH, |point| {
            if point == HookPoint::AfterCanonicalHash {
                let added = root.path().join("added");
                fs::write(&added, b"added").unwrap();
                fs::set_permissions(added, Permissions::from_mode(REGULAR_MODE)).unwrap();
            }
        });

        assert!(matches!(result, Err(Error::TreeChanged)));
        assert!(root.path().join("added").exists());
    }

    #[test]
    fn same_length_content_change_after_the_canonical_hash_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, b"alpha").unwrap();

        let result = normalize_and_hash_with(RootHandle::open(root.path()).unwrap(), EPOCH, |point| {
            if point == HookPoint::AfterCanonicalHash {
                fs::write(&source, b"bravo").unwrap();
            }
        });

        assert!(matches!(result, Err(Error::TreeChanged)));
        assert_eq!(fs::read(source).unwrap(), b"bravo");
    }

    #[test]
    fn ancestor_symlink_swap_cannot_escape_the_audited_root() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("tree");
        let moved = temporary.path().join("moved-outside");
        let nested = root.join("nested");
        let source = nested.join("source");
        fs::create_dir_all(&nested).unwrap();
        fs::write(&source, b"outside sentinel").unwrap();
        fs::set_permissions(&source, Permissions::from_mode(0o600)).unwrap();
        let old = filetime::FileTime::from_unix_time(123, 0);
        filetime::set_file_times(&source, old, old).unwrap();

        let result = normalize_and_hash_with(RootHandle::open(&root).unwrap(), EPOCH, |point| {
            if point == HookPoint::AfterAudit {
                fs::rename(&nested, &moved).unwrap();
                symlink(&moved, &nested).unwrap();
            }
        });

        assert!(result.is_err());
        let escaped = moved.join("source");
        assert_mode(&escaped, 0o600);
        assert_eq!(fs::metadata(escaped).unwrap().mtime(), 123);
    }

    #[test]
    fn hard_links_reject_the_whole_tree_before_mutation() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("a-original");
        fs::write(&original, b"shared").unwrap();
        fs::set_permissions(&original, Permissions::from_mode(0o600)).unwrap();
        let old = filetime::FileTime::from_unix_time(123, 0);
        filetime::set_file_times(&original, old, old).unwrap();
        fs::hard_link(&original, root.path().join("b-link")).unwrap();

        assert!(matches!(
            normalize_and_hash(root.path(), EPOCH),
            Err(Error::UnexpectedLinkCount { links: 2, .. })
        ));
        assert_mode(&original, 0o600);
        assert_eq!(fs::metadata(&original).unwrap().mtime(), 123);
    }

    #[test]
    fn fifos_and_sockets_are_rejected_before_mutation() {
        let fifo_root = tempfile::tempdir().unwrap();
        let fifo_sentinel = fifo_root.path().join("a-sentinel");
        fs::write(&fifo_sentinel, b"sentinel").unwrap();
        fs::set_permissions(&fifo_sentinel, Permissions::from_mode(0o600)).unwrap();
        mkfifo(&fifo_root.path().join("z-fifo"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        assert!(matches!(
            normalize_and_hash(fifo_root.path(), EPOCH),
            Err(Error::UnsupportedFileType { kind: "FIFO", .. })
        ));
        assert_mode(&fifo_sentinel, 0o600);

        let socket_root = tempfile::tempdir().unwrap();
        let socket_sentinel = socket_root.path().join("a-sentinel");
        fs::write(&socket_sentinel, b"sentinel").unwrap();
        fs::set_permissions(&socket_sentinel, Permissions::from_mode(0o600)).unwrap();
        let _listener = UnixListener::bind(socket_root.path().join("z-socket")).unwrap();
        assert!(matches!(
            normalize_and_hash(socket_root.path(), EPOCH),
            Err(Error::UnsupportedFileType { kind: "socket", .. })
        ));
        assert_mode(&socket_sentinel, 0o600);
    }

    #[test]
    fn symlinks_are_hashed_and_timestamped_without_following_targets() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("tree");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::write(&outside, b"outside").unwrap();
        fs::set_permissions(&outside, Permissions::from_mode(0o600)).unwrap();
        let old = filetime::FileTime::from_unix_time(123, 0);
        filetime::set_file_times(&outside, old, old).unwrap();
        symlink("../outside", root.join("link")).unwrap();

        normalize_and_hash(&root, EPOCH).unwrap();

        assert!(
            fs::symlink_metadata(root.join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_timestamp(&root.join("link"), EPOCH);
        assert_mode(&outside, 0o600);
        assert_eq!(fs::metadata(&outside).unwrap().mtime(), 123);
    }

    fn create_equivalent_tree(root: &Path, raw_name: &OsString, reverse: bool, timestamp: i64) {
        fs::create_dir(root.join("nested")).unwrap();
        let files = if reverse {
            vec![
                (PathBuf::from("executable"), b"execute".as_slice()),
                (PathBuf::from(raw_name), b"raw".as_slice()),
            ]
        } else {
            vec![
                (PathBuf::from(raw_name), b"raw".as_slice()),
                (PathBuf::from("executable"), b"execute".as_slice()),
            ]
        };
        for (path, bytes) in files {
            fs::write(root.join(path), bytes).unwrap();
        }
        symlink(raw_name, root.join("link")).unwrap();

        fs::set_permissions(root, Permissions::from_mode(if reverse { 0o777 } else { 0o700 })).unwrap();
        fs::set_permissions(
            root.join("nested"),
            Permissions::from_mode(if reverse { 0o775 } else { 0o700 }),
        )
        .unwrap();
        fs::set_permissions(
            root.join(raw_name),
            Permissions::from_mode(if reverse { 0o664 } else { 0o600 }),
        )
        .unwrap();
        fs::set_permissions(
            root.join("executable"),
            Permissions::from_mode(if reverse { 0o777 } else { 0o711 }),
        )
        .unwrap();

        let old = filetime::FileTime::from_unix_time(timestamp, 0);
        for path in [
            root.to_owned(),
            root.join("nested"),
            root.join(raw_name),
            root.join("executable"),
        ] {
            filetime::set_file_times(path, old, old).unwrap();
        }
        filetime::set_symlink_file_times(root.join("link"), old, old).unwrap();
    }

    fn digest_with(mutate: impl FnOnce(&Path)) -> String {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("regular"), b"alpha").unwrap();
        fs::write(root.path().join("executable"), b"execute").unwrap();
        fs::set_permissions(root.path().join("executable"), Permissions::from_mode(0o755)).unwrap();
        fs::write(root.path().join("kind"), b"kind").unwrap();
        symlink("regular", root.path().join("link")).unwrap();
        mutate(root.path());
        normalize_and_hash(root.path(), EPOCH).unwrap()
    }

    fn assert_mode(path: &Path, expected: u32) {
        assert_eq!(fs::symlink_metadata(path).unwrap().mode() & 0o7777, expected);
    }

    fn assert_timestamp(path: &Path, expected: i64) {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert_eq!(metadata.atime(), expected);
        assert_eq!(metadata.atime_nsec(), 0);
        assert_eq!(metadata.mtime(), expected);
        assert_eq!(metadata.mtime_nsec(), 0);
    }
}
