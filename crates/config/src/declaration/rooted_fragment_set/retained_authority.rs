use std::{
    ffi::{CStr, CString, OsString},
    fs::Metadata,
    io,
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt as _, OsStringExt as _},
            fs::MetadataExt as _,
        },
    },
    path::{Path, PathBuf},
};

use fs_err::File;

const MAX_INTERRUPTED_OPEN_RETRIES: usize = 1_024;
const ROOTED_RESOLUTION: u64 = libc::RESOLVE_BENEATH
    | libc::RESOLVE_NO_MAGICLINKS
    | libc::RESOLVE_NO_SYMLINKS
    | libc::RESOLVE_NO_XDEV;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NodeSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    owner: u32,
    group: u32,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl NodeSnapshot {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            owner: metadata.uid(),
            group: metadata.gid(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RetainedNodeKind {
    Directory,
    RegularFile,
}

#[derive(Debug)]
pub(super) struct RetainedRoot {
    path: PathBuf,
    descriptor: File,
    expected: NodeSnapshot,
}

impl RetainedRoot {
    pub(super) fn duplicate(path: &Path, descriptor: RawFd) -> io::Result<Self> {
        let retained = openat2_file(
            descriptor,
            Path::new("."),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            path,
        )?;
        let metadata = retained.metadata()?;
        if !metadata.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "retained declaration root is not a directory",
            ));
        }
        Ok(Self {
            path: path.to_owned(),
            descriptor: retained,
            expected: NodeSnapshot::from_metadata(&metadata),
        })
    }

    pub(super) fn descriptor(&self) -> &File {
        &self.descriptor
    }

    pub(super) fn open_optional(
        &self,
        relative_path: &Path,
        flags: i32,
        diagnostic_path: &Path,
    ) -> io::Result<Option<File>> {
        match openat2_file(
            self.descriptor.as_raw_fd(),
            relative_path,
            flags,
            diagnostic_path,
        ) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(super) fn verify_descriptor(&self) -> io::Result<()> {
        require_snapshot(&self.descriptor, self.expected, &self.path, RetainedNodeKind::Directory)
    }

    fn verify_named(&self, node: &RetainedNode) -> io::Result<()> {
        let flags = match node.kind {
            RetainedNodeKind::Directory => {
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW
            }
            RetainedNodeKind::RegularFile => libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        };
        let named = openat2_file(
            self.descriptor.as_raw_fd(),
            &node.relative_path,
            flags,
            &node.path,
        )?;
        require_snapshot(&named, node.expected, &node.path, node.kind)
    }
}

#[derive(Debug)]
pub(super) struct RetainedNode {
    path: PathBuf,
    relative_path: PathBuf,
    descriptor: File,
    expected: NodeSnapshot,
    kind: RetainedNodeKind,
}

impl RetainedNode {
    pub(super) fn from_opened(
        path: PathBuf,
        relative_path: PathBuf,
        descriptor: File,
        kind: RetainedNodeKind,
    ) -> io::Result<Self> {
        let metadata = descriptor.metadata()?;
        let valid_kind = match kind {
            RetainedNodeKind::Directory => metadata.file_type().is_dir(),
            RetainedNodeKind::RegularFile => metadata.file_type().is_file(),
        };
        if !valid_kind {
            let message = match kind {
                RetainedNodeKind::Directory => {
                    "retained declaration collection is not a directory"
                }
                RetainedNodeKind::RegularFile => {
                    "retained declaration candidate is not a regular file"
                }
            };
            return Err(io::Error::new(io::ErrorKind::InvalidData, message));
        }
        Ok(Self {
            path,
            relative_path,
            descriptor,
            expected: NodeSnapshot::from_metadata(&metadata),
            kind,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn verify_beneath(&self, root: &RetainedRoot) -> io::Result<()> {
        require_snapshot(&self.descriptor, self.expected, &self.path, self.kind)?;
        root.verify_named(self)
    }

    pub(super) fn entry_names(&self, limit: usize) -> io::Result<BoundedDirectoryEntries> {
        if self.kind != RetainedNodeKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot enumerate a retained non-directory declaration node",
            ));
        }
        let duplicate = openat2_file(
            self.descriptor.as_raw_fd(),
            Path::new("."),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK,
            &self.path,
        )?;
        let descriptor: OwnedFd = duplicate.into();
        let raw_descriptor = descriptor.into_raw_fd();
        // SAFETY: `raw_descriptor` is freshly owned. `fdopendir` consumes it
        // on success, while the failure branch closes it exactly once.
        let stream = unsafe { libc::fdopendir(raw_descriptor) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: failed `fdopendir` did not consume the descriptor.
            unsafe {
                libc::close(raw_descriptor);
            }
            return Err(error);
        }
        let stream = DirectoryStream(stream);
        let mut names = Vec::new();
        loop {
            // SAFETY: errno is thread-local and this stream is private.
            unsafe {
                *libc::__errno_location() = 0;
            }
            // SAFETY: the stream remains live and exclusively accessed.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                // SAFETY: errno was cleared immediately before `readdir`.
                let errno = unsafe { *libc::__errno_location() };
                if errno == 0 {
                    break;
                }
                return Err(io::Error::from_raw_os_error(errno));
            }
            // SAFETY: `readdir` returned a live NUL-terminated name.
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
}

pub(super) enum BoundedDirectoryEntries {
    Complete(Vec<OsString>),
    LimitExceeded,
}

fn require_snapshot(
    descriptor: &File,
    expected: NodeSnapshot,
    path: &Path,
    kind: RetainedNodeKind,
) -> io::Result<()> {
    let metadata = descriptor.metadata()?;
    let valid_kind = match kind {
        RetainedNodeKind::Directory => metadata.file_type().is_dir(),
        RetainedNodeKind::RegularFile => metadata.file_type().is_file(),
    };
    if valid_kind && NodeSnapshot::from_metadata(&metadata) == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("retained declaration node changed at {}", path.display()),
        ))
    }
}

fn openat2_file(
    directory: RawFd,
    relative_path: &Path,
    flags: i32,
    diagnostic_path: &Path,
) -> io::Result<File> {
    let path = CString::new(relative_path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "rooted declaration path contains a NUL byte",
        )
    })?;
    // SAFETY: every `open_how` field accepts zero before assignment.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.resolve = ROOTED_RESOLUTION;
    let mut interruptions = 0usize;
    loop {
        // SAFETY: the path and initialized `open_how` remain live. Success
        // returns one fresh descriptor owned by this function.
        let descriptor = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                directory,
                path.as_ptr(),
                &how,
                size_of::<libc::open_how>(),
            )
        };
        if descriptor != -1 {
            // SAFETY: `openat2` returned a fresh owned descriptor.
            let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor as RawFd) };
            return Ok(File::from_parts(descriptor.into(), diagnostic_path));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        if interruptions == MAX_INTERRUPTED_OPEN_RETRIES {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                format!(
                    "rooted declaration open exceeded {MAX_INTERRUPTED_OPEN_RETRIES} interrupted retries at {}",
                    diagnostic_path.display()
                ),
            ));
        }
        interruptions += 1;
    }
}

struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: `fdopendir` created this stream and it is closed once here.
        unsafe {
            libc::closedir(self.0);
        }
    }
}
