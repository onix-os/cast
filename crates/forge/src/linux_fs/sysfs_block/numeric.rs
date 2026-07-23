use std::{io, num::NonZeroU32, num::NonZeroU64, time::Instant};

use super::{
    SYSFS_DEV_ATTRIBUTE_MAX_BYTES, SYSFS_PARTITION_ATTRIBUTE_MAX_BYTES, SysfsDeviceNumber, SysfsDiskSequence,
    SysfsPartitionNumber, invalid_data, require_deadline, unexpected_eof,
};

const MAX_U32_DECIMAL_BYTES: usize = 10;
const MAX_U64_DECIMAL_BYTES: usize = 20;

/// Parse the complete contents of a sysfs block-device `dev` attribute.
pub(crate) fn parse_sysfs_dev(bytes: &[u8]) -> io::Result<SysfsDeviceNumber> {
    parse_sysfs_dev_with_deadline(bytes, None)
}

/// Parse one complete `dev` attribute within the caller's operation deadline.
pub(crate) fn parse_sysfs_dev_until(bytes: &[u8], deadline: Instant) -> io::Result<SysfsDeviceNumber> {
    parse_sysfs_dev_with_deadline(bytes, Some(deadline))
}

fn parse_sysfs_dev_with_deadline(bytes: &[u8], deadline: Option<Instant>) -> io::Result<SysfsDeviceNumber> {
    require_deadline(deadline)?;
    if bytes.len() > SYSFS_DEV_ATTRIBUTE_MAX_BYTES {
        return Err(invalid_data("sysfs dev attribute exceeds its canonical byte bound"));
    }
    let body = terminated_body(bytes, "sysfs dev attribute")?;
    let mut separator = None;
    for (index, byte) in body.iter().enumerate() {
        require_deadline(deadline)?;
        if *byte == b':' && separator.replace(index).is_some() {
            return Err(invalid_data("sysfs dev attribute has more than one separator"));
        }
    }
    let separator = separator.ok_or_else(|| invalid_data("sysfs dev attribute lacks its major:minor separator"))?;
    let major = canonical_u32_with_deadline(&body[..separator], false, "sysfs major number", deadline)?;
    let minor = canonical_u32_with_deadline(&body[separator + 1..], false, "sysfs minor number", deadline)?;
    require_deadline(deadline)?;
    Ok(SysfsDeviceNumber::from_major_minor(major, minor))
}

/// Parse the complete contents of a sysfs partition `partition` attribute.
pub(crate) fn parse_sysfs_partition_number(bytes: &[u8]) -> io::Result<SysfsPartitionNumber> {
    parse_sysfs_partition_number_with_deadline(bytes, None)
}

/// Parse one complete `partition` attribute within the caller's deadline.
pub(crate) fn parse_sysfs_partition_number_until(bytes: &[u8], deadline: Instant) -> io::Result<SysfsPartitionNumber> {
    parse_sysfs_partition_number_with_deadline(bytes, Some(deadline))
}

fn parse_sysfs_partition_number_with_deadline(
    bytes: &[u8],
    deadline: Option<Instant>,
) -> io::Result<SysfsPartitionNumber> {
    require_deadline(deadline)?;
    if bytes.len() > SYSFS_PARTITION_ATTRIBUTE_MAX_BYTES {
        return Err(invalid_data(
            "sysfs partition attribute exceeds its canonical byte bound",
        ));
    }
    let body = terminated_body(bytes, "sysfs partition attribute")?;
    let number = canonical_positive_u32_with_deadline(body, "sysfs partition number", deadline)?;
    require_deadline(deadline)?;
    Ok(number)
}

pub(super) fn canonical_u32(bytes: &[u8], positive: bool, field: &'static str) -> io::Result<u32> {
    canonical_u32_with_deadline(bytes, positive, field, None)
}

pub(super) fn canonical_u32_until(
    bytes: &[u8],
    positive: bool,
    field: &'static str,
    deadline: Instant,
) -> io::Result<u32> {
    canonical_u32_with_deadline(bytes, positive, field, Some(deadline))
}

fn canonical_u32_with_deadline(
    bytes: &[u8],
    positive: bool,
    field: &'static str,
    deadline: Option<Instant>,
) -> io::Result<u32> {
    require_deadline(deadline)?;
    if bytes.is_empty() || bytes.len() > MAX_U32_DECIMAL_BYTES {
        return Err(invalid_data(format!("{field} is not a bounded canonical u32")));
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return Err(invalid_data(format!("{field} has a leading zero")));
    }

    let mut value = 0_u32;
    for byte in bytes {
        require_deadline(deadline)?;
        let digit = byte
            .checked_sub(b'0')
            .filter(|digit| *digit <= 9)
            .ok_or_else(|| invalid_data(format!("{field} contains a non-decimal byte")))?;
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(digit)))
            .ok_or_else(|| invalid_data(format!("{field} exceeds u32")))?;
    }
    if positive && value == 0 {
        return Err(invalid_data(format!("{field} must be positive")));
    }
    require_deadline(deadline)?;
    Ok(value)
}

pub(super) fn canonical_positive_u32(bytes: &[u8], field: &'static str) -> io::Result<SysfsPartitionNumber> {
    canonical_positive_u32_with_deadline(bytes, field, None)
}

pub(super) fn canonical_positive_u32_until(
    bytes: &[u8],
    field: &'static str,
    deadline: Instant,
) -> io::Result<SysfsPartitionNumber> {
    canonical_positive_u32_with_deadline(bytes, field, Some(deadline))
}

fn canonical_positive_u32_with_deadline(
    bytes: &[u8],
    field: &'static str,
    deadline: Option<Instant>,
) -> io::Result<SysfsPartitionNumber> {
    let value = canonical_u32_with_deadline(bytes, true, field, deadline)?;
    Ok(SysfsPartitionNumber(
        NonZeroU32::new(value).expect("positive u32 is nonzero"),
    ))
}

pub(super) fn canonical_positive_u64(bytes: &[u8], field: &'static str) -> io::Result<SysfsDiskSequence> {
    canonical_positive_u64_with_deadline(bytes, field, None)
}

pub(super) fn canonical_positive_u64_until(
    bytes: &[u8],
    field: &'static str,
    deadline: Instant,
) -> io::Result<SysfsDiskSequence> {
    canonical_positive_u64_with_deadline(bytes, field, Some(deadline))
}

fn canonical_positive_u64_with_deadline(
    bytes: &[u8],
    field: &'static str,
    deadline: Option<Instant>,
) -> io::Result<SysfsDiskSequence> {
    require_deadline(deadline)?;
    if bytes.is_empty() || bytes.len() > MAX_U64_DECIMAL_BYTES {
        return Err(invalid_data(format!("{field} is not a bounded canonical u64")));
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return Err(invalid_data(format!("{field} has a leading zero")));
    }

    let mut value = 0_u64;
    for byte in bytes {
        require_deadline(deadline)?;
        let digit = byte
            .checked_sub(b'0')
            .filter(|digit| *digit <= 9)
            .ok_or_else(|| invalid_data(format!("{field} contains a non-decimal byte")))?;
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(digit)))
            .ok_or_else(|| invalid_data(format!("{field} exceeds u64")))?;
    }
    require_deadline(deadline)?;
    Ok(SysfsDiskSequence(
        NonZeroU64::new(value).ok_or_else(|| invalid_data(format!("{field} must be positive")))?,
    ))
}

fn terminated_body<'a>(bytes: &'a [u8], field: &'static str) -> io::Result<&'a [u8]> {
    let Some(body) = bytes.strip_suffix(b"\n") else {
        return Err(unexpected_eof(format!("{field} lacks its terminating newline")));
    };
    if body.is_empty() {
        return Err(invalid_data(format!("{field} is empty")));
    }
    Ok(body)
}
