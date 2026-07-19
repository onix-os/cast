//! Pure foundation for reconciling a retained, read-only GPT parent device.
//!
//! This module compares exact opening/closing block-node observations with sealed
//! sysfs expectations and already authenticated GPT table evidence. It
//! reconciles Linux's fixed 512-byte partition geometry with GPT logical-block
//! geometry and returns only closed scalars. Actual descriptor opening,
//! `fstat`, mount-ID capture, block-device queries, and reads remain behind a
//! private observer seam for a later production adapter; this module performs no
//! discovery, I/O, mount, write, or disk operation itself.
//!
//! This pure layer cannot prove that caller-supplied GPT evidence was read
//! from the observed descriptor. A live authority must run the GPT parser
//! between its opening and closing descriptor observations before construction
//! can be treated as authentication. Optional sysfs `DISKSEQ` evidence is
//! deliberately not returned: Linux 5.6 has no matching descriptor-bound disk
//! sequence query in this project's baseline. The future live adapter must use
//! the private GPT inter-pass callback so it can revalidate node identity,
//! device geometry, and name binding between parser passes.

use std::{io, time::Instant};

mod budget;
mod geometry;
mod input;
mod live;
mod observation;
mod stable;

use super::{
    gpt_partition_role::{GptPartitionRole, ValidatedGptPartitionRole},
    sysfs_identity::SysfsGptDeviceExpectation,
};
#[allow(unused_imports)] // retained syscall/image foundations for the production composition adapter
pub(in crate::linux_fs) use live::{RetainedBlockDeviceObserver, RetainedReadOnlyBlockImage};
pub(in crate::linux_fs) use observation::BlockDeviceObserver;
#[allow(unused_imports)] // sealed vocabulary for the later descriptor/syscall adapter
pub(in crate::linux_fs) use observation::{BlockDeviceObservation, ObservedDeviceAccess, ObservedNodeKind};

#[cfg(test)]
pub(in crate::linux_fs) use live::{
    FixtureBlockDeviceSyscall, FixtureBlockDeviceSyscallResult, fixture_block_ioctl_requests,
    observe_retained_block_device_fixture_with_clock_until, retained_read_only_block_image_fixture_until,
};

/// Closed scalar evidence for one stable read-only parent and GPT partition.
///
/// No descriptor, path, image, buffer, observer, or reusable operation
/// authority survives reconciliation. The value is not read-provenance proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::linux_fs) struct ReconciledGptPartitionDeviceEvidence {
    containing_device: u64,
    inode: u64,
    mount_id: u64,
    parent_major: u32,
    parent_minor: u32,
    logical_block_size: u32,
    device_byte_length: u64,
    partition_number: u32,
    partition_uuid: [u8; 36],
    partition_start_bytes: u64,
    partition_size_bytes: u64,
    role: GptPartitionRole,
    table_sha256: [u8; 32],
}

impl ReconciledGptPartitionDeviceEvidence {
    pub(in crate::linux_fs) const fn containing_device(&self) -> u64 {
        self.containing_device
    }

    pub(in crate::linux_fs) const fn inode(&self) -> u64 {
        self.inode
    }

    pub(in crate::linux_fs) const fn mount_id(&self) -> u64 {
        self.mount_id
    }

    pub(in crate::linux_fs) const fn parent_major(&self) -> u32 {
        self.parent_major
    }

    pub(in crate::linux_fs) const fn parent_minor(&self) -> u32 {
        self.parent_minor
    }

    pub(in crate::linux_fs) const fn logical_block_size(&self) -> u32 {
        self.logical_block_size
    }

    pub(in crate::linux_fs) const fn device_byte_length(&self) -> u64 {
        self.device_byte_length
    }

    pub(in crate::linux_fs) const fn partition_number(&self) -> u32 {
        self.partition_number
    }

    pub(in crate::linux_fs) fn partition_uuid(&self) -> &str {
        std::str::from_utf8(&self.partition_uuid).expect("validated partition UUID is ASCII")
    }

    pub(in crate::linux_fs) const fn partition_start_bytes(&self) -> u64 {
        self.partition_start_bytes
    }

    pub(in crate::linux_fs) const fn partition_size_bytes(&self) -> u64 {
        self.partition_size_bytes
    }

    pub(in crate::linux_fs) const fn role(&self) -> GptPartitionRole {
        self.role
    }

    pub(in crate::linux_fs) const fn table_sha256(&self) -> &[u8; 32] {
        &self.table_sha256
    }
}

/// Reconcile one retained parent observation sandwich with sysfs and GPT evidence.
///
/// The supplied GPT evidence must have been produced from the same retained
/// parent by the enclosing adapter. This function checks its partition number,
/// UUID, logical block size, image length, and exact byte geometry, but it does
/// not prove read provenance and therefore does not return live authority.
pub(in crate::linux_fs) fn reconcile_gpt_partition_device_evidence_until(
    observer: &mut impl BlockDeviceObserver,
    expected: &SysfsGptDeviceExpectation<'_>,
    validated: &ValidatedGptPartitionRole,
    deadline: Instant,
) -> io::Result<ReconciledGptPartitionDeviceEvidence> {
    let parent = expected.parent_device();
    let partition_uuid = expected.partition_uuid();
    let expected = input::ExpectedPartition {
        parent_major: parent.major(),
        parent_minor: parent.minor(),
        partition_number: expected.partition_number().get(),
        partition_uuid: partition_uuid.as_str(),
        start_512_sectors: expected.partition_start_512_sectors(),
        size_512_sectors: expected.partition_size_512_sectors(),
    };
    let validated = input::ValidatedPartition {
        role: validated.role(),
        partition_number: validated.partition_number(),
        partition_uuid: validated.partition_uuid(),
        start_lba: validated.start_lba(),
        size_lba: validated.size_lba(),
        logical_block_size: validated.logical_block_size(),
        image_bytes: validated.image_bytes(),
        table_sha256: *validated.table_sha256(),
    };
    stable::reconcile_until(observer, &expected, &validated, budget::Limits::production(), deadline)
}

#[cfg(test)]
pub(in crate::linux_fs) use budget::FixtureLimits as FixtureGptPartitionDeviceLimits;

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(in crate::linux_fs) fn reconcile_gpt_partition_device_fixture_until(
    observer: &mut impl BlockDeviceObserver,
    parent_major: u32,
    parent_minor: u32,
    partition_number: u32,
    partition_uuid: &str,
    start_512_sectors: u64,
    size_512_sectors: u64,
    role: GptPartitionRole,
    validated_partition_number: u32,
    validated_partition_uuid: &str,
    start_lba: u64,
    size_lba: u64,
    validated_logical_block_size: u32,
    validated_image_bytes: u64,
    table_sha256: [u8; 32],
    limits: FixtureGptPartitionDeviceLimits,
    deadline: Instant,
) -> io::Result<ReconciledGptPartitionDeviceEvidence> {
    reconcile_gpt_partition_device_fixture_with_clock_until(
        observer,
        parent_major,
        parent_minor,
        partition_number,
        partition_uuid,
        start_512_sectors,
        size_512_sectors,
        role,
        validated_partition_number,
        validated_partition_uuid,
        start_lba,
        size_lba,
        validated_logical_block_size,
        validated_image_bytes,
        table_sha256,
        limits,
        deadline,
        None,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(in crate::linux_fs) fn reconcile_gpt_partition_device_fixture_with_clock_until(
    observer: &mut impl BlockDeviceObserver,
    parent_major: u32,
    parent_minor: u32,
    partition_number: u32,
    partition_uuid: &str,
    start_512_sectors: u64,
    size_512_sectors: u64,
    role: GptPartitionRole,
    validated_partition_number: u32,
    validated_partition_uuid: &str,
    start_lba: u64,
    size_lba: u64,
    validated_logical_block_size: u32,
    validated_image_bytes: u64,
    table_sha256: [u8; 32],
    limits: FixtureGptPartitionDeviceLimits,
    deadline: Instant,
    clock: Option<&mut dyn FnMut() -> Instant>,
) -> io::Result<ReconciledGptPartitionDeviceEvidence> {
    let expected = input::ExpectedPartition {
        parent_major,
        parent_minor,
        partition_number,
        partition_uuid,
        start_512_sectors,
        size_512_sectors,
    };
    let validated = input::ValidatedPartition {
        role,
        partition_number: validated_partition_number,
        partition_uuid: validated_partition_uuid,
        start_lba,
        size_lba,
        logical_block_size: validated_logical_block_size,
        image_bytes: validated_image_bytes,
        table_sha256,
    };
    stable::reconcile_with_clock_until(observer, &expected, &validated, limits.into(), deadline, clock)
}
