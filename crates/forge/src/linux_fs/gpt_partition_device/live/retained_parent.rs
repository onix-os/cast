//! Retained, descriptor-relative parent block-node capability.
//!
//! Construction starts from an already authenticated devtmpfs root descriptor
//! and one sealed sysfs expectation. The exact authenticated `DEVNAME` bytes
//! are resolved only beneath that root with `openat2(2)`. The resulting block
//! descriptor is read-only and retained privately; callers receive neither a
//! raw descriptor nor a name/path getter.

use std::{
    ffi::{CStr, CString},
    fs::File,
    io,
    marker::PhantomData,
    os::fd::{AsFd as _, AsRawFd as _},
    rc::Rc,
    time::Instant,
};

use crate::linux_fs::{descriptor_mount_id_until, openat2_file_until};

use super::{
    super::{BlockDeviceObservation, BlockDeviceObserver, ObservedDeviceAccess, ObservedNodeKind},
    observation::RetainedBlockDeviceObserver,
};
use crate::linux_fs::sysfs_identity::SysfsGptDeviceExpectation;

const PARENT_OPEN_FLAGS: i32 =
    nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK;
const PARENT_OPEN_RESOLUTION: u64 = (nix::libc::RESOLVE_BENEATH
    | nix::libc::RESOLVE_NO_MAGICLINKS
    | nix::libc::RESOLVE_NO_SYMLINKS
    | nix::libc::RESOLVE_NO_XDEV) as u64;
const SYSFS_SECTOR_BYTES: u64 = 512;
const MIN_LOGICAL_BLOCK_SIZE: u32 = 512;
const MAX_LOGICAL_BLOCK_SIZE: u32 = 65_536;
const MAX_DEVICE_NAME_BYTES: usize = 4 * 1024 - 1;
const MAX_DEVICE_NAME_COMPONENTS: usize = 128;
const MAX_DEVICE_NAME_COMPONENT_BYTES: usize = 255;

/// Sealed scalar snapshot of the exact opening retained by the parent owner.
///
/// The tuple field is private so sibling modules can compare against this
/// opening but cannot construct a production token from unrelated evidence.
#[derive(Clone, Copy)]
pub(super) struct CanonicalRetainedParentOpening(BlockDeviceObservation);

impl CanonicalRetainedParentOpening {
    pub(super) const fn observation(self) -> BlockDeviceObservation {
        self.0
    }

    #[cfg(test)]
    pub(super) const fn fixture(observation: BlockDeviceObservation) -> Self {
        Self(observation)
    }
}

/// Private operation authority over one retained read-only parent block node.
///
/// The capability is intentionally neither clonable nor debuggable. Its only
/// operation-bearing output is a same-descriptor observer restricted to the
/// enclosing Linux filesystem implementation. The marker preserves the
/// current-thread mount-namespace affinity of the sealed sysfs expectation.
pub(in crate::linux_fs) struct RetainedGptParentBlockDevice<'root, 'expectation> {
    devtmpfs_root: &'root File,
    descriptor: File,
    expectation: ParentExpectation<'expectation>,
    root_mount_id: u64,
    opening: BlockDeviceObservation,
    _thread_bound: PhantomData<Rc<()>>,
}

impl RetainedGptParentBlockDevice<'_, '_> {
    /// Return the sealed scalar opening captured while this owner was retained.
    pub(super) const fn canonical_opening(&self) -> CanonicalRetainedParentOpening {
        CanonicalRetainedParentOpening(self.opening)
    }

    /// Borrow an observer over the exact retained descriptor.
    ///
    /// No descriptor, path, or device name crosses this private seam.
    pub(in crate::linux_fs) fn same_descriptor_observer(&self) -> io::Result<RetainedBlockDeviceObserver<'_>> {
        RetainedBlockDeviceObserver::new(self.descriptor.as_fd(), self.opening.mount_id())
    }

    /// Re-open the exact authenticated name and require full observation equality.
    pub(in crate::linux_fs) fn rebind_same_name_until(&self, deadline: Instant) -> io::Result<()> {
        let mut protocol = LinuxRetainedParentProtocol;
        let mut clock = Instant::now;
        self.rebind_with_protocol_and_clock_until(deadline, &mut clock, &mut protocol)
    }

    /// Perform the terminal same-name rebind and consume all retained authority.
    pub(in crate::linux_fs) fn closing_rebind_until(self, deadline: Instant) -> io::Result<()> {
        let mut protocol = LinuxRetainedParentProtocol;
        let mut clock = Instant::now;
        self.rebind_with_protocol_and_clock_until(deadline, &mut clock, &mut protocol)
    }

    fn rebind_with_protocol_and_clock_until(
        &self,
        deadline: Instant,
        clock: &mut impl FnMut() -> Instant,
        protocol: &mut impl RetainedParentProtocol,
    ) -> io::Result<()> {
        let (_, rebound) = open_and_observe_until(
            self.devtmpfs_root,
            self.root_mount_id,
            self.expectation,
            deadline,
            clock,
            protocol,
        )?;
        checkpoint(deadline, clock)?;
        if rebound != self.opening {
            return Err(invalid(
                "same-name parent block-device identity, access, or geometry changed",
            ));
        }
        checkpoint(deadline, clock)
    }
}

/// Retain one exact sysfs-selected parent block node below authenticated devtmpfs.
pub(super) fn retain_gpt_parent_block_device_until<'root, 'expectation>(
    devtmpfs_root: &'root File,
    authenticated_root_mount_id: u64,
    expected: &SysfsGptDeviceExpectation<'expectation>,
    deadline: Instant,
) -> io::Result<RetainedGptParentBlockDevice<'root, 'expectation>> {
    let expectation = ParentExpectation::from_sysfs(expected);
    let mut protocol = LinuxRetainedParentProtocol;
    let mut clock = Instant::now;
    retain_with_protocol_and_clock_until(
        devtmpfs_root,
        authenticated_root_mount_id,
        expectation,
        deadline,
        &mut clock,
        &mut protocol,
    )
}

#[derive(Clone, Copy)]
struct ParentExpectation<'name> {
    name: &'name [u8],
    parent_major: u32,
    parent_minor: u32,
    partition_start_512_sectors: u64,
    partition_size_512_sectors: u64,
}

impl<'name> ParentExpectation<'name> {
    fn from_sysfs(expected: &SysfsGptDeviceExpectation<'name>) -> Self {
        let parent = expected.parent_device();
        Self {
            name: expected.authenticated_parent_devname(),
            parent_major: parent.major(),
            parent_minor: parent.minor(),
            partition_start_512_sectors: expected.partition_start_512_sectors(),
            partition_size_512_sectors: expected.partition_size_512_sectors(),
        }
    }
}

fn retain_with_protocol_and_clock_until<'root, 'expectation>(
    devtmpfs_root: &'root File,
    authenticated_root_mount_id: u64,
    expectation: ParentExpectation<'expectation>,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    protocol: &mut impl RetainedParentProtocol,
) -> io::Result<RetainedGptParentBlockDevice<'root, 'expectation>> {
    checkpoint(deadline, clock)?;
    if authenticated_root_mount_id == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "authenticated devtmpfs root mount ID must be nonzero",
        ));
    }
    let (descriptor, opening) = open_and_observe_until(
        devtmpfs_root,
        authenticated_root_mount_id,
        expectation,
        deadline,
        clock,
        protocol,
    )?;
    checkpoint(deadline, clock)?;
    Ok(RetainedGptParentBlockDevice {
        devtmpfs_root,
        descriptor,
        expectation,
        root_mount_id: authenticated_root_mount_id,
        opening,
        _thread_bound: PhantomData,
    })
}

fn open_and_observe_until(
    devtmpfs_root: &File,
    authenticated_root_mount_id: u64,
    expectation: ParentExpectation<'_>,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    protocol: &mut impl RetainedParentProtocol,
) -> io::Result<(File, BlockDeviceObservation)> {
    checkpoint(deadline, clock)?;
    let name = exact_relative_name(expectation.name)?;
    checkpoint(deadline, clock)?;

    let descriptor = one_call_until(deadline, clock, || {
        protocol.open_relative_once(
            devtmpfs_root,
            &name,
            PARENT_OPEN_FLAGS,
            0,
            PARENT_OPEN_RESOLUTION,
            deadline,
        )
    })?;
    let mount_id = one_call_until(deadline, clock, || {
        protocol.descriptor_mount_id_once(&descriptor, deadline)
    })?;
    if mount_id != authenticated_root_mount_id {
        return Err(invalid(
            "parent block node did not remain on the authenticated devtmpfs root mount",
        ));
    }
    let observation = one_call_until(deadline, clock, || {
        protocol.observe_once(&descriptor, mount_id, deadline)
    })?;
    if observation.mount_id() != mount_id {
        return Err(invalid(
            "parent block-device observation changed the authenticated descriptor mount ID",
        ));
    }
    preflight_observation_until(observation, expectation, deadline, clock)?;
    Ok((descriptor, observation))
}

fn exact_relative_name(bytes: &[u8]) -> io::Result<CString> {
    if bytes.is_empty() || bytes.len() > MAX_DEVICE_NAME_BYTES || bytes[0] == b'/' || bytes.contains(&0) {
        return Err(invalid(
            "authenticated parent DEVNAME is not one bounded relative locator",
        ));
    }
    let mut components = 0usize;
    for component in bytes.split(|byte| *byte == b'/') {
        components = components
            .checked_add(1)
            .ok_or_else(|| invalid("parent DEVNAME component count overflowed"))?;
        if components > MAX_DEVICE_NAME_COMPONENTS
            || component.is_empty()
            || component.len() > MAX_DEVICE_NAME_COMPONENT_BYTES
            || component == b"."
            || component == b".."
        {
            return Err(invalid(
                "authenticated parent DEVNAME contains an inadmissible component",
            ));
        }
    }
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(bytes.len().saturating_add(1))
        .map_err(|source| io::Error::other(format!("allocating authenticated parent DEVNAME failed: {source}")))?;
    owned.extend_from_slice(bytes);
    CString::new(owned).map_err(|_| invalid("authenticated parent DEVNAME contains NUL"))
}

fn preflight_observation_until(
    observation: BlockDeviceObservation,
    expectation: ParentExpectation<'_>,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> io::Result<()> {
    checkpoint(deadline, clock)?;
    if observation.node_kind() != ObservedNodeKind::BlockDevice {
        return Err(invalid("retained parent node is not a block device"));
    }
    if observation.access() != ObservedDeviceAccess::ReadOnly {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "retained parent block-device descriptor is write-capable",
        ));
    }
    if observation.containing_device() == 0 || observation.inode() == 0 || observation.mount_id() == 0 {
        return Err(invalid("retained parent block-device identity contains a zero scalar"));
    }
    if (observation.block_major(), observation.block_minor()) != (expectation.parent_major, expectation.parent_minor) {
        return Err(invalid(
            "retained block node rdev disagrees with authenticated sysfs parent",
        ));
    }

    let logical_block_size = observation.logical_block_size();
    if !(MIN_LOGICAL_BLOCK_SIZE..=MAX_LOGICAL_BLOCK_SIZE).contains(&logical_block_size)
        || !logical_block_size.is_power_of_two()
    {
        return Err(invalid("block device reports an unsupported logical block size"));
    }
    let logical_block_size = u64::from(logical_block_size);
    let device_bytes = observation.byte_length();
    if device_bytes == 0 || device_bytes > i64::MAX as u64 || device_bytes % logical_block_size != 0 {
        return Err(invalid(
            "block-device capacity is zero, unaddressable, or not logical-block aligned",
        ));
    }

    let partition_start = expectation
        .partition_start_512_sectors
        .checked_mul(SYSFS_SECTOR_BYTES)
        .ok_or_else(|| invalid("sysfs partition start overflows bytes"))?;
    let partition_size = expectation
        .partition_size_512_sectors
        .checked_mul(SYSFS_SECTOR_BYTES)
        .ok_or_else(|| invalid("sysfs partition size overflows bytes"))?;
    if partition_size == 0 || partition_start % logical_block_size != 0 || partition_size % logical_block_size != 0 {
        return Err(invalid(
            "sysfs partition geometry is empty or not aligned to the observed logical block size",
        ));
    }
    let partition_end = partition_start
        .checked_add(partition_size)
        .ok_or_else(|| invalid("sysfs partition byte range overflows"))?;
    if partition_end > device_bytes {
        return Err(invalid(
            "sysfs partition byte range exceeds the observed parent capacity",
        ));
    }
    checkpoint(deadline, clock)
}

trait RetainedParentProtocol {
    fn open_relative_once(
        &mut self,
        root: &File,
        name: &CStr,
        flags: i32,
        mode: u32,
        resolve: u64,
        deadline: Instant,
    ) -> io::Result<File>;

    fn descriptor_mount_id_once(&mut self, descriptor: &File, deadline: Instant) -> io::Result<u64>;

    fn observe_once(
        &mut self,
        descriptor: &File,
        authenticated_mount_id: u64,
        deadline: Instant,
    ) -> io::Result<BlockDeviceObservation>;
}

struct LinuxRetainedParentProtocol;

impl RetainedParentProtocol for LinuxRetainedParentProtocol {
    fn open_relative_once(
        &mut self,
        root: &File,
        name: &CStr,
        flags: i32,
        mode: u32,
        resolve: u64,
        deadline: Instant,
    ) -> io::Result<File> {
        openat2_file_until(root.as_raw_fd(), name, flags, mode, resolve, deadline)
    }

    fn descriptor_mount_id_once(&mut self, descriptor: &File, deadline: Instant) -> io::Result<u64> {
        descriptor_mount_id_until(descriptor, deadline)
    }

    fn observe_once(
        &mut self,
        descriptor: &File,
        authenticated_mount_id: u64,
        deadline: Instant,
    ) -> io::Result<BlockDeviceObservation> {
        let mut observer = RetainedBlockDeviceObserver::new(descriptor.as_fd(), authenticated_mount_id)?;
        observer.observe_until(deadline)
    }
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
            "retained parent block-node protocol exceeded its caller deadline",
        ))
    } else {
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::linux_fs) enum FixtureRetainedParentProtocolCall {
    Open {
        name: Vec<u8>,
        flags: i32,
        mode: u32,
        resolve: u64,
        deadline: Instant,
    },
    DescriptorMountId {
        deadline: Instant,
    },
    Observe {
        authenticated_mount_id: u64,
        deadline: Instant,
    },
}

#[cfg(test)]
pub(in crate::linux_fs) enum FixtureRetainedParentProtocolResult {
    Opened(File),
    DescriptorMountId(u64),
    Observation(BlockDeviceObservation),
}

#[cfg(test)]
struct FixtureRetainedParentProtocol<'fixture, F> {
    respond: &'fixture mut F,
}

#[cfg(test)]
impl<F> RetainedParentProtocol for FixtureRetainedParentProtocol<'_, F>
where
    F: FnMut(FixtureRetainedParentProtocolCall) -> io::Result<FixtureRetainedParentProtocolResult>,
{
    fn open_relative_once(
        &mut self,
        _root: &File,
        name: &CStr,
        flags: i32,
        mode: u32,
        resolve: u64,
        deadline: Instant,
    ) -> io::Result<File> {
        match (self.respond)(FixtureRetainedParentProtocolCall::Open {
            name: name.to_bytes().to_vec(),
            flags,
            mode,
            resolve,
            deadline,
        })? {
            FixtureRetainedParentProtocolResult::Opened(file) => Ok(file),
            _ => Err(fixture_protocol_error()),
        }
    }

    fn descriptor_mount_id_once(&mut self, _descriptor: &File, deadline: Instant) -> io::Result<u64> {
        match (self.respond)(FixtureRetainedParentProtocolCall::DescriptorMountId { deadline })? {
            FixtureRetainedParentProtocolResult::DescriptorMountId(mount_id) => Ok(mount_id),
            _ => Err(fixture_protocol_error()),
        }
    }

    fn observe_once(
        &mut self,
        _descriptor: &File,
        authenticated_mount_id: u64,
        deadline: Instant,
    ) -> io::Result<BlockDeviceObservation> {
        match (self.respond)(FixtureRetainedParentProtocolCall::Observe {
            authenticated_mount_id,
            deadline,
        })? {
            FixtureRetainedParentProtocolResult::Observation(observation) => Ok(observation),
            _ => Err(fixture_protocol_error()),
        }
    }
}

#[cfg(test)]
fn fixture_protocol_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "injected retained-parent protocol returned the wrong result kind",
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(in crate::linux_fs) fn retain_gpt_parent_block_device_fixture_with_clock_until<'root, 'name>(
    devtmpfs_root: &'root File,
    authenticated_root_mount_id: u64,
    name: &'name [u8],
    parent_major: u32,
    parent_minor: u32,
    partition_start_512_sectors: u64,
    partition_size_512_sectors: u64,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    respond: &mut impl FnMut(FixtureRetainedParentProtocolCall) -> io::Result<FixtureRetainedParentProtocolResult>,
) -> io::Result<RetainedGptParentBlockDevice<'root, 'name>> {
    let expectation = ParentExpectation {
        name,
        parent_major,
        parent_minor,
        partition_start_512_sectors,
        partition_size_512_sectors,
    };
    let mut protocol = FixtureRetainedParentProtocol { respond };
    retain_with_protocol_and_clock_until(
        devtmpfs_root,
        authenticated_root_mount_id,
        expectation,
        deadline,
        clock,
        &mut protocol,
    )
}

#[cfg(test)]
pub(in crate::linux_fs) fn rebind_retained_gpt_parent_fixture_with_clock_until(
    retained: &RetainedGptParentBlockDevice<'_, '_>,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    respond: &mut impl FnMut(FixtureRetainedParentProtocolCall) -> io::Result<FixtureRetainedParentProtocolResult>,
) -> io::Result<()> {
    let mut protocol = FixtureRetainedParentProtocol { respond };
    retained.rebind_with_protocol_and_clock_until(deadline, clock, &mut protocol)
}

#[cfg(test)]
pub(in crate::linux_fs) fn close_retained_gpt_parent_fixture_with_clock_until(
    retained: RetainedGptParentBlockDevice<'_, '_>,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    respond: &mut impl FnMut(FixtureRetainedParentProtocolCall) -> io::Result<FixtureRetainedParentProtocolResult>,
) -> io::Result<()> {
    let mut protocol = FixtureRetainedParentProtocol { respond };
    retained.rebind_with_protocol_and_clock_until(deadline, clock, &mut protocol)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(in crate::linux_fs) fn retain_gpt_parent_block_device_linux_fixture_until<'root, 'name>(
    devtmpfs_root: &'root File,
    authenticated_root_mount_id: u64,
    name: &'name [u8],
    parent_major: u32,
    parent_minor: u32,
    partition_start_512_sectors: u64,
    partition_size_512_sectors: u64,
    deadline: Instant,
) -> io::Result<RetainedGptParentBlockDevice<'root, 'name>> {
    let expectation = ParentExpectation {
        name,
        parent_major,
        parent_minor,
        partition_start_512_sectors,
        partition_size_512_sectors,
    };
    let mut protocol = LinuxRetainedParentProtocol;
    let mut clock = Instant::now;
    retain_with_protocol_and_clock_until(
        devtmpfs_root,
        authenticated_root_mount_id,
        expectation,
        deadline,
        &mut clock,
        &mut protocol,
    )
}
