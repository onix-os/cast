use std::{
    ffi::{CStr, CString},
    io,
    os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
};

use super::super::super::{openat2_file_until, retry_interrupted};
use super::super::filesystem::{MountNamespaceLimits, Operation, TaskRootWitness, mounted_directory_witness};

pub(super) type DirectoryWitness = TaskRootWitness;
pub(super) type AttachmentLimits = MountNamespaceLimits;

// Mount crossings are intentional: an ESP or XBOOTLDR destination is normally
// a different mount below the task root. Do not add RESOLVE_NO_XDEV here.
const ATTACHMENT_RESOLUTION: u64 =
    (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64;
const _: () = assert!(ATTACHMENT_RESOLUTION & nix::libc::RESOLVE_NO_XDEV as u64 == 0);

pub(super) fn duplicate_directory(
    directory: &std::fs::File,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<std::fs::File> {
    operation.charge_descriptor(action)?;
    let descriptor = retry_interrupted(Some(operation.deadline()), || {
        // SAFETY: F_DUPFD_CLOEXEC duplicates the live retained descriptor and
        // returns a new descriptor owned by the caller on success.
        let result = unsafe { nix::libc::fcntl(directory.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
        if result >= 0 {
            Ok(result)
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    // SAFETY: successful F_DUPFD_CLOEXEC returned one fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    operation.checkpoint()?;
    Ok(std::fs::File::from(descriptor))
}

pub(super) fn open_directory_component(
    parent: &std::fs::File,
    name: &CStr,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<std::fs::File> {
    require_component(name, action)?;
    operation.charge_descriptor(action)?;
    openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        ATTACHMENT_RESOLUTION,
        operation.deadline(),
    )
}

pub(super) fn directory_witness(
    directory: &std::fs::File,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<DirectoryWitness> {
    mounted_directory_witness(directory, operation, action)
}

pub(super) fn require_same_directory(
    expected: DirectoryWitness,
    actual: DirectoryWitness,
    context: &'static str,
) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{context} changed device, inode, kind, or mount ID"),
        ))
    }
}

pub(super) fn copy_component(component: &CStr, operation: &mut Operation<'_>) -> io::Result<CString> {
    require_component(component, "copying attachment component")?;
    operation.charge(
        component.to_bytes().len().saturating_add(1),
        "copying attachment component",
    )?;
    CString::new(component.to_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "validated attachment component contains NUL",
        )
    })
}

fn require_component(component: &CStr, context: &'static str) -> io::Result<()> {
    let bytes = component.to_bytes();
    if bytes.is_empty() || bytes.len() > 255 || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{context} is not one bounded raw path component"),
        ))
    } else {
        Ok(())
    }
}
