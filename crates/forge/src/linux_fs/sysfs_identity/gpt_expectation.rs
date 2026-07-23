//! Sealed sysfs facts required by a future authenticated GPT-device reader.
//!
//! This value is constructible only from one freshly revalidated retained
//! sysfs identity. It borrows the authenticated parent `DEVNAME`, copies only
//! closed scalar evidence from that same capture, and intentionally carries no
//! file, descriptor, path, reopen, discovery, or mutation capability.

use std::{marker::PhantomData, rc::Rc};

use super::{
    super::sysfs_block::{SysfsDeviceNumber, SysfsDiskSequence, SysfsPartitionNumber, SysfsPartitionUuid},
    RevalidatedSysfsPartitionIdentity,
};

/// Lifetime- and thread-bound expectation for one future GPT-device check.
///
/// All fields are private. In particular, callers cannot supply a parent
/// device number independently from the revalidated capture that supplied the
/// parent name and partition facts.
#[must_use = "authenticated sysfs-to-GPT expectations must remain bound to their revalidated view"]
pub(in crate::linux_fs) struct SysfsGptDeviceExpectation<'a> {
    authenticated_parent_devname: &'a [u8],
    parent_device: SysfsDeviceNumber,
    partition_number: SysfsPartitionNumber,
    partition_uuid: SysfsPartitionUuid,
    partition_start_512_sectors: u64,
    partition_size_512_sectors: u64,
    disk_sequence: Option<SysfsDiskSequence>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl<'a> SysfsGptDeviceExpectation<'a> {
    pub(super) fn from_revalidated(revalidated: &'a RevalidatedSysfsPartitionIdentity<'_>) -> Self {
        Self {
            authenticated_parent_devname: revalidated.current.parent_device_name(),
            parent_device: revalidated.current.parent_device(),
            partition_number: revalidated.current.partition_number(),
            partition_uuid: revalidated.current.partition_uuid(),
            partition_start_512_sectors: revalidated.current.partition_start_512_sectors(),
            partition_size_512_sectors: revalidated.current.partition_size_512_sectors(),
            disk_sequence: revalidated.current.disk_sequence(),
            _thread_bound: PhantomData,
        }
    }

    /// Borrow the validated relative kernel name for the whole-disk parent.
    pub(in crate::linux_fs) const fn authenticated_parent_devname(&self) -> &'a [u8] {
        self.authenticated_parent_devname
    }

    /// Exact major/minor captured from that same retained parent object.
    pub(in crate::linux_fs) const fn parent_device(&self) -> SysfsDeviceNumber {
        self.parent_device
    }

    pub(in crate::linux_fs) const fn partition_number(&self) -> SysfsPartitionNumber {
        self.partition_number
    }

    pub(in crate::linux_fs) const fn partition_uuid(&self) -> SysfsPartitionUuid {
        self.partition_uuid
    }

    pub(in crate::linux_fs) const fn partition_start_512_sectors(&self) -> u64 {
        self.partition_start_512_sectors
    }

    pub(in crate::linux_fs) const fn partition_size_512_sectors(&self) -> u64 {
        self.partition_size_512_sectors
    }

    pub(in crate::linux_fs) const fn disk_sequence(&self) -> Option<SysfsDiskSequence> {
        self.disk_sequence
    }
}

impl std::fmt::Debug for SysfsGptDeviceExpectation<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SysfsGptDeviceExpectation")
            .field("authenticated_parent_devname", &self.authenticated_parent_devname)
            .field("parent_device", &self.parent_device)
            .field("partition_number", &self.partition_number)
            .field("partition_uuid", &self.partition_uuid)
            .field("partition_start_512_sectors", &self.partition_start_512_sectors)
            .field("partition_size_512_sectors", &self.partition_size_512_sectors)
            .field("disk_sequence", &self.disk_sequence)
            .finish_non_exhaustive()
    }
}
