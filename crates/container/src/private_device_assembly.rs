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
const PARENT_PERMISSIONS: libc::mode_t = 0o755;
const PLACEHOLDER_PERMISSIONS: libc::mode_t = 0o600;

/// Linear child-side authority to attach and finish one private minimal
/// `/dev` tree.
///
/// The bounded parent remains detached only until
/// [`Self::attach_to_authenticated_target`] moves it onto the already-pinned
/// final `/dev` directory. Creating placeholders after that attachment keeps
/// the lifecycle simple: every partial result is final-target setup state in
/// the single disposable child namespace, never a second staging topology.
#[derive(Debug)]
pub(crate) struct PreparedPrivateDev {
    parent: OwnedFd,
    devices: PrivateDeviceMounts,
}

/// Validate one private-device capability and prepare its bounded parent.
///
/// This function deliberately creates no placeholder below the detached
/// parent. The kernel ENOENT found during VM validation was caused by moving
/// an unlinked source mount, not by a detached target; keeping construction at
/// the final target is an explicit lifecycle choice rather than that diagnosis.
pub(crate) fn prepare_private_minimal_dev(
    devices: PrivateDeviceMounts,
) -> Result<PreparedPrivateDev, PrivateDeviceAssemblyError> {
    devices
        .validate_namespace_invariants()
        .map_err(|source| PrivateDeviceAssemblyError::ValidateCapability { source })?;

    Ok(PreparedPrivateDev {
        parent: detached_bounded_tmpfs()?,
        devices,
    })
}

impl PreparedPrivateDev {
    /// Attach the bounded parent to an authenticated final `/dev` directory,
    /// then populate and seal it before any payload code can run.
    ///
    /// A failure after the parent attachment leaves state only in the single
    /// trusted setup child's private mount namespace. The child exits without
    /// invoking the payload, and namespace teardown reclaims the partial tree.
    pub(crate) fn attach_to_authenticated_target(
        self,
        target: BorrowedFd<'_>,
    ) -> Result<(), PrivateDeviceAssemblyError> {
        // SAFETY: parent is the fresh detached bounded tmpfs, target is an
        // authenticated O_PATH directory in the current child mount
        // namespace, and both empty paths are admitted explicitly.
        unsafe {
            move_mount(
                self.parent.as_raw_fd(),
                Path::new(""),
                target.as_raw_fd(),
                Path::new(""),
                MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH,
            )
        }
        .map_err(Errno::from_i32)
        .map_err(|source| PrivateDeviceAssemblyError::Syscall {
            operation: "move_mount parent onto authenticated final target",
            target: "dev",
            source: io::Error::from_raw_os_error(source as i32),
        })?;

        self.finish_attached()
    }

    fn finish_attached(self) -> Result<(), PrivateDeviceAssemblyError> {
        for (device, source) in self.devices.ordered() {
            create_exact_placeholder(self.parent.as_raw_fd(), device)?;
            attach_private_device(source, self.parent.as_raw_fd(), device)?;
        }
        require_no_parent_inode_headroom(self.parent.as_raw_fd())?;
        seal_parent_read_only(self.parent.as_raw_fd())?;
        require_parent_read_only(self.parent.as_raw_fd())?;
        validate_tmpfs_observation(observe_tmpfs(self.parent.as_raw_fd())?)?;
        self.devices
            .validate_namespace_invariants()
            .map_err(|source| PrivateDeviceAssemblyError::ValidateAttachedDevices { source })?;
        Ok(())
    }
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
        "private minimal /dev tmpfs root has type {actual_type:o}, permissions {actual_permissions:o}, and owner {actual_uid}:{actual_gid}; expected a 0755 directory owned by child root"
    ))]
    UnexpectedTmpfsRoot {
        actual_type: libc::mode_t,
        actual_permissions: libc::mode_t,
        actual_uid: libc::uid_t,
        actual_gid: libc::gid_t,
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
    #[snafu(display(
        "private minimal /dev target {device} exposes inode {actual_device}:{actual_inode}; expected moved private inode {expected_device}:{expected_inode}"
    ))]
    AttachedDeviceIdentityMismatch {
        device: PrivateDevice,
        expected_device: libc::dev_t,
        expected_inode: libc::ino_t,
        actual_device: libc::dev_t,
        actual_inode: libc::ino_t,
    },
    #[snafu(display(
        "private minimal /dev parent retains {actual} free inodes after its exact three placeholders"
    ))]
    UnexpectedParentFreeInodes { actual: libc::fsfilcnt_t },
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
    configure_fscontext_string(context.as_raw_fd(), c"mode", c"0755", "dev tmpfs root mode")?;

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
) -> Result<(), PrivateDeviceAssemblyError> {
    // SAFETY: parent is the fresh tmpfs root after it was attached in this
    // child mount namespace and the fixed name is NUL-terminated. O_EXCL
    // proves no prior object supplied the target.
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
    Ok(())
}

fn attach_private_device(
    source: BorrowedFd<'_>,
    parent: RawFd,
    device: PrivateDevice,
) -> Result<(), PrivateDeviceAssemblyError> {
    // SAFETY: source is a validated detached file mount, parent is the
    // now-attached bounded tmpfs, the fixed relative name denotes the exact
    // placeholder created above, and the source empty path is admitted.
    unsafe {
        move_mount(
            source.as_raw_fd(),
            Path::new(""),
            parent,
            device_path(device),
            MOVE_MOUNT_F_EMPTY_PATH,
        )
    }
    .map_err(Errno::from_i32)
    .map_err(|source| PrivateDeviceAssemblyError::Syscall {
        operation: "move_mount",
        target: device_target(device),
        source: io::Error::from_raw_os_error(source as i32),
    })?;

    // Reopen the final visible name and prove that move_mount covered the
    // placeholder with the exact already-validated private inode.
    // SAFETY: parent remains live and the fixed name is NUL-terminated.
    let attached = unsafe {
        libc::openat(
            parent,
            device.name().as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )
    };
    let attached = owned_raw_descriptor(attached, "openat(attached O_PATH)", device_target(device))?;
    let expected = observe_inode_identity(source.as_raw_fd(), device)?;
    let actual = observe_inode_identity(attached.as_raw_fd(), device)?;
    validate_attached_inode_identity(device, expected, actual)?;
    Ok(())
}

fn validate_attached_inode_identity(
    device: PrivateDevice,
    expected: (libc::dev_t, libc::ino_t),
    actual: (libc::dev_t, libc::ino_t),
) -> Result<(), PrivateDeviceAssemblyError> {
    if actual != expected {
        return Err(PrivateDeviceAssemblyError::AttachedDeviceIdentityMismatch {
            device,
            expected_device: expected.0,
            expected_inode: expected.1,
            actual_device: actual.0,
            actual_inode: actual.1,
        });
    }
    Ok(())
}

fn observe_inode_identity(
    descriptor: RawFd,
    device: PrivateDevice,
) -> Result<(libc::dev_t, libc::ino_t), PrivateDeviceAssemblyError> {
    // SAFETY: zero is valid initialization and descriptor remains live for
    // this read-only metadata query.
    let mut stat: libc::stat = unsafe { zeroed() };
    if unsafe { libc::fstat(descriptor, &mut stat) } == -1 {
        return Err(syscall_error("fstat(attached identity)", device_target(device)));
    }
    Ok((stat.st_dev, stat.st_ino))
}

fn require_no_parent_inode_headroom(descriptor: RawFd) -> Result<(), PrivateDeviceAssemblyError> {
    // SAFETY: zero is valid initialization and descriptor remains live for
    // this read-only filesystem-statistics call.
    let mut filesystem: libc::statfs = unsafe { zeroed() };
    if unsafe { libc::fstatfs(descriptor, &mut filesystem) } == -1 {
        return Err(syscall_error("fstatfs(free inodes)", "dev"));
    }
    validate_parent_free_inodes(filesystem.f_ffree)
}

fn validate_parent_free_inodes(free: libc::fsfilcnt_t) -> Result<(), PrivateDeviceAssemblyError> {
    if free == 0 {
        Ok(())
    } else {
        Err(PrivateDeviceAssemblyError::UnexpectedParentFreeInodes { actual: free })
    }
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
    root_type: libc::mode_t,
    root_permissions: libc::mode_t,
    root_uid: libc::uid_t,
    root_gid: libc::gid_t,
}

fn observe_tmpfs(descriptor: RawFd) -> Result<TmpfsObservation, PrivateDeviceAssemblyError> {
    // SAFETY: zero is valid initialization and descriptor remains live for the
    // read-only filesystem-statistics call.
    let mut filesystem: libc::statfs = unsafe { zeroed() };
    if unsafe { libc::fstatfs(descriptor, &mut filesystem) } == -1 {
        return Err(syscall_error("fstatfs", "dev"));
    }
    // SAFETY: zero is valid initialization and descriptor remains live for the
    // read-only root-inode metadata query.
    let mut root: libc::stat = unsafe { zeroed() };
    if unsafe { libc::fstat(descriptor, &mut root) } == -1 {
        return Err(syscall_error("fstat(root metadata)", "dev"));
    }
    Ok(TmpfsObservation {
        filesystem: filesystem.f_type,
        block_size: filesystem.f_bsize,
        blocks: filesystem.f_blocks,
        inodes: filesystem.f_files,
        root_type: root.st_mode & libc::S_IFMT,
        root_permissions: root.st_mode & 0o7777,
        root_uid: root.st_uid,
        root_gid: root.st_gid,
    })
}

fn validate_tmpfs_observation(observation: TmpfsObservation) -> Result<(), PrivateDeviceAssemblyError> {
    if observation.filesystem != TMPFS_MAGIC {
        return Err(PrivateDeviceAssemblyError::UnexpectedTmpfsFilesystem {
            expected: TMPFS_MAGIC,
            actual: observation.filesystem,
        });
    }
    if (
        observation.root_type,
        observation.root_permissions,
        observation.root_uid,
        observation.root_gid,
    ) != (libc::S_IFDIR, PARENT_PERMISSIONS, 0, 0)
    {
        return Err(PrivateDeviceAssemblyError::UnexpectedTmpfsRoot {
            actual_type: observation.root_type,
            actual_permissions: observation.root_permissions,
            actual_uid: observation.root_uid,
            actual_gid: observation.root_gid,
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

fn device_path(device: PrivateDevice) -> &'static Path {
    match device {
        PrivateDevice::Null => Path::new("null"),
        PrivateDevice::Zero => Path::new("zero"),
        PrivateDevice::Full => Path::new("full"),
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
        root_type: libc::S_IFDIR,
        root_permissions: PARENT_PERMISSIONS,
        root_uid: 0,
        root_gid: 0,
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
    fn child_dev_tmpfs_rejects_noncanonical_root_metadata() {
        for observation in [
            TmpfsObservation {
                root_type: libc::S_IFREG,
                ..EXACT_TMPFS
            },
            TmpfsObservation {
                root_permissions: 0o1777,
                ..EXACT_TMPFS
            },
            TmpfsObservation {
                root_uid: 1,
                ..EXACT_TMPFS
            },
            TmpfsObservation {
                root_gid: 1,
                ..EXACT_TMPFS
            },
        ] {
            assert!(matches!(
                validate_tmpfs_observation(observation),
                Err(PrivateDeviceAssemblyError::UnexpectedTmpfsRoot { .. })
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
        assert_eq!(
            [PrivateDevice::Null, PrivateDevice::Zero, PrivateDevice::Full].map(device_path),
            [Path::new("null"), Path::new("zero"), Path::new("full")]
        );
    }

    #[test]
    fn attached_device_identity_must_match_the_validated_source() {
        validate_attached_inode_identity(PrivateDevice::Null, (41, 7), (41, 7)).unwrap();
        assert!(matches!(
            validate_attached_inode_identity(PrivateDevice::Null, (41, 7), (41, 8)),
            Err(PrivateDeviceAssemblyError::AttachedDeviceIdentityMismatch {
                device: PrivateDevice::Null,
                ..
            })
        ));
    }

    #[test]
    fn attached_parent_requires_zero_free_inodes() {
        validate_parent_free_inodes(0).unwrap();
        assert!(matches!(
            validate_parent_free_inodes(1),
            Err(PrivateDeviceAssemblyError::UnexpectedParentFreeInodes { actual: 1 })
        ));
    }
}
