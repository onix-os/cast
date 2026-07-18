use std::{io, time::Instant};

use super::{
    SysfsDeviceNumber, SysfsDiskSequence, SysfsPartitionNumber, SysfsPartitionUuid, SysfsUevent, invalid_data,
    numeric::{
        canonical_positive_u32, canonical_positive_u32_until, canonical_positive_u64, canonical_positive_u64_until,
        canonical_u32, canonical_u32_until,
    },
    parse_sysfs_dev, parse_sysfs_dev_until, parse_sysfs_partition_number, parse_sysfs_partition_number_until,
    parse_sysfs_uevent, parse_sysfs_uevent_until, require_deadline,
    uuid::{canonical_partition_uuid, canonical_partition_uuid_until},
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SysfsPartitionIdentity {
    device: SysfsDeviceNumber,
    partition_number: SysfsPartitionNumber,
    partition_uuid: SysfsPartitionUuid,
    disk_sequence: Option<SysfsDiskSequence>,
    uevent: SysfsUevent,
}

impl SysfsPartitionIdentity {
    pub(crate) const fn device(&self) -> SysfsDeviceNumber {
        self.device
    }

    pub(crate) const fn partition_number(&self) -> SysfsPartitionNumber {
        self.partition_number
    }

    pub(crate) const fn partition_uuid(&self) -> SysfsPartitionUuid {
        self.partition_uuid
    }

    pub(crate) const fn disk_sequence(&self) -> Option<SysfsDiskSequence> {
        self.disk_sequence
    }

    pub(crate) const fn uevent(&self) -> &SysfsUevent {
        &self.uevent
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SysfsDiskIdentity {
    device: SysfsDeviceNumber,
    disk_sequence: Option<SysfsDiskSequence>,
    uevent: SysfsUevent,
}

impl SysfsDiskIdentity {
    pub(crate) const fn device(&self) -> SysfsDeviceNumber {
        self.device
    }

    pub(crate) const fn disk_sequence(&self) -> Option<SysfsDiskSequence> {
        self.disk_sequence
    }

    pub(crate) const fn uevent(&self) -> &SysfsUevent {
        &self.uevent
    }
}

/// Parse and cross-check one partition's `dev`, `partition`, and `uevent`
/// attributes from a single descriptor-retained capture.
pub(crate) fn parse_sysfs_partition_identity(
    dev_bytes: &[u8],
    partition_bytes: &[u8],
    uevent_bytes: &[u8],
) -> io::Result<SysfsPartitionIdentity> {
    parse_sysfs_partition_identity_with_deadline(dev_bytes, partition_bytes, uevent_bytes, None)
}

/// Parse and cross-check one complete partition capture under one deadline.
pub(crate) fn parse_sysfs_partition_identity_until(
    dev_bytes: &[u8],
    partition_bytes: &[u8],
    uevent_bytes: &[u8],
    deadline: Instant,
) -> io::Result<SysfsPartitionIdentity> {
    parse_sysfs_partition_identity_with_deadline(dev_bytes, partition_bytes, uevent_bytes, Some(deadline))
}

fn parse_sysfs_partition_identity_with_deadline(
    dev_bytes: &[u8],
    partition_bytes: &[u8],
    uevent_bytes: &[u8],
    deadline: Option<Instant>,
) -> io::Result<SysfsPartitionIdentity> {
    require_deadline(deadline)?;
    let device = parse_dev(dev_bytes, deadline)?;
    let partition_number = parse_partition_number(partition_bytes, deadline)?;
    let uevent = parse_uevent(uevent_bytes, deadline)?;

    require_exact(&uevent, b"DEVTYPE", b"partition", "partition DEVTYPE", deadline)?;
    let uevent_device = event_device_number(&uevent, deadline)?;
    if uevent_device != device {
        return Err(invalid_data(
            "sysfs partition dev attribute disagrees with uevent MAJOR/MINOR",
        ));
    }

    let event_partition_number = positive_u32(required(&uevent, b"PARTN", deadline)?, "uevent PARTN", deadline)?;
    if event_partition_number != partition_number {
        return Err(invalid_data("sysfs partition attribute disagrees with uevent PARTN"));
    }

    let partition_uuid = partition_uuid(required(&uevent, b"PARTUUID", deadline)?, deadline)?;
    let disk_sequence = optional_disk_sequence(&uevent, deadline)?;
    require_deadline(deadline)?;
    Ok(SysfsPartitionIdentity {
        device,
        partition_number,
        partition_uuid,
        disk_sequence,
        uevent,
    })
}

/// Parse and cross-check one whole disk's `dev` and `uevent` attributes from a
/// single descriptor-retained capture.
pub(crate) fn parse_sysfs_disk_identity(dev_bytes: &[u8], uevent_bytes: &[u8]) -> io::Result<SysfsDiskIdentity> {
    parse_sysfs_disk_identity_with_deadline(dev_bytes, uevent_bytes, None)
}

/// Parse and cross-check one complete whole-disk capture under one deadline.
pub(crate) fn parse_sysfs_disk_identity_until(
    dev_bytes: &[u8],
    uevent_bytes: &[u8],
    deadline: Instant,
) -> io::Result<SysfsDiskIdentity> {
    parse_sysfs_disk_identity_with_deadline(dev_bytes, uevent_bytes, Some(deadline))
}

fn parse_sysfs_disk_identity_with_deadline(
    dev_bytes: &[u8],
    uevent_bytes: &[u8],
    deadline: Option<Instant>,
) -> io::Result<SysfsDiskIdentity> {
    require_deadline(deadline)?;
    let device = parse_dev(dev_bytes, deadline)?;
    let uevent = parse_uevent(uevent_bytes, deadline)?;

    require_exact(&uevent, b"DEVTYPE", b"disk", "disk DEVTYPE", deadline)?;
    let uevent_device = event_device_number(&uevent, deadline)?;
    if uevent_device != device {
        return Err(invalid_data(
            "sysfs disk dev attribute disagrees with uevent MAJOR/MINOR",
        ));
    }
    if optional(&uevent, b"PARTN", deadline)?.is_some() || optional(&uevent, b"PARTUUID", deadline)?.is_some() {
        return Err(invalid_data(
            "sysfs whole-disk uevent contains partition-only identity fields",
        ));
    }

    let disk_sequence = optional_disk_sequence(&uevent, deadline)?;
    require_deadline(deadline)?;
    Ok(SysfsDiskIdentity {
        device,
        disk_sequence,
        uevent,
    })
}

/// Require the optional kernel disk sequence to be absent on both identities
/// or present and equal on both.
///
/// Equality is useful race evidence, but is not a substitute for retaining the
/// partition's parent-disk descriptor and proving that relationship directly.
pub(crate) fn require_matching_disk_sequence(
    partition: &SysfsPartitionIdentity,
    disk: &SysfsDiskIdentity,
) -> io::Result<Option<SysfsDiskSequence>> {
    require_matching_disk_sequence_with_deadline(partition, disk, None)
}

/// Compare optional disk-sequence evidence under the enclosing deadline.
pub(crate) fn require_matching_disk_sequence_until(
    partition: &SysfsPartitionIdentity,
    disk: &SysfsDiskIdentity,
    deadline: Instant,
) -> io::Result<Option<SysfsDiskSequence>> {
    require_matching_disk_sequence_with_deadline(partition, disk, Some(deadline))
}

fn require_matching_disk_sequence_with_deadline(
    partition: &SysfsPartitionIdentity,
    disk: &SysfsDiskIdentity,
    deadline: Option<Instant>,
) -> io::Result<Option<SysfsDiskSequence>> {
    require_deadline(deadline)?;
    match (partition.disk_sequence, disk.disk_sequence) {
        (None, None) => {
            require_deadline(deadline)?;
            Ok(None)
        }
        (Some(partition), Some(disk)) if partition == disk => {
            require_deadline(deadline)?;
            Ok(Some(partition))
        }
        _ => Err(invalid_data(
            "partition and parent-disk uevents have inconsistent DISKSEQ evidence",
        )),
    }
}

fn event_device_number(event: &SysfsUevent, deadline: Option<Instant>) -> io::Result<SysfsDeviceNumber> {
    let major = u32_value(required(event, b"MAJOR", deadline)?, false, "uevent MAJOR", deadline)?;
    let minor = u32_value(required(event, b"MINOR", deadline)?, false, "uevent MINOR", deadline)?;
    require_deadline(deadline)?;
    Ok(SysfsDeviceNumber::from_major_minor(major, minor))
}

fn optional_disk_sequence(event: &SysfsUevent, deadline: Option<Instant>) -> io::Result<Option<SysfsDiskSequence>> {
    let sequence = optional(event, b"DISKSEQ", deadline)?
        .map(|bytes| positive_u64(bytes, "uevent DISKSEQ", deadline))
        .transpose()?;
    require_deadline(deadline)?;
    Ok(sequence)
}

fn required<'a>(event: &'a SysfsUevent, key: &[u8], deadline: Option<Instant>) -> io::Result<&'a [u8]> {
    optional(event, key, deadline)?.ok_or_else(|| invalid_data("sysfs uevent lacks a required identity field"))
}

fn optional<'a>(event: &'a SysfsUevent, key: &[u8], deadline: Option<Instant>) -> io::Result<Option<&'a [u8]>> {
    require_deadline(deadline)?;
    for field in event.fields() {
        require_deadline(deadline)?;
        if field.key() == key {
            require_deadline(deadline)?;
            return Ok(Some(field.value()));
        }
    }
    require_deadline(deadline)?;
    Ok(None)
}

fn require_exact(
    event: &SysfsUevent,
    key: &[u8],
    expected: &[u8],
    field: &'static str,
    deadline: Option<Instant>,
) -> io::Result<()> {
    if required(event, key, deadline)? != expected {
        return Err(invalid_data(format!("sysfs {field} has an unexpected value")));
    }
    require_deadline(deadline)?;
    Ok(())
}

fn parse_dev(bytes: &[u8], deadline: Option<Instant>) -> io::Result<SysfsDeviceNumber> {
    match deadline {
        Some(deadline) => parse_sysfs_dev_until(bytes, deadline),
        None => parse_sysfs_dev(bytes),
    }
}

fn parse_partition_number(bytes: &[u8], deadline: Option<Instant>) -> io::Result<SysfsPartitionNumber> {
    match deadline {
        Some(deadline) => parse_sysfs_partition_number_until(bytes, deadline),
        None => parse_sysfs_partition_number(bytes),
    }
}

fn parse_uevent(bytes: &[u8], deadline: Option<Instant>) -> io::Result<SysfsUevent> {
    match deadline {
        Some(deadline) => parse_sysfs_uevent_until(bytes, deadline),
        None => parse_sysfs_uevent(bytes),
    }
}

fn u32_value(bytes: &[u8], positive: bool, field: &'static str, deadline: Option<Instant>) -> io::Result<u32> {
    match deadline {
        Some(deadline) => canonical_u32_until(bytes, positive, field, deadline),
        None => canonical_u32(bytes, positive, field),
    }
}

fn positive_u32(bytes: &[u8], field: &'static str, deadline: Option<Instant>) -> io::Result<SysfsPartitionNumber> {
    match deadline {
        Some(deadline) => canonical_positive_u32_until(bytes, field, deadline),
        None => canonical_positive_u32(bytes, field),
    }
}

fn positive_u64(bytes: &[u8], field: &'static str, deadline: Option<Instant>) -> io::Result<SysfsDiskSequence> {
    match deadline {
        Some(deadline) => canonical_positive_u64_until(bytes, field, deadline),
        None => canonical_positive_u64(bytes, field),
    }
}

fn partition_uuid(bytes: &[u8], deadline: Option<Instant>) -> io::Result<SysfsPartitionUuid> {
    match deadline {
        Some(deadline) => canonical_partition_uuid_until(bytes, deadline),
        None => canonical_partition_uuid(bytes),
    }
}
