//! Child-local assembly of the private minimal `/dev` tree.
//!
//! The privileged provider owns creation of the three character-device
//! inodes. This module only assembles those already-validated detached file
//! mounts beneath a fresh bounded tmpfs in the container child's private mount
//! namespace. No ambient device pathname is consulted.

use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd, RawFd};
use std::path::Path;

use nc::{AT_EMPTY_PATH, MOVE_MOUNT_F_EMPTY_PATH, SYS_MOUNT_SETATTR, mount_attr_t, move_mount};
use nix::{errno::Errno, libc};
use snafu::Snafu;

use crate::private_devices::{
    PRIVATE_DEVICE_TMPFS_INODES, PRIVATE_DEVICE_TMPFS_SIZE_BYTES, PrivateDevice, PrivateDeviceError,
    PrivateDeviceMounts,
};

const FSOPEN_CLOEXEC: libc::c_uint = 0x0000_0001;
const FSCONFIG_SET_STRING: libc::c_uint = 1;
const FSCONFIG_CMD_CREATE: libc::c_uint = 6;
const FSMOUNT_CLOEXEC: libc::c_uint = 0x0000_0001;
const MOVE_MOUNT_T_EMPTY_PATH: u32 = 0x0000_0040;
const MOUNT_ATTR_RDONLY: u64 = 0x0000_0001;
const TMPFS_MAGIC: libc::c_long = 0x0102_1994;
const PLACEHOLDER_PERMISSIONS: libc::mode_t = 0o600;

/// Consume one validated private-device capability and return one complete,
/// detached minimal `/dev` directory mount.
///
/// The parent tmpfs is sealed read-only without `AT_RECURSIVE`. The three
/// nested private file mounts therefore retain their writable data and
/// metadata semantics, including ordinary existing-file `O_CREAT` opens. On
/// every failure, dropping the detached parent destroys all partial child
/// attachments and dropping `devices` closes every remaining source mount.
pub(crate) fn assemble_private_minimal_dev(
    devices: PrivateDeviceMounts,
) -> Result<OwnedFd, PrivateDeviceAssemblyError> {
    devices
        .validate_namespace_invariants()
        .map_err(|source| PrivateDeviceAssemblyError::ValidateCapability { source })?;

    let dev = detached_bounded_tmpfs()?;
    for (device, source) in devices.ordered() {
        let target = create_exact_placeholder(dev.as_raw_fd(), device)?;
        attach_private_device(source, target.as_raw_fd(), device)?;
    }
    seal_parent_read_only(dev.as_raw_fd())?;
    require_parent_read_only(dev.as_raw_fd())?;
    devices
        .validate_namespace_invariants()
        .map_err(|source| PrivateDeviceAssemblyError::ValidateAttachedDevices { source })?;
    Ok(dev)
}

#[derive(Debug, Snafu)]
pub(crate) enum PrivateDeviceAssemblyError {
    #[snafu(display("validate private minimal-device capability before child assembly"))]
    ValidateCapability { source: PrivateDeviceError },
    #[snafu(display("validate attached private devices after sealing the child /dev parent"))]
    ValidateAttachedDevices { source: PrivateDeviceError },
    #[snafu(display("{operation} for private minimal-device assembly target {target}"))]
    Syscall {
        operation: &'static str,
        target: &'static str,
        source: io::Error,
    },
    #[snafu(display("{operation} returned an invalid descriptor for private minimal-device target {target}"))]
    InvalidDescriptor {
        operation: &'static str,
        target: &'static str,
    },
    #[snafu(display(
        "private minimal /dev tmpfs has filesystem magic {actual:#x}; expected {expected:#x}"
    ))]
    UnexpectedTmpfsFilesystem {
        expected: libc::c_long,
        actual: libc::c_long,
    },
    #[snafu(display("private minimal /dev tmpfs capacity readback overflowed"))]
    InvalidTmpfsCapacity,
    #[snafu(display(
        "private minimal /dev tmpfs has size {actual_size_bytes} and {actual_inodes} inodes; expected exactly {expected_size_bytes} and {expected_inodes}"
    ))]
    UnexpectedTmpfsCapacity {
        expected_size_bytes: u64,
        actual_size_bytes: u64,
        expected_inodes: u64,
        actual_inodes: u64,
    },
    #[snafu(display(
        "private minimal /dev placeholder {device} has type {actual_type:o}, permissions {actual_permissions:o}, size {actual_size}, and {actual_links} links"
    ))]
    InvalidPlaceholder {
        device: PrivateDevice,
        actual_type: libc::mode_t,
        actual_permissions: libc::mode_t,
        actual_size: libc::off_t,
        actual_links: libc::nlink_t,
    },
    #[snafu(display("private minimal /dev parent tmpfs is not read-only after its nonrecursive seal"))]
    ParentNotReadOnly,
}

fn detached_bounded_tmpfs() -> Result<OwnedFd, PrivateDeviceAssemblyError> {
    // SAFETY: the fixed filesystem name is NUL-terminated. Success returns a
    // fresh close-on-exec filesystem-context descriptor.
    let context = unsafe { libc::syscall(libc::SYS_fsopen, c"tmpfs".as_ptr(), FSOPEN_CLOEXEC) };
    let context = owned_syscall_descriptor(context, "fsopen", "dev")?;

    configure_fscontext_string(context.as_raw_fd(), c"size", c"65536", "dev tmpfs size")?;
    configure_fscontext_string(context.as_raw_fd(), c"nr_inodes", c"4", "dev tmpfs inode ceiling")?;

    // SAFETY: CREATE accepts null key/value pointers and borrows only the live
    // context for the duration of this call.
    let configured = unsafe {
        libc::syscall(
            libc::SYS_fsconfig,
            context.as_raw_fd(),
            FSCONFIG_CMD_CREATE,
            std::ptr::null::<libc::c_char>(),
            std::ptr::null::<libc::c_void>(),
            0,
        )
    };
    if configured == -1 {
        return Err(syscall_error("fsconfig(CREATE)", "dev"));
    }

    // SAFETY: the configured context is live. Success returns one fresh,
    // detached, close-on-exec mount descriptor.
    let mount = unsafe { libc::syscall(libc::SYS_fsmount, context.as_raw_fd(), FSMOUNT_CLOEXEC, 0) };
    let mount = owned_syscall_descriptor(mount, "fsmount", "dev")?;
    validate_tmpfs_observation(observe_tmpfs(mount.as_raw_fd())?)?;
    Ok(mount)
}

fn configure_fscontext_string(
    context: RawFd,
    key: &std::ffi::CStr,
    value: &std::ffi::CStr,
    target: &'static str,
) -> Result<(), PrivateDeviceAssemblyError> {
    // SAFETY: context is a live fscontext descriptor and both strings are
    // NUL-terminated and borrowed only for this call.
    let configured = unsafe {
        libc::syscall(
            libc::SYS_fsconfig,
            context,
            FSCONFIG_SET_STRING,
            key.as_ptr(),
            value.as_ptr(),
            0,
        )
    };
    if configured == -1 {
        Err(syscall_error("fsconfig(SET_STRING)", target))
    } else {
        Ok(())
    }
}

fn create_exact_placeholder(
    parent: RawFd,
    device: PrivateDevice,
) -> Result<OwnedFd, PrivateDeviceAssemblyError> {
    // SAFETY: parent is the fresh detached tmpfs root and the fixed name is
    // NUL-terminated. O_EXCL proves no prior object supplied the target.
    let descriptor = unsafe {
        libc::openat(
            parent,
            device.name().as_ptr(),
            libc::O_WRONLY
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
            PLACEHOLDER_PERMISSIONS,
        )
    };
    let descriptor = owned_raw_descriptor(descriptor, "openat(O_CREAT|O_EXCL)", device_target(device))?;

    // The process umask may have narrowed the create mode. Restore the exact
    // setup-only placeholder mode through the already-open regular file.
    if unsafe { libc::fchmod(descriptor.as_raw_fd(), PLACEHOLDER_PERMISSIONS) } == -1 {
        return Err(syscall_error("fchmod", device_target(device)));
    }
    validate_placeholder_observation(device, observe_placeholder(descriptor.as_raw_fd())?)?;

    // Use an O_PATH descriptor as the empty-path move_mount target. The
    // creator descriptor closes before attachment and cannot carry writable
    // file authority beyond this helper.
    drop(descriptor);
    let target = unsafe {
        libc::openat(
            parent,
            device.name().as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )
    };
    owned_raw_descriptor(target, "openat(O_PATH)", device_target(device))
}

fn attach_private_device(
    source: BorrowedFd<'_>,
    target: RawFd,
    device: PrivateDevice,
) -> Result<(), PrivateDeviceAssemblyError> {
    // SAFETY: source is a validated detached file mount, target is the exact
    // O_PATH placeholder in the detached child tmpfs, and both empty paths are
    // admitted explicitly.
    unsafe {
        move_mount(
            source.as_raw_fd(),
            Path::new(""),
            target,
            Path::new(""),
            MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH,
        )
    }
    .map_err(Errno::from_i32)
    .map_err(|source| PrivateDeviceAssemblyError::Syscall {
        operation: "move_mount",
        target: device_target(device),
        source: io::Error::from_raw_os_error(source as i32),
    })?;
    Ok(())
}

fn seal_parent_read_only(descriptor: RawFd) -> Result<(), PrivateDeviceAssemblyError> {
    let attributes = mount_attr_t {
        attr_set: MOUNT_ATTR_RDONLY,
        attr_clr: 0,
        program: 0,
        userns_fd: 0,
    };
    // Deliberately omit the recursive flag: only the tmpfs directory is
    // immutable; its three private file-mount children must remain writable.
    let result = unsafe {
        nc::syscalls::syscall5(
            SYS_MOUNT_SETATTR,
            descriptor as usize,
            c"".as_ptr() as usize,
            AT_EMPTY_PATH as usize,
            &attributes as *const mount_attr_t as usize,
            size_of::<mount_attr_t>(),
        )
    };
    result.map(|_| ()).map_err(|errno| PrivateDeviceAssemblyError::Syscall {
        operation: "mount_setattr(read-only, nonrecursive)",
        target: "dev",
        source: io::Error::from_raw_os_error(errno),
    })
}

fn require_parent_read_only(descriptor: RawFd) -> Result<(), PrivateDeviceAssemblyError> {
    // SAFETY: zero is valid initialization and descriptor remains live for the
    // read-only filesystem-statistics call.
    let mut mount: libc::statvfs = unsafe { zeroed() };
    if unsafe { libc::fstatvfs(descriptor, &mut mount) } == -1 {
        return Err(syscall_error("fstatvfs", "dev"));
    }
    if mount.f_flag & libc::ST_RDONLY == 0 {
        Err(PrivateDeviceAssemblyError::ParentNotReadOnly)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TmpfsObservation {
    filesystem: libc::c_long,
    block_size: libc::c_long,
    blocks: libc::fsblkcnt_t,
    inodes: libc::fsfilcnt_t,
}

fn observe_tmpfs(descriptor: RawFd) -> Result<TmpfsObservation, PrivateDeviceAssemblyError> {
    // SAFETY: zero is valid initialization and descriptor remains live for the
    // read-only filesystem-statistics call.
    let mut filesystem: libc::statfs = unsafe { zeroed() };
    if unsafe { libc::fstatfs(descriptor, &mut filesystem) } == -1 {
        return Err(syscall_error("fstatfs", "dev"));
    }
    Ok(TmpfsObservation {
        filesystem: filesystem.f_type,
        block_size: filesystem.f_bsize,
        blocks: filesystem.f_blocks,
        inodes: filesystem.f_files,
    })
}

fn validate_tmpfs_observation(observation: TmpfsObservation) -> Result<(), PrivateDeviceAssemblyError> {
    if observation.filesystem != TMPFS_MAGIC {
        return Err(PrivateDeviceAssemblyError::UnexpectedTmpfsFilesystem {
            expected: TMPFS_MAGIC,
            actual: observation.filesystem,
        });
    }
    let block_size = u64::try_from(observation.block_size)
        .map_err(|_| PrivateDeviceAssemblyError::InvalidTmpfsCapacity)?;
    let blocks = u64::try_from(observation.blocks).map_err(|_| PrivateDeviceAssemblyError::InvalidTmpfsCapacity)?;
    let size_bytes = block_size
        .checked_mul(blocks)
        .ok_or(PrivateDeviceAssemblyError::InvalidTmpfsCapacity)?;
    let inodes = u64::try_from(observation.inodes).map_err(|_| PrivateDeviceAssemblyError::InvalidTmpfsCapacity)?;
    if (size_bytes, inodes) != (PRIVATE_DEVICE_TMPFS_SIZE_BYTES, PRIVATE_DEVICE_TMPFS_INODES) {
        return Err(PrivateDeviceAssemblyError::UnexpectedTmpfsCapacity {
            expected_size_bytes: PRIVATE_DEVICE_TMPFS_SIZE_BYTES,
            actual_size_bytes: size_bytes,
            expected_inodes: PRIVATE_DEVICE_TMPFS_INODES,
            actual_inodes: inodes,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlaceholderObservation {
    file_type: libc::mode_t,
    permissions: libc::mode_t,
    size: libc::off_t,
    links: libc::nlink_t,
}

fn observe_placeholder(descriptor: RawFd) -> Result<PlaceholderObservation, PrivateDeviceAssemblyError> {
    // SAFETY: zero is valid initialization and descriptor remains live for the
    // read-only metadata call.
    let mut stat: libc::stat = unsafe { zeroed() };
    if unsafe { libc::fstat(descriptor, &mut stat) } == -1 {
        return Err(syscall_error("fstat", "dev placeholder"));
    }
    Ok(PlaceholderObservation {
        file_type: stat.st_mode & libc::S_IFMT,
        permissions: stat.st_mode & 0o7777,
        size: stat.st_size,
        links: stat.st_nlink,
    })
}

fn validate_placeholder_observation(
    device: PrivateDevice,
    observation: PlaceholderObservation,
) -> Result<(), PrivateDeviceAssemblyError> {
    if observation
        == (PlaceholderObservation {
            file_type: libc::S_IFREG,
            permissions: PLACEHOLDER_PERMISSIONS,
            size: 0,
            links: 1,
        })
    {
        Ok(())
    } else {
        Err(PrivateDeviceAssemblyError::InvalidPlaceholder {
            device,
            actual_type: observation.file_type,
            actual_permissions: observation.permissions,
            actual_size: observation.size,
            actual_links: observation.links,
        })
    }
}

fn owned_syscall_descriptor(
    result: libc::c_long,
    operation: &'static str,
    target: &'static str,
) -> Result<OwnedFd, PrivateDeviceAssemblyError> {
    if result == -1 {
        return Err(syscall_error(operation, target));
    }
    let descriptor = RawFd::try_from(result)
        .map_err(|_| PrivateDeviceAssemblyError::InvalidDescriptor { operation, target })?;
    // SAFETY: the successful syscall returned one fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn owned_raw_descriptor(
    result: RawFd,
    operation: &'static str,
    target: &'static str,
) -> Result<OwnedFd, PrivateDeviceAssemblyError> {
    if result == -1 {
        return Err(syscall_error(operation, target));
    }
    // SAFETY: the successful syscall returned one fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(result) })
}

fn syscall_error(operation: &'static str, target: &'static str) -> PrivateDeviceAssemblyError {
    PrivateDeviceAssemblyError::Syscall {
        operation,
        target,
        source: io::Error::last_os_error(),
    }
}

const fn device_target(device: PrivateDevice) -> &'static str {
    match device {
        PrivateDevice::Null => "dev/null",
        PrivateDevice::Zero => "dev/zero",
        PrivateDevice::Full => "dev/full",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXACT_TMPFS: TmpfsObservation = TmpfsObservation {
        filesystem: TMPFS_MAGIC,
        block_size: 4096,
        blocks: PRIVATE_DEVICE_TMPFS_SIZE_BYTES / 4096,
        inodes: PRIVATE_DEVICE_TMPFS_INODES,
    };

    const EXACT_PLACEHOLDER: PlaceholderObservation = PlaceholderObservation {
        file_type: libc::S_IFREG,
        permissions: PLACEHOLDER_PERMISSIONS,
        size: 0,
        links: 1,
    };

    #[test]
    fn child_dev_tmpfs_contract_is_exact_and_bounded() {
        assert_eq!(PRIVATE_DEVICE_TMPFS_SIZE_BYTES, 65_536);
        assert_eq!(PRIVATE_DEVICE_TMPFS_INODES, 4);
        validate_tmpfs_observation(EXACT_TMPFS).unwrap();
    }

    #[test]
    fn child_dev_tmpfs_rejects_wrong_filesystem_and_capacity() {
        let mut wrong_filesystem = EXACT_TMPFS;
        wrong_filesystem.filesystem = 0;
        assert!(matches!(
            validate_tmpfs_observation(wrong_filesystem),
            Err(PrivateDeviceAssemblyError::UnexpectedTmpfsFilesystem { .. })
        ));

        for observation in [
            TmpfsObservation {
                blocks: EXACT_TMPFS.blocks - 1,
                ..EXACT_TMPFS
            },
            TmpfsObservation {
                inodes: EXACT_TMPFS.inodes - 1,
                ..EXACT_TMPFS
            },
        ] {
            assert!(matches!(
                validate_tmpfs_observation(observation),
                Err(PrivateDeviceAssemblyError::UnexpectedTmpfsCapacity { .. })
            ));
        }
    }

    #[test]
    fn child_dev_placeholder_contract_is_exact_regular_empty_and_single_linked() {
        for device in [PrivateDevice::Null, PrivateDevice::Zero, PrivateDevice::Full] {
            validate_placeholder_observation(device, EXACT_PLACEHOLDER).unwrap();
        }
    }

    #[test]
    fn child_dev_placeholders_fail_closed_on_each_metadata_dimension() {
        for observation in [
            PlaceholderObservation {
                file_type: libc::S_IFIFO,
                ..EXACT_PLACEHOLDER
            },
            PlaceholderObservation {
                permissions: 0o666,
                ..EXACT_PLACEHOLDER
            },
            PlaceholderObservation {
                size: 1,
                ..EXACT_PLACEHOLDER
            },
            PlaceholderObservation {
                links: 2,
                ..EXACT_PLACEHOLDER
            },
        ] {
            assert!(matches!(
                validate_placeholder_observation(PrivateDevice::Null, observation),
                Err(PrivateDeviceAssemblyError::InvalidPlaceholder {
                    device: PrivateDevice::Null,
                    ..
                })
            ));
        }
    }

    #[test]
    fn private_device_targets_are_fixed_and_never_path_inputs() {
        assert_eq!(
            [PrivateDevice::Null, PrivateDevice::Zero, PrivateDevice::Full].map(device_target),
            ["dev/null", "dev/zero", "dev/full"]
        );
    }
}
