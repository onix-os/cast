use std::{
    collections::HashSet,
    ffi::CStr,
    fs::File,
    io,
    os::fd::{AsRawFd as _, IntoRawFd as _},
    path::Path,
    ptr::NonNull,
};

use crate::linux_fs::{
    controlled_resolution, openat2_file_until, require_no_access_acl_until, require_no_default_acl_until,
};

use super::{
    CandidateInventoryError, WorkBudget,
    error::inventory_io,
    inventory::{MARKER_NAME, MetadataWitness, NamespaceCounter},
};

pub(super) fn directory_names(
    directory: &File,
    path: &Path,
    child_depth: usize,
    marker_exempt: bool,
    counter: &mut NamespaceCounter,
    budget: &mut WorkBudget,
) -> Result<Vec<Vec<u8>>, CandidateInventoryError> {
    budget.operation(path)?;
    let duplicate = open_relative(
        directory,
        c".",
        directory_read_flags(),
        path,
        "open candidate directory cursor",
        budget,
    )?;
    let descriptor = duplicate.into_raw_fd();
    // SAFETY: fdopendir consumes the fresh descriptor on success.
    let stream = unsafe { nix::libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume the descriptor.
        unsafe { nix::libc::close(descriptor) };
        return Err(inventory_io("enumerate candidate directory", path, source));
    };
    let mut stream = DirectoryStream(Some(stream));
    let mut names = Vec::new();
    loop {
        budget.operation(path)?;
        // SAFETY: errno is thread-local and readdir uses null for EOF/error.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: the stream is live and exclusively borrowed here.
        let entry = unsafe { nix::libc::readdir(stream.pointer().as_ptr()) };
        if entry.is_null() {
            let errno = unsafe { *nix::libc::__errno_location() };
            if errno == 0 {
                break;
            }
            if errno == nix::libc::EINTR {
                continue;
            }
            return Err(inventory_io(
                "enumerate candidate directory",
                path,
                io::Error::from_raw_os_error(errno),
            ));
        }
        // SAFETY: d_name is NUL terminated for the returned live dirent.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if !(marker_exempt && name == MARKER_NAME.to_bytes()) {
            counter.entry(child_depth, name, path)?;
        }
        reserve(&mut names, 1, "directory name", path)?;
        names.push(clone_bytes(name, "directory name bytes", path)?);
    }
    budget.operation(path)?;
    stream
        .close()
        .map_err(|source| inventory_io("close candidate directory cursor", path, source))?;
    budget.operation(path)?;
    names.sort_unstable();
    budget.check(path)?;
    for pair in names.windows(2) {
        budget.operation(path)?;
        if pair[0] == pair[1] {
            return Err(CandidateInventoryError::ChildNamesChanged { path: path.to_owned() });
        }
    }
    budget.check(path)?;
    Ok(names)
}

pub(super) fn require_regular_acl(
    file: &File,
    path: &Path,
    budget: &mut WorkBudget,
) -> Result<(), CandidateInventoryError> {
    budget.operation(path)?;
    require_no_access_acl_until(file, path, budget.deadline())
        .map_err(|source| inventory_io("reject POSIX access ACL on regular file", path, source))?;
    budget.check(path)?;
    require_no_extended_attributes(file, path, budget)
}

pub(super) fn require_directory_acls(
    file: &File,
    path: &Path,
    budget: &mut WorkBudget,
) -> Result<(), CandidateInventoryError> {
    budget.operation(path)?;
    require_no_access_acl_until(file, path, budget.deadline())
        .map_err(|source| inventory_io("reject POSIX access ACL on directory", path, source))?;
    budget.operation(path)?;
    require_no_default_acl_until(file, path, budget.deadline())
        .map_err(|source| inventory_io("reject POSIX default ACL on directory", path, source))?;
    budget.check(path)?;
    require_no_extended_attributes(file, path, budget)
}

/// Reject every extended attribute on an exact readable inode.
///
/// ACL probes run first for regular files and directories so those retain
/// their established, more specific errors. Linux aliases `ENOTSUP` to
/// `EOPNOTSUPP`; a filesystem without xattr support therefore remains valid.
pub(super) fn require_no_extended_attributes(
    file: &File,
    path: &Path,
    budget: &mut WorkBudget,
) -> Result<(), CandidateInventoryError> {
    loop {
        budget.operation(path)?;
        // SAFETY: `file` is a live readable descriptor. A null list with size
        // zero is the documented flistxattr size probe and writes no bytes.
        let result = unsafe { nix::libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0) };
        if result >= 0 {
            budget.check(path)?;
            let name_bytes = usize::try_from(result).expect("nonnegative flistxattr length fits usize");
            return if name_bytes == 0 {
                Ok(())
            } else {
                Err(CandidateInventoryError::ExtendedAttributes {
                    path: path.to_owned(),
                    name_bytes,
                })
            };
        }

        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if source.raw_os_error() == Some(nix::libc::EOPNOTSUPP) {
            budget.check(path)?;
            return Ok(());
        }
        return Err(inventory_io("reject extended attributes", path, source));
    }
}

pub(super) fn directory_read_flags() -> i32 {
    nix::libc::O_RDONLY
        | nix::libc::O_DIRECTORY
        | nix::libc::O_CLOEXEC
        | nix::libc::O_NOFOLLOW
        | nix::libc::O_NONBLOCK
        | nix::libc::O_NOATIME
}

pub(super) fn regular_read_flags() -> i32 {
    nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_NOATIME
}

pub(super) fn open_relative(
    directory: &File,
    name: &CStr,
    flags: i32,
    path: &Path,
    operation: &'static str,
    budget: &mut WorkBudget,
) -> Result<File, CandidateInventoryError> {
    open_raw_fd_relative(directory.as_raw_fd(), name, flags, path, operation, budget)
}

pub(super) fn open_raw_fd_relative(
    directory: i32,
    name: &CStr,
    flags: i32,
    path: &Path,
    operation: &'static str,
    budget: &mut WorkBudget,
) -> Result<File, CandidateInventoryError> {
    budget.operation(path)?;
    match openat2_file_until(directory, name, flags, 0, controlled_resolution(), budget.deadline()) {
        Ok(file) => {
            budget.check(path)?;
            Ok(file)
        }
        Err(source) if source.raw_os_error() == Some(nix::libc::EXDEV) => {
            Err(CandidateInventoryError::MountedEntry { path: path.to_owned() })
        }
        Err(source) => Err(inventory_io(operation, path, source)),
    }
}

pub(super) fn require_effective_owner(metadata: MetadataWitness, path: &Path) -> Result<(), CandidateInventoryError> {
    // SAFETY: geteuid has no arguments and cannot fail.
    let expected = unsafe { nix::libc::geteuid() };
    if metadata.owner == expected {
        Ok(())
    } else {
        Err(CandidateInventoryError::UnexpectedOwner {
            path: path.to_owned(),
            owner: metadata.owner,
            expected,
        })
    }
}

pub(super) fn reserve<T>(
    values: &mut Vec<T>,
    additional: usize,
    resource: &'static str,
    path: &Path,
) -> Result<(), CandidateInventoryError> {
    values
        .try_reserve(additional)
        .map_err(|_| CandidateInventoryError::Allocation {
            resource,
            path: path.to_owned(),
        })
}

pub(super) fn reserve_set(
    values: &mut HashSet<(u64, u64)>,
    additional: usize,
    path: &Path,
) -> Result<(), CandidateInventoryError> {
    values
        .try_reserve(additional)
        .map_err(|_| CandidateInventoryError::Allocation {
            resource: "inode-identity set",
            path: path.to_owned(),
        })
}

pub(super) fn clone_bytes(
    bytes: &[u8],
    resource: &'static str,
    path: &Path,
) -> Result<Vec<u8>, CandidateInventoryError> {
    let mut cloned = Vec::new();
    cloned
        .try_reserve_exact(bytes.len())
        .map_err(|_| CandidateInventoryError::Allocation {
            resource,
            path: path.to_owned(),
        })?;
    cloned.extend_from_slice(bytes);
    Ok(cloned)
}

pub(super) fn clone_nul_terminated_bytes(
    bytes: &[u8],
    resource: &'static str,
    path: &Path,
) -> Result<Vec<u8>, CandidateInventoryError> {
    let capacity = bytes
        .len()
        .checked_add(1)
        .ok_or_else(|| CandidateInventoryError::Allocation {
            resource,
            path: path.to_owned(),
        })?;
    let mut cloned = Vec::new();
    cloned
        .try_reserve_exact(capacity)
        .map_err(|_| CandidateInventoryError::Allocation {
            resource,
            path: path.to_owned(),
        })?;
    cloned.extend_from_slice(bytes);
    cloned.push(0);
    Ok(cloned)
}

struct DirectoryStream(Option<NonNull<nix::libc::DIR>>);

impl DirectoryStream {
    fn pointer(&self) -> NonNull<nix::libc::DIR> {
        self.0.expect("live directory stream")
    }

    fn close(&mut self) -> io::Result<()> {
        let stream = self.0.take().expect("live directory stream");
        // SAFETY: this wrapper uniquely owns the stream.
        if unsafe { nix::libc::closedir(stream.as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        if let Some(stream) = self.0.take() {
            // SAFETY: this wrapper uniquely owns the stream.
            unsafe { nix::libc::closedir(stream.as_ptr()) };
        }
    }
}
