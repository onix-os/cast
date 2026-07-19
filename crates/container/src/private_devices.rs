//! Private, descriptor-only character devices for the minimal `/dev` policy.
//!
//! This module does not clone ambient host device nodes. The direct
//! provisioner creates a fresh detached tmpfs, creates the three fixed Linux
//! memory devices on it, clones each node into its own detached file mount,
//! unlinks every source name, and drops the source mount. The returned
//! capability therefore owns exactly three writable mounts backed by private,
//! unlinked inodes.

use std::fmt;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd, RawFd};

use nc::{AT_EMPTY_PATH, SYS_MOUNT_SETATTR, mount_attr_t};
use nix::libc;
use snafu::Snafu;

const FSOPEN_CLOEXEC: libc::c_uint = 0x0000_0001;
const FSCONFIG_SET_STRING: libc::c_uint = 1;
const FSCONFIG_CMD_CREATE: libc::c_uint = 6;
const FSMOUNT_CLOEXEC: libc::c_uint = 0x0000_0001;
const OPEN_TREE_CLONE: libc::c_uint = 0x0000_0001;
const OPEN_TREE_CLOEXEC: libc::c_uint = libc::O_CLOEXEC as libc::c_uint;
const MOUNT_ATTR_RDONLY: u64 = 0x0000_0001;
const MOUNT_ATTR_NODEV: u64 = 0x0000_0004;
const TMPFS_MAGIC: libc::c_long = 0x0102_1994;
const DEVICE_PERMISSIONS: libc::mode_t = 0o666;

pub(crate) const PRIVATE_DEVICE_COUNT: usize = 3;
/// Fixed metadata/data ceiling for one provider-owned private-device tmpfs.
pub(crate) const PRIVATE_DEVICE_TMPFS_SIZE_BYTES: u64 = 64 * 1024;
/// One root directory inode plus exactly three unlinked device inodes.
pub(crate) const PRIVATE_DEVICE_TMPFS_INODES: u64 = 4;

/// Stable order and Linux identity for the complete private-device contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrivateDevice {
    Null,
    Zero,
    Full,
}

pub(crate) const PRIVATE_DEVICE_ORDER: [PrivateDevice; PRIVATE_DEVICE_COUNT] =
    [PrivateDevice::Null, PrivateDevice::Zero, PrivateDevice::Full];

impl PrivateDevice {
    pub(crate) const fn name(self) -> &'static std::ffi::CStr {
        match self {
            Self::Null => c"null",
            Self::Zero => c"zero",
            Self::Full => c"full",
        }
    }

    pub(crate) const fn major(self) -> libc::c_uint {
        1
    }

    pub(crate) const fn minor(self) -> libc::c_uint {
        match self {
            Self::Null => 3,
            Self::Zero => 5,
            Self::Full => 7,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Null => 0,
            Self::Zero => 1,
            Self::Full => 2,
        }
    }
}

impl fmt::Display for PrivateDevice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name().to_str().expect("fixed device name is UTF-8"))
    }
}

/// Owned authority over the complete private minimal-device set.
///
/// The type deliberately does not implement `Clone`: duplicating or replacing
/// one descriptor must be an explicit operation at the activation boundary.
/// Its private constructor admits only a validated, ordered set produced by
/// the direct provisioner.
#[derive(Debug)]
pub(crate) struct PrivateDeviceMounts {
    descriptors: [OwnedFd; PRIVATE_DEVICE_COUNT],
}

impl PrivateDeviceMounts {
    fn from_provisioned(descriptors: [OwnedFd; PRIVATE_DEVICE_COUNT]) -> Result<Self, PrivateDeviceError> {
        let mounts = Self { descriptors };
        mounts.validate()?;
        Ok(mounts)
    }

    /// Admit exactly one authenticated broker response's fixed descriptor set.
    pub(super) fn from_received(descriptors: [OwnedFd; PRIVATE_DEVICE_COUNT]) -> Result<Self, PrivateDeviceError> {
        Self::from_provisioned(descriptors)
    }

    /// Borrow all three detached file-mount descriptors in their fixed order.
    pub(crate) fn ordered(&self) -> [(PrivateDevice, BorrowedFd<'_>); PRIVATE_DEVICE_COUNT] {
        PRIVATE_DEVICE_ORDER.map(|device| (device, self.descriptors[device.index()].as_fd()))
    }

    /// Re-authenticate every descriptor and the relationships between them.
    pub(crate) fn validate(&self) -> Result<(), PrivateDeviceError> {
        validate_private_device_observations(&self.observations()?)
    }

    /// Re-authenticate properties that remain stable after entering a user
    /// namespace whose ID map may translate initial-namespace root ownership.
    /// Exact `0:0` ownership is required by [`Self::validate`] before clone;
    /// child setup must not mistake an unmapped owner for capability drift.
    pub(crate) fn validate_namespace_invariants(&self) -> Result<(), PrivateDeviceError> {
        validate_namespace_invariant_observations(&self.observations()?)
    }

    fn observations(&self) -> Result<[PrivateDeviceObservation; PRIVATE_DEVICE_COUNT], PrivateDeviceError> {
        Ok([
            observe_private_device(
                self.descriptors[PrivateDevice::Null.index()].as_fd(),
                PrivateDevice::Null,
            )?,
            observe_private_device(
                self.descriptors[PrivateDevice::Zero.index()].as_fd(),
                PrivateDevice::Zero,
            )?,
            observe_private_device(
                self.descriptors[PrivateDevice::Full.index()].as_fd(),
                PrivateDevice::Full,
            )?,
        ])
    }
}

/// Provision exactly `/dev/null`, `/dev/zero`, and `/dev/full` as private,
/// writable detached file mounts.
///
/// This is the direct privileged implementation. The process needs
/// initial-user-namespace `CAP_SYS_ADMIN` and `CAP_MKNOD` to create these
/// mounts and character devices. There is no ambient
/// `/dev` fallback: lack of that authority is returned as an error. Production
/// activation reaches this implementation only through the fixed broker; its
/// result must satisfy the validation contract enforced by
/// [`PrivateDeviceMounts`].
pub(crate) fn provision_private_device_mounts() -> Result<PrivateDeviceMounts, PrivateDeviceError> {
    let mut scratch = ScratchDeviceMount::new()?;
    for device in PRIVATE_DEVICE_ORDER {
        scratch.create(device)?;
    }

    let null = scratch.clone_node(PrivateDevice::Null)?;
    let zero = scratch.clone_node(PrivateDevice::Zero)?;
    let full = scratch.clone_node(PrivateDevice::Full)?;
    scratch.unlink_all()?;
    drop(scratch);

    PrivateDeviceMounts::from_provisioned([null, zero, full])
}

#[derive(Debug, Snafu)]
pub(crate) enum PrivateDeviceError {
    #[snafu(display("{operation} for private device target {target}"))]
    Syscall {
        operation: &'static str,
        target: &'static str,
        source: io::Error,
    },
    #[snafu(display("{operation} returned an invalid descriptor for private device target {target}"))]
    InvalidDescriptor {
        operation: &'static str,
        target: &'static str,
    },
    #[snafu(display(
        "private {device} mount has filesystem magic {actual:#x}; expected private tmpfs magic {expected:#x}"
    ))]
    UnexpectedFilesystem {
        device: PrivateDevice,
        expected: libc::c_long,
        actual: libc::c_long,
    },
    #[snafu(display("private {device} mount has file type {actual:o}; expected a character device"))]
    UnexpectedFileType {
        device: PrivateDevice,
        actual: libc::mode_t,
    },
    #[snafu(display("private {device} mount has permissions {actual:o}; expected exactly {expected:o}"))]
    UnexpectedPermissions {
        device: PrivateDevice,
        expected: libc::mode_t,
        actual: libc::mode_t,
    },
    #[snafu(display(
        "private {device} mount has owner {actual_uid}:{actual_gid}; expected initial-namespace root 0:0"
    ))]
    UnexpectedOwner {
        device: PrivateDevice,
        actual_uid: libc::uid_t,
        actual_gid: libc::gid_t,
    },
    #[snafu(display(
        "private {device} mount has Linux device identity ({actual_major},{actual_minor}); expected ({expected_major},{expected_minor})"
    ))]
    UnexpectedIdentity {
        device: PrivateDevice,
        expected_major: libc::c_uint,
        expected_minor: libc::c_uint,
        actual_major: libc::c_uint,
        actual_minor: libc::c_uint,
    },
    #[snafu(display("private {device} inode still has {actual} directory links; expected an unlinked inode"))]
    LinkedSource {
        device: PrivateDevice,
        actual: libc::nlink_t,
    },
    #[snafu(display("private {device} file mount is read-only"))]
    ReadOnlyMount { device: PrivateDevice },
    #[snafu(display("private {device} file mount disables character-device access with nodev"))]
    DeviceAccessDisabled { device: PrivateDevice },
    #[snafu(display("private {device} mount descriptor is not close-on-exec"))]
    DescriptorNotCloseOnExec { device: PrivateDevice },
    #[snafu(display(
        "private {device} mount descriptor has status flags {actual:#x}; expected an O_PATH mount capability"
    ))]
    DescriptorNotPathCapability { device: PrivateDevice, actual: libc::c_int },
    #[snafu(display("private {device} mount is backed by a different tmpfs from private {expected_peer}"))]
    DifferentBackingFilesystem {
        device: PrivateDevice,
        expected_peer: PrivateDevice,
    },
    #[snafu(display("private {first} and private {second} mounts alias the same inode"))]
    AliasedInode {
        first: PrivateDevice,
        second: PrivateDevice,
    },
    #[snafu(display("private tmpfs capacity readback for {target} is not representable"))]
    InvalidTmpfsCapacityReadback { target: &'static str },
    #[snafu(display(
        "private tmpfs for {target} has capacity {actual_size_bytes} bytes/{actual_inodes} inodes; expected exactly {expected_size_bytes} bytes/{expected_inodes} inodes"
    ))]
    UnexpectedTmpfsCapacity {
        target: &'static str,
        expected_size_bytes: u64,
        actual_size_bytes: u64,
        expected_inodes: u64,
        actual_inodes: u64,
    },
    #[snafu(display("private tmpfs for {target} has filesystem magic {actual:#x}; expected {expected:#x}"))]
    UnexpectedPrivateTmpfsFilesystem {
        target: &'static str,
        expected: libc::c_long,
        actual: libc::c_long,
    },
}

struct ScratchDeviceMount {
    mount: OwnedFd,
    linked: [bool; PRIVATE_DEVICE_COUNT],
}

impl ScratchDeviceMount {
    fn new() -> Result<Self, PrivateDeviceError> {
        let mount = detached_tmpfs()?;
        set_device_mount_writable(mount.as_raw_fd(), "scratch tmpfs")?;
        Ok(Self {
            mount,
            linked: [false; PRIVATE_DEVICE_COUNT],
        })
    }

    fn create(&mut self, device: PrivateDevice) -> Result<(), PrivateDeviceError> {
        // SAFETY: the fixed name is NUL-terminated, the detached tmpfs root is
        // a live directory descriptor, and makedev receives stable UAPI values.
        let created = unsafe {
            libc::mknodat(
                self.mount.as_raw_fd(),
                device.name().as_ptr(),
                libc::S_IFCHR | 0o600,
                libc::makedev(device.major(), device.minor()),
            )
        };
        if created == -1 {
            return Err(syscall_error("mknodat", device_target(device)));
        }
        self.linked[device.index()] = true;

        // mknodat applies the process umask. Descriptor-relative chmod makes
        // the final data-plane permissions exact without changing the
        // process-global umask.
        // SAFETY: the fixed name resolves only inside the private detached
        // tmpfs and the flags value requests ordinary fchmodat semantics.
        if unsafe { libc::fchmodat(self.mount.as_raw_fd(), device.name().as_ptr(), DEVICE_PERMISSIONS, 0) } == -1 {
            return Err(syscall_error("fchmodat", device_target(device)));
        }
        Ok(())
    }

    fn clone_node(&self, device: PrivateDevice) -> Result<OwnedFd, PrivateDeviceError> {
        // SAFETY: the fixed relative name and live tmpfs descriptor are valid
        // for the call. OPEN_TREE_CLONE returns a new detached file mount and
        // OPEN_TREE_CLOEXEC prevents an accidental exec inheritance.
        let descriptor = unsafe {
            libc::syscall(
                libc::SYS_open_tree,
                self.mount.as_raw_fd(),
                device.name().as_ptr(),
                OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC,
            )
        };
        let descriptor = owned_syscall_descriptor(descriptor, "open_tree", device_target(device))?;
        set_device_mount_writable(descriptor.as_raw_fd(), device_target(device))?;
        Ok(descriptor)
    }

    fn unlink_all(&mut self) -> Result<(), PrivateDeviceError> {
        let mut first_error = None;
        for device in PRIVATE_DEVICE_ORDER {
            if !self.linked[device.index()] {
                continue;
            }
            // SAFETY: the fixed name is relative to the live private tmpfs.
            let result = unsafe { libc::unlinkat(self.mount.as_raw_fd(), device.name().as_ptr(), 0) };
            if result == -1 {
                if first_error.is_none() {
                    first_error = Some(syscall_error("unlinkat", device_target(device)));
                }
            } else {
                self.linked[device.index()] = false;
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for ScratchDeviceMount {
    fn drop(&mut self) {
        for device in PRIVATE_DEVICE_ORDER {
            if self.linked[device.index()] {
                // Best-effort error-path cleanup. Dropping the detached root
                // immediately afterwards is the final containment boundary.
                // SAFETY: both descriptor and fixed name remain live here.
                unsafe {
                    libc::unlinkat(self.mount.as_raw_fd(), device.name().as_ptr(), 0);
                }
            }
        }
    }
}

fn detached_tmpfs() -> Result<OwnedFd, PrivateDeviceError> {
    // SAFETY: the fixed filesystem name is NUL-terminated. A successful call
    // returns one fresh close-on-exec filesystem-context descriptor.
    let context = unsafe { libc::syscall(libc::SYS_fsopen, c"tmpfs".as_ptr(), FSOPEN_CLOEXEC) };
    let context = owned_syscall_descriptor(context, "fsopen", "scratch tmpfs")?;

    configure_fscontext_string(context.as_raw_fd(), c"size", c"65536", "scratch tmpfs size")?;
    configure_fscontext_string(context.as_raw_fd(), c"nr_inodes", c"4", "scratch tmpfs inode ceiling")?;

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
        return Err(syscall_error("fsconfig(CREATE)", "scratch tmpfs"));
    }

    // SAFETY: the configured context is live and a successful fsmount returns
    // one fresh detached close-on-exec mount descriptor.
    let mount = unsafe { libc::syscall(libc::SYS_fsmount, context.as_raw_fd(), FSMOUNT_CLOEXEC, 0) };
    let mount = owned_syscall_descriptor(mount, "fsmount", "scratch tmpfs")?;
    validate_tmpfs_capacity(mount.as_fd(), "scratch tmpfs")?;
    Ok(mount)
}

fn configure_fscontext_string(
    context: RawFd,
    key: &std::ffi::CStr,
    value: &std::ffi::CStr,
    target: &'static str,
) -> Result<(), PrivateDeviceError> {
    // SAFETY: context is a live fscontext descriptor; key and value are
    // NUL-terminated and borrowed only for this fsconfig call.
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

fn validate_tmpfs_capacity(descriptor: BorrowedFd<'_>, target: &'static str) -> Result<(), PrivateDeviceError> {
    // SAFETY: zero is valid initialization and descriptor remains live for
    // this read-only filesystem-statistics call.
    let mut filesystem: libc::statfs = unsafe { zeroed() };
    if unsafe { libc::fstatfs(descriptor.as_raw_fd(), &mut filesystem) } == -1 {
        return Err(syscall_error("fstatfs(capacity)", target));
    }
    if filesystem.f_type != TMPFS_MAGIC {
        return Err(PrivateDeviceError::UnexpectedPrivateTmpfsFilesystem {
            target,
            expected: TMPFS_MAGIC,
            actual: filesystem.f_type,
        });
    }
    let (size_bytes, inodes) = tmpfs_capacity(&filesystem, target)?;
    validate_tmpfs_capacity_values(target, size_bytes, inodes)
}

fn tmpfs_capacity(filesystem: &libc::statfs, target: &'static str) -> Result<(u64, u64), PrivateDeviceError> {
    let block_size =
        u64::try_from(filesystem.f_bsize).map_err(|_| PrivateDeviceError::InvalidTmpfsCapacityReadback { target })?;
    let blocks =
        u64::try_from(filesystem.f_blocks).map_err(|_| PrivateDeviceError::InvalidTmpfsCapacityReadback { target })?;
    let size_bytes = block_size
        .checked_mul(blocks)
        .ok_or(PrivateDeviceError::InvalidTmpfsCapacityReadback { target })?;
    let inodes =
        u64::try_from(filesystem.f_files).map_err(|_| PrivateDeviceError::InvalidTmpfsCapacityReadback { target })?;
    Ok((size_bytes, inodes))
}

fn validate_tmpfs_capacity_values(
    target: &'static str,
    size_bytes: u64,
    inodes: u64,
) -> Result<(), PrivateDeviceError> {
    if (size_bytes, inodes) == (PRIVATE_DEVICE_TMPFS_SIZE_BYTES, PRIVATE_DEVICE_TMPFS_INODES) {
        Ok(())
    } else {
        Err(PrivateDeviceError::UnexpectedTmpfsCapacity {
            target,
            expected_size_bytes: PRIVATE_DEVICE_TMPFS_SIZE_BYTES,
            actual_size_bytes: size_bytes,
            expected_inodes: PRIVATE_DEVICE_TMPFS_INODES,
            actual_inodes: inodes,
        })
    }
}

fn set_device_mount_writable(descriptor: RawFd, target: &'static str) -> Result<(), PrivateDeviceError> {
    let attributes = mount_attr_t {
        attr_set: 0,
        attr_clr: MOUNT_ATTR_RDONLY | MOUNT_ATTR_NODEV,
        program: 0,
        userns_fd: 0,
    };
    // SAFETY: descriptor names a live detached mount, the empty path is
    // NUL-terminated, AT_EMPTY_PATH selects that mount, and attributes has the
    // kernel UAPI layout supplied by nc.
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
    result.map(|_| ()).map_err(|errno| PrivateDeviceError::Syscall {
        operation: "mount_setattr(clear ro,nodev)",
        target,
        source: io::Error::from_raw_os_error(errno),
    })
}

fn owned_syscall_descriptor(
    result: libc::c_long,
    operation: &'static str,
    target: &'static str,
) -> Result<OwnedFd, PrivateDeviceError> {
    if result == -1 {
        return Err(syscall_error(operation, target));
    }
    let descriptor =
        RawFd::try_from(result).map_err(|_| PrivateDeviceError::InvalidDescriptor { operation, target })?;
    // SAFETY: a successful descriptor-producing syscall returned a fresh
    // owned descriptor, transferred exactly once into OwnedFd.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn syscall_error(operation: &'static str, target: &'static str) -> PrivateDeviceError {
    PrivateDeviceError::Syscall {
        operation,
        target,
        source: io::Error::last_os_error(),
    }
}

const fn device_target(device: PrivateDevice) -> &'static str {
    match device {
        PrivateDevice::Null => "null",
        PrivateDevice::Zero => "zero",
        PrivateDevice::Full => "full",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrivateDeviceObservation {
    filesystem: libc::c_long,
    file_type: libc::mode_t,
    permissions: libc::mode_t,
    uid: libc::uid_t,
    gid: libc::gid_t,
    major: libc::c_uint,
    minor: libc::c_uint,
    links: libc::nlink_t,
    filesystem_device: libc::dev_t,
    inode: libc::ino_t,
    mount_flags: libc::c_ulong,
    descriptor_flags: libc::c_int,
    status_flags: libc::c_int,
    tmpfs_size_bytes: u64,
    tmpfs_inodes: u64,
}

fn observe_private_device(
    descriptor: BorrowedFd<'_>,
    device: PrivateDevice,
) -> Result<PrivateDeviceObservation, PrivateDeviceError> {
    // SAFETY: zero is valid initialization for stat and the live descriptor
    // remains borrowed exclusively for the output call.
    let mut stat: libc::stat = unsafe { zeroed() };
    if unsafe { libc::fstat(descriptor.as_raw_fd(), &mut stat) } == -1 {
        return Err(syscall_error("fstat", device_target(device)));
    }

    // SAFETY: zero is valid initialization for statfs and the live descriptor
    // remains borrowed exclusively for the output call.
    let mut filesystem: libc::statfs = unsafe { zeroed() };
    if unsafe { libc::fstatfs(descriptor.as_raw_fd(), &mut filesystem) } == -1 {
        return Err(syscall_error("fstatfs", device_target(device)));
    }
    let (tmpfs_size_bytes, tmpfs_inodes) = tmpfs_capacity(&filesystem, device_target(device))?;

    // SAFETY: zero is valid initialization for statvfs and the live descriptor
    // remains borrowed exclusively for the output call.
    let mut mount: libc::statvfs = unsafe { zeroed() };
    if unsafe { libc::fstatvfs(descriptor.as_raw_fd(), &mut mount) } == -1 {
        return Err(syscall_error("fstatvfs", device_target(device)));
    }

    // SAFETY: both fcntl commands are read-only descriptor queries.
    let descriptor_flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) };
    if descriptor_flags == -1 {
        return Err(syscall_error("fcntl(F_GETFD)", device_target(device)));
    }
    // SAFETY: both fcntl commands are read-only descriptor queries.
    let status_flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
    if status_flags == -1 {
        return Err(syscall_error("fcntl(F_GETFL)", device_target(device)));
    }

    Ok(PrivateDeviceObservation {
        filesystem: filesystem.f_type,
        file_type: stat.st_mode & libc::S_IFMT,
        permissions: stat.st_mode & 0o7777,
        uid: stat.st_uid,
        gid: stat.st_gid,
        major: libc::major(stat.st_rdev),
        minor: libc::minor(stat.st_rdev),
        links: stat.st_nlink,
        filesystem_device: stat.st_dev,
        inode: stat.st_ino,
        mount_flags: mount.f_flag,
        descriptor_flags,
        status_flags,
        tmpfs_size_bytes,
        tmpfs_inodes,
    })
}

fn validate_private_device_observations(
    observations: &[PrivateDeviceObservation; PRIVATE_DEVICE_COUNT],
) -> Result<(), PrivateDeviceError> {
    validate_namespace_invariant_observations(observations)?;
    for (index, device) in PRIVATE_DEVICE_ORDER.into_iter().enumerate() {
        let observation = observations[index];
        if (observation.uid, observation.gid) != (0, 0) {
            return Err(PrivateDeviceError::UnexpectedOwner {
                device,
                actual_uid: observation.uid,
                actual_gid: observation.gid,
            });
        }
    }
    Ok(())
}

fn validate_namespace_invariant_observations(
    observations: &[PrivateDeviceObservation; PRIVATE_DEVICE_COUNT],
) -> Result<(), PrivateDeviceError> {
    for (index, device) in PRIVATE_DEVICE_ORDER.into_iter().enumerate() {
        validate_namespace_invariant_observation(device, observations[index])?;
    }

    let first_backing = observations[PrivateDevice::Null.index()].filesystem_device;
    for device in [PrivateDevice::Zero, PrivateDevice::Full] {
        if observations[device.index()].filesystem_device != first_backing {
            return Err(PrivateDeviceError::DifferentBackingFilesystem {
                device,
                expected_peer: PrivateDevice::Null,
            });
        }
    }

    for first_index in 0..PRIVATE_DEVICE_COUNT {
        for second_index in (first_index + 1)..PRIVATE_DEVICE_COUNT {
            let first = observations[first_index];
            let second = observations[second_index];
            if (first.filesystem_device, first.inode) == (second.filesystem_device, second.inode) {
                return Err(PrivateDeviceError::AliasedInode {
                    first: PRIVATE_DEVICE_ORDER[first_index],
                    second: PRIVATE_DEVICE_ORDER[second_index],
                });
            }
        }
    }
    Ok(())
}

fn validate_namespace_invariant_observation(
    device: PrivateDevice,
    observation: PrivateDeviceObservation,
) -> Result<(), PrivateDeviceError> {
    if observation.filesystem != TMPFS_MAGIC {
        return Err(PrivateDeviceError::UnexpectedFilesystem {
            device,
            expected: TMPFS_MAGIC,
            actual: observation.filesystem,
        });
    }
    if observation.file_type != libc::S_IFCHR {
        return Err(PrivateDeviceError::UnexpectedFileType {
            device,
            actual: observation.file_type,
        });
    }
    if observation.permissions != DEVICE_PERMISSIONS {
        return Err(PrivateDeviceError::UnexpectedPermissions {
            device,
            expected: DEVICE_PERMISSIONS,
            actual: observation.permissions,
        });
    }
    if (observation.major, observation.minor) != (device.major(), device.minor()) {
        return Err(PrivateDeviceError::UnexpectedIdentity {
            device,
            expected_major: device.major(),
            expected_minor: device.minor(),
            actual_major: observation.major,
            actual_minor: observation.minor,
        });
    }
    if observation.links != 0 {
        return Err(PrivateDeviceError::LinkedSource {
            device,
            actual: observation.links,
        });
    }
    if observation.mount_flags & libc::ST_RDONLY != 0 {
        return Err(PrivateDeviceError::ReadOnlyMount { device });
    }
    if observation.mount_flags & libc::ST_NODEV != 0 {
        return Err(PrivateDeviceError::DeviceAccessDisabled { device });
    }
    if observation.descriptor_flags & libc::FD_CLOEXEC == 0 {
        return Err(PrivateDeviceError::DescriptorNotCloseOnExec { device });
    }
    if observation.status_flags & libc::O_PATH != libc::O_PATH {
        return Err(PrivateDeviceError::DescriptorNotPathCapability {
            device,
            actual: observation.status_flags,
        });
    }
    validate_tmpfs_capacity_values(
        device_target(device),
        observation.tmpfs_size_bytes,
        observation.tmpfs_inodes,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn exact_observation(device: PrivateDevice) -> PrivateDeviceObservation {
        PrivateDeviceObservation {
            filesystem: TMPFS_MAGIC,
            file_type: libc::S_IFCHR,
            permissions: DEVICE_PERMISSIONS,
            uid: 0,
            gid: 0,
            major: device.major(),
            minor: device.minor(),
            links: 0,
            filesystem_device: 41,
            inode: 100 + device.index() as libc::ino_t,
            mount_flags: 0,
            descriptor_flags: libc::FD_CLOEXEC,
            status_flags: libc::O_PATH,
            tmpfs_size_bytes: PRIVATE_DEVICE_TMPFS_SIZE_BYTES,
            tmpfs_inodes: PRIVATE_DEVICE_TMPFS_INODES,
        }
    }

    fn exact_set() -> [PrivateDeviceObservation; PRIVATE_DEVICE_COUNT] {
        PRIVATE_DEVICE_ORDER.map(exact_observation)
    }

    #[test]
    fn fixed_contract_is_exactly_null_zero_and_full() {
        assert_eq!(PRIVATE_DEVICE_COUNT, 3);
        assert_eq!(PRIVATE_DEVICE_TMPFS_SIZE_BYTES, 65_536);
        assert_eq!(PRIVATE_DEVICE_TMPFS_INODES, 4);
        assert_eq!(
            PRIVATE_DEVICE_ORDER,
            [PrivateDevice::Null, PrivateDevice::Zero, PrivateDevice::Full]
        );
        assert_eq!(
            PRIVATE_DEVICE_ORDER.map(|device| (device.name().to_bytes(), device.major(), device.minor())),
            [
                (b"null".as_slice(), 1, 3),
                (b"zero".as_slice(), 1, 5),
                (b"full".as_slice(), 1, 7)
            ]
        );
    }

    #[test]
    fn exact_private_unlinked_tmpfs_set_is_accepted() {
        validate_private_device_observations(&exact_set()).unwrap();
    }

    #[test]
    fn namespace_invariant_validation_allows_only_owner_translation() {
        let mut translated = exact_set();
        for observation in &mut translated {
            observation.uid = 65_534;
            observation.gid = 65_534;
        }
        validate_namespace_invariant_observations(&translated).unwrap();
        assert!(matches!(
            validate_private_device_observations(&translated),
            Err(PrivateDeviceError::UnexpectedOwner { .. })
        ));

        translated[PrivateDevice::Zero.index()].minor = 9;
        assert!(matches!(
            validate_namespace_invariant_observations(&translated),
            Err(PrivateDeviceError::UnexpectedIdentity {
                device: PrivateDevice::Zero,
                ..
            })
        ));
    }

    #[test]
    fn device_metadata_contract_fails_closed() {
        let cases: [(fn(&mut PrivateDeviceObservation), fn(&PrivateDeviceError) -> bool); 12] = [
            (
                |observation| observation.filesystem = 0,
                |error| matches!(error, PrivateDeviceError::UnexpectedFilesystem { .. }),
            ),
            (
                |observation| observation.file_type = libc::S_IFREG,
                |error| matches!(error, PrivateDeviceError::UnexpectedFileType { .. }),
            ),
            (
                |observation| observation.permissions = 0o600,
                |error| matches!(error, PrivateDeviceError::UnexpectedPermissions { .. }),
            ),
            (
                |observation| observation.uid = 1_000,
                |error| matches!(error, PrivateDeviceError::UnexpectedOwner { .. }),
            ),
            (
                |observation| observation.gid = 1_000,
                |error| matches!(error, PrivateDeviceError::UnexpectedOwner { .. }),
            ),
            (
                |observation| observation.minor = 9,
                |error| matches!(error, PrivateDeviceError::UnexpectedIdentity { .. }),
            ),
            (
                |observation| observation.links = 1,
                |error| matches!(error, PrivateDeviceError::LinkedSource { .. }),
            ),
            (
                |observation| observation.mount_flags = libc::ST_RDONLY,
                |error| matches!(error, PrivateDeviceError::ReadOnlyMount { .. }),
            ),
            (
                |observation| observation.mount_flags = libc::ST_NODEV,
                |error| matches!(error, PrivateDeviceError::DeviceAccessDisabled { .. }),
            ),
            (
                |observation| observation.descriptor_flags = 0,
                |error| matches!(error, PrivateDeviceError::DescriptorNotCloseOnExec { .. }),
            ),
            (
                |observation| observation.status_flags = libc::O_RDONLY,
                |error| matches!(error, PrivateDeviceError::DescriptorNotPathCapability { .. }),
            ),
            (
                |observation| observation.tmpfs_size_bytes *= 2,
                |error| matches!(error, PrivateDeviceError::UnexpectedTmpfsCapacity { .. }),
            ),
        ];

        for (mutate, expected) in cases {
            let mut observations = exact_set();
            mutate(&mut observations[PrivateDevice::Zero.index()]);
            let error = validate_private_device_observations(&observations).unwrap_err();
            assert!(expected(&error), "unexpected validation error: {error}");
        }
    }

    #[test]
    fn all_mounts_must_share_one_private_tmpfs() {
        let mut observations = exact_set();
        observations[PrivateDevice::Full.index()].filesystem_device += 1;
        assert!(matches!(
            validate_private_device_observations(&observations),
            Err(PrivateDeviceError::DifferentBackingFilesystem {
                device: PrivateDevice::Full,
                expected_peer: PrivateDevice::Null,
            })
        ));
    }

    #[test]
    fn device_mounts_must_not_alias_an_inode() {
        let mut observations = exact_set();
        observations[PrivateDevice::Full.index()].inode = observations[PrivateDevice::Zero.index()].inode;
        assert!(matches!(
            validate_private_device_observations(&observations),
            Err(PrivateDeviceError::AliasedInode {
                first: PrivateDevice::Zero,
                second: PrivateDevice::Full,
            })
        ));
    }

    #[test]
    fn tmpfs_capacity_contract_rejects_inode_headroom() {
        assert!(matches!(
            validate_tmpfs_capacity_values(
                "test tmpfs",
                PRIVATE_DEVICE_TMPFS_SIZE_BYTES,
                PRIVATE_DEVICE_TMPFS_INODES + 1,
            ),
            Err(PrivateDeviceError::UnexpectedTmpfsCapacity {
                target: "test tmpfs",
                ..
            })
        ));
    }

    #[test]
    fn privileged_provider_returns_exact_private_mounts() {
        let required = std::env::var_os("CAST_REQUIRE_PRIVATE_DEVICE_PROVISIONING").is_some_and(|value| value == "1");
        let first = match provision_private_device_mounts() {
            Ok(mounts) => mounts,
            Err(error) if !required && provider_is_unavailable(&error) => {
                eprintln!("private-device provider explicitly skipped: {error}");
                return;
            }
            Err(error) => panic!("private-device provider failed: {error}"),
        };
        first.validate().unwrap();
        let second = provision_private_device_mounts()
            .unwrap_or_else(|error| panic!("second private-device provision failed: {error}"));
        second.validate().unwrap();

        let first_identities = inode_identities(&first);
        let second_identities = inode_identities(&second);
        assert!(
            first_identities
                .into_iter()
                .zip(second_identities)
                .all(|(first, second)| first != second),
            "consecutive private-device provisions reused inode identities"
        );
    }

    fn provider_is_unavailable(error: &PrivateDeviceError) -> bool {
        matches!(
            error,
            PrivateDeviceError::Syscall { source, .. }
                if matches!(
                    source.raw_os_error(),
                    Some(libc::EPERM | libc::EACCES | libc::ENOSYS)
                )
        )
    }

    fn inode_identities(mounts: &PrivateDeviceMounts) -> [(libc::dev_t, libc::ino_t); PRIVATE_DEVICE_COUNT] {
        mounts.ordered().map(|(device, descriptor)| {
            let observation = observe_private_device(descriptor, device).unwrap();
            (observation.filesystem_device, observation.inode)
        })
    }
}
