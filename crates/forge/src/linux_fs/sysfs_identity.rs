//! Descriptor-retained authentication of Linux sysfs partition identity.
//!
//! A successful revalidation produces one descriptor-authenticated snapshot
//! of a kernel block object. Two views can compare whether their snapshots
//! captured the same authenticated block-parent evidence, but that comparison
//! does not prove either public name remains live at call time or that both
//! names were resident simultaneously. This layer also does **not** prove GPT
//! roles, filesystem identity, physical-disk identity, persistence or
//! durability, and it grants no mutation authority.
//! Thread binding prevents cross-thread namespace confusion, but this layer
//! does not prove that its owning thread avoided `setns(2)` between calls. A
//! caller which combines mount-local evidence must sandwich these operations
//! with its own retained mount-namespace witness.

use std::{
    io,
    marker::PhantomData,
    rc::Rc,
    time::{Duration, Instant},
};

use super::sysfs_block::{SysfsDeviceNumber, SysfsDiskSequence, SysfsPartitionNumber, SysfsPartitionUuid};

mod capture;
mod filesystem;
mod gpt_expectation;

pub(in crate::linux_fs) use gpt_expectation::SysfsGptDeviceExpectation;

use capture::{Capture, capture_twice, require_capture_matches};
use filesystem::{Operation, RootHandle, SysfsIdentityLimits};

const PRODUCTION_TIMEOUT: Duration = Duration::from_secs(30);
const PRODUCTION_MAX_WORK: usize = 128 * 1024 * 1024;
const PRODUCTION_MAX_ANCESTORS: usize = 128;
const PRODUCTION_MAX_DESCRIPTORS: usize = 32_768;

// Conservative operation-wide feasibility proof for the advertised ancestor
// ceiling. Eighteen witnesses per possible component overbounds preparation
// and the larger revalidation flow: retained-chain entry checks, two complete
// captures, terminal classification, two final chains, and fixed attributes.
// Every witness reserves ten descriptor opens plus distinct 16-KiB fdinfo
// read and parse passes, while the remaining terms overbound direct opens,
// link reads, and parser work.
const WORST_CASE_WITNESSES: usize = 18 * (PRODUCTION_MAX_ANCESTORS + 1);
const REQUIRED_DESCRIPTOR_UNITS: usize = WORST_CASE_WITNESSES * 10 + (PRODUCTION_MAX_ANCESTORS + 1) * 16;
const REQUIRED_WORK_UNITS: usize =
    WORST_CASE_WITNESSES * (2 * 16 * 1024 + 128) + (PRODUCTION_MAX_ANCESTORS + 1) * 3 * 64 * 1024 + 16 * 1024 * 1024;
const _: () = assert!(PRODUCTION_MAX_DESCRIPTORS >= REQUIRED_DESCRIPTOR_UNITS);
const _: () = assert!(PRODUCTION_MAX_WORK >= REQUIRED_WORK_UNITS);

const PRODUCTION_LIMITS: SysfsIdentityLimits = SysfsIdentityLimits {
    max_work: PRODUCTION_MAX_WORK,
    max_ancestors: PRODUCTION_MAX_ANCESTORS,
    max_descriptors: PRODUCTION_MAX_DESCRIPTORS,
};

/// Retained partition evidence which must be revalidated before inspection.
///
/// This value is deliberately thread-bound. Sysfs mount IDs are interpreted
/// in the mount namespace of the thread which performed the capture.
pub(crate) struct PreparedSysfsPartitionIdentity {
    root: RootHandle,
    capture: Capture,
    _thread_bound: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for PreparedSysfsPartitionIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedSysfsPartitionIdentity")
            .field("evidence", &"retained; revalidation required")
            .finish_non_exhaustive()
    }
}

impl PreparedSysfsPartitionIdentity {
    /// Prepare one partition from the fixed production `/sys` mount.
    ///
    /// The lookup is exactly `/sys/dev/block/<major>:<minor>`; this method
    /// never enumerates sysfs and never opens a block device.
    pub(crate) fn prepare(device: SysfsDeviceNumber) -> io::Result<Self> {
        let deadline = deadline_after(PRODUCTION_TIMEOUT)?;
        Self::prepare_until(device, deadline)
    }

    /// Prepare one partition under the caller's absolute deadline.
    ///
    /// The supplied deadline is shared by every open, read, parse, and
    /// terminal consistency check. It is never replaced with a fresh timeout.
    pub(crate) fn prepare_until(device: SysfsDeviceNumber, deadline: Instant) -> io::Result<Self> {
        let mut operation = Operation::production(PRODUCTION_LIMITS, deadline);
        let root = RootHandle::open_production(&mut operation)?;
        Self::prepare_from_root(root, device, &mut operation)
    }

    fn prepare_from_root(
        root: RootHandle,
        device: SysfsDeviceNumber,
        operation: &mut Operation<'_>,
    ) -> io::Result<Self> {
        let capture = capture_twice(&root, device, operation)?;
        root.require_named(operation)?;
        capture.require_terminal_names(&root, operation)?;
        // No prepared identity may escape after the caller's absolute
        // deadline, including expiry immediately after the final name check.
        operation.checkpoint()?;
        Ok(Self {
            root,
            capture,
            _thread_bound: PhantomData,
        })
    }

    /// Revalidate the complete retained identity twice under one finite
    /// deadline and return a lifetime-bound semantic view.
    pub(crate) fn revalidate(&self) -> io::Result<RevalidatedSysfsPartitionIdentity<'_>> {
        let deadline = deadline_after(PRODUCTION_TIMEOUT)?;
        self.revalidate_until(deadline)
    }

    /// Revalidate under the caller's absolute deadline without resetting it.
    pub(crate) fn revalidate_until(&self, deadline: Instant) -> io::Result<RevalidatedSysfsPartitionIdentity<'_>> {
        let mut operation = Operation::production(PRODUCTION_LIMITS, deadline);
        self.revalidate_with_operation(&mut operation)
    }

    fn revalidate_with_operation<'a>(
        &'a self,
        operation: &mut Operation<'_>,
    ) -> io::Result<RevalidatedSysfsPartitionIdentity<'a>> {
        self.root.require_named(operation)?;
        self.capture.require_retained(&self.root, operation)?;
        let current = capture_twice(&self.root, self.capture.device(), operation)?;
        require_capture_matches(&self.capture, &current)?;
        self.root.require_named(operation)?;
        current.require_terminal_names(&self.root, operation)?;
        // This is deliberately after all retained and public-name evidence;
        // terminal expiry must fail rather than returning a stale view.
        operation.checkpoint()?;
        Ok(RevalidatedSysfsPartitionIdentity {
            prepared: self,
            current,
        })
    }

    #[cfg(test)]
    pub(crate) fn revalidate_with(
        &self,
        limits: FixtureSysfsIdentityLimits,
        deadline: Instant,
        hook: &mut impl FnMut(FixtureCheckpoint) -> io::Result<()>,
    ) -> io::Result<RevalidatedSysfsPartitionIdentity<'_>> {
        let mut operation = Operation::fixture(limits.into(), deadline, hook)?;
        self.revalidate_with_operation(&mut operation)
    }

    #[cfg(test)]
    pub(crate) fn revalidate_with_clock(
        &self,
        limits: FixtureSysfsIdentityLimits,
        deadline: Instant,
        hook: &mut impl FnMut(FixtureCheckpoint) -> io::Result<()>,
        clock: &mut impl FnMut() -> Instant,
    ) -> io::Result<RevalidatedSysfsPartitionIdentity<'_>> {
        let mut operation = Operation::fixture_with_clock(limits.into(), deadline, hook, clock)?;
        self.revalidate_with_operation(&mut operation)
    }
}

/// A freshly revalidated, semantic-only view of retained sysfs evidence.
pub(crate) struct RevalidatedSysfsPartitionIdentity<'a> {
    prepared: &'a PreparedSysfsPartitionIdentity,
    current: Capture,
}

impl std::fmt::Debug for RevalidatedSysfsPartitionIdentity<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevalidatedSysfsPartitionIdentity")
            .field("device", &self.device())
            .field("partition_number", &self.partition_number())
            .field("partition_uuid", &self.partition_uuid())
            .field("disk_sequence", &self.disk_sequence())
            .field("partition_device_name", &self.partition_device_name())
            .field("parent_device_name", &self.parent_device_name())
            .field("partition_start_512_sectors", &self.partition_start_512_sectors())
            .field("partition_size_512_sectors", &self.partition_size_512_sectors())
            .finish_non_exhaustive()
    }
}

impl RevalidatedSysfsPartitionIdentity<'_> {
    pub(crate) const fn device(&self) -> SysfsDeviceNumber {
        self.current.device()
    }

    pub(crate) const fn partition_number(&self) -> SysfsPartitionNumber {
        self.current.partition_number()
    }

    pub(crate) const fn partition_uuid(&self) -> SysfsPartitionUuid {
        self.current.partition_uuid()
    }

    pub(crate) const fn disk_sequence(&self) -> Option<SysfsDiskSequence> {
        self.current.disk_sequence()
    }

    /// Return the normalized logical `/sys/devices/...` bytes.
    ///
    /// These bytes are descriptive evidence, not path or mutation authority.
    pub(crate) fn normalized_devpath(&self) -> &[u8] {
        self.current.normalized_devpath()
    }

    /// Return the validated kernel name for the partition block object.
    ///
    /// This is descriptive relative-locator evidence only. It carries no
    /// descriptor, open, reopen, path-resolution, or mutation authority.
    pub(crate) fn partition_device_name(&self) -> &[u8] {
        self.current.partition_device_name()
    }

    /// Return the validated kernel name for the retained whole-disk parent.
    ///
    /// This is descriptive relative-locator evidence only. It carries no
    /// descriptor, open, reopen, path-resolution, or mutation authority.
    pub(crate) fn parent_device_name(&self) -> &[u8] {
        self.current.parent_device_name()
    }

    /// Return the stable partition start reported by sysfs in 512-byte sectors.
    ///
    /// This does not prove a GPT role or the parent disk's logical block size.
    pub(crate) const fn partition_start_512_sectors(&self) -> u64 {
        self.current.partition_start_512_sectors()
    }

    /// Return the stable partition size reported by sysfs in 512-byte sectors.
    ///
    /// This does not prove a GPT role or the parent disk's logical block size.
    pub(crate) const fn partition_size_512_sectors(&self) -> u64 {
        self.current.partition_size_512_sectors()
    }

    /// Bind the GPT-device facts from this exact revalidated capture.
    ///
    /// The result borrows the authenticated parent `DEVNAME` from this view,
    /// remains thread-bound, and carries no descriptor, path, reopen, or
    /// mutation authority. Its private constructor does not accept a second
    /// parent device number which could disagree with the retained capture.
    pub(in crate::linux_fs) fn gpt_device_expectation(&self) -> SysfsGptDeviceExpectation<'_> {
        SysfsGptDeviceExpectation::from_revalidated(self)
    }

    /// Compare the authenticated block-parent evidence in two revalidated
    /// snapshots.
    ///
    /// Equality does not prove that either lookup name remains live when this
    /// method is called or that both partitions were resident simultaneously.
    /// A topology consumer must sandwich both prepared identities under one
    /// retained namespace/topology epoch. This also does not establish
    /// physical-disk identity, GPT/filesystem roles, durability, or permission
    /// to mutate either object.
    pub(crate) fn has_same_revalidated_block_parent_snapshot(&self, other: &Self) -> bool {
        self.current.has_same_parent_snapshot(&other.current)
    }

    #[allow(dead_code)]
    pub(super) const fn prepared(&self) -> &PreparedSysfsPartitionIdentity {
        self.prepared
    }
}

fn deadline_after(timeout: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "sysfs identity deadline overflowed"))
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FixtureSysfsIdentityLimits {
    pub(crate) max_work: usize,
    pub(crate) max_ancestors: usize,
    pub(crate) max_descriptors: usize,
}

#[cfg(test)]
impl Default for FixtureSysfsIdentityLimits {
    fn default() -> Self {
        Self {
            max_work: PRODUCTION_MAX_WORK,
            max_ancestors: PRODUCTION_MAX_ANCESTORS,
            max_descriptors: PRODUCTION_MAX_DESCRIPTORS,
        }
    }
}

#[cfg(test)]
impl From<FixtureSysfsIdentityLimits> for SysfsIdentityLimits {
    fn from(limits: FixtureSysfsIdentityLimits) -> Self {
        Self {
            max_work: limits.max_work,
            max_ancestors: limits.max_ancestors,
            max_descriptors: limits.max_descriptors,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixtureNode {
    Partition,
    Parent,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixtureAttribute {
    Dev,
    Partition,
    Start,
    Size,
    Uevent,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixtureCheckpoint {
    RootRebind,
    LookupPinned,
    LookupRebound,
    TargetPinned,
    AttributePinned {
        node: FixtureNode,
        attribute: FixtureAttribute,
    },
    AttributeRead {
        node: FixtureNode,
        attribute: FixtureAttribute,
    },
    AttributeRebound {
        node: FixtureNode,
        attribute: FixtureAttribute,
    },
    SubsystemPinned {
        depth: usize,
    },
    SubsystemRead {
        depth: usize,
    },
    SubsystemRebound {
        depth: usize,
    },
    AncestorExamined {
        depth: usize,
    },
    ParentSelected {
        depth: usize,
    },
    TerminalRebind,
    FinalNameRebind,
}

/// Test-only admission of an ordinary, test-owned synthetic sysfs tree.
#[cfg(test)]
pub(crate) struct FixtureSysfsTree {
    root: RootHandle,
    _thread_bound: PhantomData<Rc<()>>,
}

#[cfg(test)]
impl FixtureSysfsTree {
    pub(crate) fn admit(parent: std::fs::File, root_name: std::ffi::CString) -> io::Result<Self> {
        let deadline = deadline_after(PRODUCTION_TIMEOUT)?;
        let mut operation = Operation::fixture_without_hook(PRODUCTION_LIMITS, deadline)?;
        let root = RootHandle::admit_fixture(parent, root_name, &mut operation)?;
        Ok(Self {
            root,
            _thread_bound: PhantomData,
        })
    }

    pub(crate) fn prepare(&self, device: SysfsDeviceNumber) -> io::Result<PreparedSysfsPartitionIdentity> {
        let deadline = deadline_after(PRODUCTION_TIMEOUT)?;
        let mut operation = Operation::fixture_without_hook(PRODUCTION_LIMITS, deadline)?;
        let root = self.root.reopen_owned(&mut operation)?;
        PreparedSysfsPartitionIdentity::prepare_from_root(root, device, &mut operation)
    }

    pub(crate) fn prepare_with(
        &self,
        device: SysfsDeviceNumber,
        limits: FixtureSysfsIdentityLimits,
        deadline: Instant,
        hook: &mut impl FnMut(FixtureCheckpoint) -> io::Result<()>,
    ) -> io::Result<PreparedSysfsPartitionIdentity> {
        let mut operation = Operation::fixture(limits.into(), deadline, hook)?;
        let root = self.root.reopen_owned(&mut operation)?;
        PreparedSysfsPartitionIdentity::prepare_from_root(root, device, &mut operation)
    }

    pub(crate) fn prepare_with_clock(
        &self,
        device: SysfsDeviceNumber,
        limits: FixtureSysfsIdentityLimits,
        deadline: Instant,
        hook: &mut impl FnMut(FixtureCheckpoint) -> io::Result<()>,
        clock: &mut impl FnMut() -> Instant,
    ) -> io::Result<PreparedSysfsPartitionIdentity> {
        let mut operation = Operation::fixture_with_clock(limits.into(), deadline, hook, clock)?;
        let root = self.root.reopen_owned(&mut operation)?;
        PreparedSysfsPartitionIdentity::prepare_from_root(root, device, &mut operation)
    }
}
