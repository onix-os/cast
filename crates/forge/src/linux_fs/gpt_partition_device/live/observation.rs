use std::{
    io,
    os::fd::{AsRawFd as _, BorrowedFd, RawFd},
    time::Instant,
};

use super::{
    super::{BlockDeviceObservation, BlockDeviceObserver, ObservedDeviceAccess, ObservedNodeKind},
    abi,
    image::RetainedReadOnlyBlockImage,
    syscalls::{LinuxObservationSyscalls, ObservationSyscalls, RawBlockDeviceStat},
};

/// One borrowed retained descriptor plus its separately authenticated mount ID.
///
/// Successful observation results contain only closed scalars. This temporary
/// observer retains the borrow solely so the composition layer can sandwich
/// GPT reads without reopening the node.
pub(in crate::linux_fs) struct RetainedBlockDeviceObserver<'descriptor> {
    descriptor: BorrowedFd<'descriptor>,
    authenticated_mount_id: u64,
    latest: Option<BlockDeviceObservation>,
}

impl<'descriptor> RetainedBlockDeviceObserver<'descriptor> {
    #[allow(dead_code)] // consumed by the next descriptor/GPT composition slice
    pub(in crate::linux_fs) fn new(
        descriptor: BorrowedFd<'descriptor>,
        authenticated_mount_id: u64,
    ) -> io::Result<Self> {
        abi::require_supported_block_abi()?;
        if authenticated_mount_id == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "retained block-device mount ID must be authenticated and nonzero",
            ));
        }
        Ok(Self {
            descriptor,
            authenticated_mount_id,
            latest: None,
        })
    }

    /// Borrow a bounded GPT image using the most recently authenticated size.
    #[allow(dead_code)] // consumed by the next descriptor/GPT composition slice
    pub(in crate::linux_fs) fn image_until(&self, deadline: Instant) -> io::Result<RetainedReadOnlyBlockImage<'_>> {
        let latest = self.latest.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "retained block device must be observed before creating its GPT image",
            )
        })?;
        RetainedReadOnlyBlockImage::new(self.descriptor, latest.byte_length(), deadline)
    }
}

impl BlockDeviceObserver for RetainedBlockDeviceObserver<'_> {
    fn observe_until(&mut self, deadline: Instant) -> io::Result<BlockDeviceObservation> {
        self.latest = None;
        let mut syscalls = LinuxObservationSyscalls;
        let mut clock = Instant::now;
        let observation = observe_with_syscalls_and_clock_until(
            self.descriptor.as_raw_fd(),
            self.authenticated_mount_id,
            deadline,
            &mut clock,
            &mut syscalls,
        )?;
        self.latest = Some(observation);
        Ok(observation)
    }
}

fn observe_with_syscalls_and_clock_until(
    descriptor: RawFd,
    authenticated_mount_id: u64,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    syscalls: &mut impl ObservationSyscalls,
) -> io::Result<BlockDeviceObservation> {
    abi::require_supported_block_abi()?;
    checkpoint(deadline, clock)?;
    if authenticated_mount_id == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "retained block-device mount ID must be authenticated and nonzero",
        ));
    }

    let status = one_call_until(deadline, clock, || syscalls.fstat_once(descriptor))?;
    let (node_kind, block_major, block_minor) = validate_status(status)?;
    if node_kind != ObservedNodeKind::BlockDevice {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained descriptor is not a block device",
        ));
    }

    let flags = one_call_until(deadline, clock, || syscalls.fcntl_getfl_once(descriptor))?;
    let access = validate_access(flags)?;
    let logical_block_size = one_call_until(deadline, clock, || syscalls.block_logical_size_once(descriptor))?;
    let byte_length = one_call_until(deadline, clock, || syscalls.block_byte_length_once(descriptor))?;
    if logical_block_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained block device reported a zero logical block size",
        ));
    }
    if byte_length == 0 || byte_length > i64::MAX as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained block device reported a zero or unaddressable byte length",
        ));
    }
    checkpoint(deadline, clock)?;
    Ok(BlockDeviceObservation::new(
        node_kind,
        access,
        status.containing_device,
        status.inode,
        authenticated_mount_id,
        block_major,
        block_minor,
        logical_block_size,
        byte_length,
    ))
}

fn validate_status(status: RawBlockDeviceStat) -> io::Result<(ObservedNodeKind, u32, u32)> {
    if status.containing_device == 0 || status.inode == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained descriptor has a zero device or inode identity",
        ));
    }
    let kind = status.mode & nix::libc::S_IFMT;
    let node_kind = if kind == nix::libc::S_IFBLK {
        ObservedNodeKind::BlockDevice
    } else {
        ObservedNodeKind::Other
    };
    let major: u32 = nix::libc::major(status.raw_device);
    let minor: u32 = nix::libc::minor(status.raw_device);
    if nix::libc::makedev(major, minor) != status.raw_device {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained block-device rdev is not exactly representable",
        ));
    }
    Ok((node_kind, major, minor))
}

fn validate_access(flags: i32) -> io::Result<ObservedDeviceAccess> {
    if flags & nix::libc::O_PATH != 0 || flags & nix::libc::O_ACCMODE != nix::libc::O_RDONLY {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "retained block-device descriptor is not readable and read-only",
        ));
    }
    Ok(ObservedDeviceAccess::ReadOnly)
}

fn one_call_until<T>(
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    call: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    checkpoint(deadline, clock)?;
    let result = call();
    checkpoint(deadline, clock)?;
    result
}

fn checkpoint(deadline: Instant, clock: &mut impl FnMut() -> Instant) -> io::Result<()> {
    if clock() > deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "retained block-device observation exceeded its caller deadline",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::linux_fs) enum FixtureBlockDeviceSyscall {
    Fstat,
    FcntlGetFl,
    BlockLogicalSize,
    BlockByteLength,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::linux_fs) enum FixtureBlockDeviceSyscallResult {
    Stat {
        containing_device: u64,
        inode: u64,
        mode: u32,
        raw_device: u64,
    },
    Flags(i32),
    LogicalBlockSize(u32),
    ByteLength(u64),
}

#[cfg(test)]
struct FixtureSyscalls<'fixture, F> {
    respond: &'fixture mut F,
}

#[cfg(test)]
impl<F> ObservationSyscalls for FixtureSyscalls<'_, F>
where
    F: FnMut(FixtureBlockDeviceSyscall) -> io::Result<FixtureBlockDeviceSyscallResult>,
{
    fn fstat_once(&mut self, _descriptor: RawFd) -> io::Result<RawBlockDeviceStat> {
        match (self.respond)(FixtureBlockDeviceSyscall::Fstat)? {
            FixtureBlockDeviceSyscallResult::Stat {
                containing_device,
                inode,
                mode,
                raw_device,
            } => Ok(RawBlockDeviceStat {
                containing_device,
                inode,
                mode,
                raw_device,
            }),
            _ => Err(protocol_error()),
        }
    }

    fn fcntl_getfl_once(&mut self, _descriptor: RawFd) -> io::Result<i32> {
        match (self.respond)(FixtureBlockDeviceSyscall::FcntlGetFl)? {
            FixtureBlockDeviceSyscallResult::Flags(flags) => Ok(flags),
            _ => Err(protocol_error()),
        }
    }

    fn block_logical_size_once(&mut self, _descriptor: RawFd) -> io::Result<u32> {
        match (self.respond)(FixtureBlockDeviceSyscall::BlockLogicalSize)? {
            FixtureBlockDeviceSyscallResult::LogicalBlockSize(value) => Ok(value),
            _ => Err(protocol_error()),
        }
    }

    fn block_byte_length_once(&mut self, _descriptor: RawFd) -> io::Result<u64> {
        match (self.respond)(FixtureBlockDeviceSyscall::BlockByteLength)? {
            FixtureBlockDeviceSyscallResult::ByteLength(value) => Ok(value),
            _ => Err(protocol_error()),
        }
    }
}

#[cfg(test)]
fn protocol_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "injected retained-block-device syscall returned the wrong result kind",
    )
}

#[cfg(test)]
pub(in crate::linux_fs) fn observe_retained_block_device_fixture_with_clock_until(
    descriptor: BorrowedFd<'_>,
    authenticated_mount_id: u64,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    respond: &mut impl FnMut(FixtureBlockDeviceSyscall) -> io::Result<FixtureBlockDeviceSyscallResult>,
) -> io::Result<BlockDeviceObservation> {
    let mut syscalls = FixtureSyscalls { respond };
    observe_with_syscalls_and_clock_until(
        descriptor.as_raw_fd(),
        authenticated_mount_id,
        deadline,
        clock,
        &mut syscalls,
    )
}
