use std::io;

use super::{
    SysfsDeviceNumber, SysfsDiskSequence, SysfsPartitionNumber, SysfsPartitionUuid, SysfsUevent, invalid_data,
    numeric::{canonical_positive_u32, canonical_positive_u64, canonical_u32},
    parse_sysfs_dev, parse_sysfs_partition_number, parse_sysfs_uevent,
    uuid::canonical_partition_uuid,
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
    let device = parse_sysfs_dev(dev_bytes)?;
    let partition_number = parse_sysfs_partition_number(partition_bytes)?;
    let uevent = parse_sysfs_uevent(uevent_bytes)?;

    require_exact(&uevent, b"DEVTYPE", b"partition", "partition DEVTYPE")?;
    let uevent_device = event_device_number(&uevent)?;
    if uevent_device != device {
        return Err(invalid_data(
            "sysfs partition dev attribute disagrees with uevent MAJOR/MINOR",
        ));
    }

    let event_partition_number = canonical_positive_u32(required(&uevent, b"PARTN")?, "uevent PARTN")?;
    if event_partition_number != partition_number {
        return Err(invalid_data("sysfs partition attribute disagrees with uevent PARTN"));
    }

    let partition_uuid = canonical_partition_uuid(required(&uevent, b"PARTUUID")?)?;
    let disk_sequence = optional_disk_sequence(&uevent)?;
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
    let device = parse_sysfs_dev(dev_bytes)?;
    let uevent = parse_sysfs_uevent(uevent_bytes)?;

    require_exact(&uevent, b"DEVTYPE", b"disk", "disk DEVTYPE")?;
    let uevent_device = event_device_number(&uevent)?;
    if uevent_device != device {
        return Err(invalid_data(
            "sysfs disk dev attribute disagrees with uevent MAJOR/MINOR",
        ));
    }
    if uevent.value(b"PARTN").is_some() || uevent.value(b"PARTUUID").is_some() {
        return Err(invalid_data(
            "sysfs whole-disk uevent contains partition-only identity fields",
        ));
    }

    let disk_sequence = optional_disk_sequence(&uevent)?;
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
    match (partition.disk_sequence, disk.disk_sequence) {
        (None, None) => Ok(None),
        (Some(partition), Some(disk)) if partition == disk => Ok(Some(partition)),
        _ => Err(invalid_data(
            "partition and parent-disk uevents have inconsistent DISKSEQ evidence",
        )),
    }
}

fn event_device_number(event: &SysfsUevent) -> io::Result<SysfsDeviceNumber> {
    let major = canonical_u32(required(event, b"MAJOR")?, false, "uevent MAJOR")?;
    let minor = canonical_u32(required(event, b"MINOR")?, false, "uevent MINOR")?;
    Ok(SysfsDeviceNumber::from_major_minor(major, minor))
}

fn optional_disk_sequence(event: &SysfsUevent) -> io::Result<Option<SysfsDiskSequence>> {
    event
        .value(b"DISKSEQ")
        .map(|bytes| canonical_positive_u64(bytes, "uevent DISKSEQ"))
        .transpose()
}

fn required<'a>(event: &'a SysfsUevent, key: &[u8]) -> io::Result<&'a [u8]> {
    event
        .value(key)
        .ok_or_else(|| invalid_data("sysfs uevent lacks a required identity field"))
}

fn require_exact(event: &SysfsUevent, key: &[u8], expected: &[u8], field: &'static str) -> io::Result<()> {
    if required(event, key)? != expected {
        return Err(invalid_data(format!("sysfs {field} has an unexpected value")));
    }
    Ok(())
}
