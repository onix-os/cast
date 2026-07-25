use std::{
    ffi::{CStr, CString, OsStr, OsString},
    fs::Metadata,
    io::{self, Read as _},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::{
            ffi::{OsStrExt as _, OsStringExt as _},
            fs::MetadataExt as _,
        },
    },
    path::{Path, PathBuf},
};

use fs_err as fs;
use fs_err::os::unix::fs::OpenOptionsExt as _;

#[derive(Debug)]
pub(crate) struct ManagedDirectory {
    path: PathBuf,
    file: fs::File,
    identity: NodeIdentity,
}

impl ManagedDirectory {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "managed declaration path is not a real directory",
            ));
        }
        Ok(Self {
            path: path.to_owned(),
            file,
            identity: NodeIdentity::from_metadata(&metadata),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn metadata(&self) -> io::Result<Metadata> {
        self.file.metadata()
    }

    pub(crate) fn verify_path(&self) -> io::Result<()> {
        let metadata = fs::symlink_metadata(&self.path)?;
        if metadata.file_type().is_dir() && NodeIdentity::from_metadata(&metadata) == self.identity {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "managed declaration directory changed while it was being managed",
            ))
        }
    }

    pub(crate) fn entry_names(&self, limit: usize) -> io::Result<BoundedDirectoryEntries> {
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

    pub(crate) fn metadata_at(&self, name: &OsStr) -> io::Result<Metadata> {
        self.open_at(name, libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW, 0)?
            .metadata()
    }

    pub(crate) fn open_at(&self, name: &OsStr, flags: i32, mode: libc::mode_t) -> io::Result<fs::File> {
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

    pub(crate) fn rename(&self, from: &OsStr, to: &OsStr, no_replace: bool) -> io::Result<()> {
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

    pub(crate) fn unlink(&self, name: &OsStr) -> io::Result<()> {
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

    pub(crate) fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    pub(crate) fn display_path(&self, name: &OsStr) -> PathBuf {
        self.path.join(name)
    }
}

pub(crate) enum BoundedDirectoryEntries {
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
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "declaration name contains a NUL byte"))
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
pub(crate) struct FileSnapshot {
    node: NodeIdentity,
    size: u64,
    ctime: i64,
    ctime_nsec: i64,
}

impl FileSnapshot {
    pub(crate) fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            node: NodeIdentity::from_metadata(metadata),
            size: metadata.size(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExistingDeclaration {
    identity: FileSnapshot,
    generated: bool,
}

impl ExistingDeclaration {
    pub(crate) const fn identity(&self) -> FileSnapshot {
        self.identity
    }

    pub(crate) const fn is_generated(&self) -> bool {
        self.generated
    }
}

pub(crate) fn inspect_existing_declaration(
    directory: &ManagedDirectory,
    name: &OsStr,
    limit: usize,
    marker: &[u8],
) -> io::Result<Option<ExistingDeclaration>> {
    inspect_existing_declaration_markers(directory, name, limit, &[marker])
}

/// Inspect one retained-directory entry against every ownership marker that
/// can legitimately own it.
///
/// Public declaration paths have one marker selected by their extension. A
/// hidden authority-switch residue has no extension, so recovery must accept
/// the marker of any registered authority without reading the file more than
/// once or weakening the exact-inode snapshot.
pub(crate) fn inspect_existing_declaration_markers(
    directory: &ManagedDirectory,
    name: &OsStr,
    limit: usize,
    markers: &[&[u8]],
) -> io::Result<Option<ExistingDeclaration>> {
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
            "managed declaration is not a real regular file",
        ));
    }
    if metadata.len() > u64::try_from(limit).unwrap_or(u64::MAX) {
        return Err(declaration_too_large(metadata.len(), limit));
    }

    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(limit).min(limit));
    io::Read::by_ref(&mut file)
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(declaration_too_large(
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
            "managed declaration changed while its marker was being read",
        ));
    }
    Ok(Some(ExistingDeclaration {
        identity: final_snapshot,
        generated: markers.iter().any(|marker| bytes.starts_with(marker)),
    }))
}

fn declaration_too_large(size: u64, limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("managed declaration is {size} bytes; limit is {limit} bytes"),
    )
}
