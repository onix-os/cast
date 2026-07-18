use std::{io, num::NonZeroU32, num::NonZeroU64};

use super::{SysfsDeviceNumber, SysfsDiskSequence, SysfsPartitionNumber, invalid_data, unexpected_eof};

const MAX_U32_DECIMAL_BYTES: usize = 10;
const MAX_U64_DECIMAL_BYTES: usize = 20;
const MAX_DEV_FILE_BYTES: usize = MAX_U32_DECIMAL_BYTES * 2 + 2;
const MAX_PARTITION_FILE_BYTES: usize = MAX_U32_DECIMAL_BYTES + 1;

/// Parse the complete contents of a sysfs block-device `dev` attribute.
pub(crate) fn parse_sysfs_dev(bytes: &[u8]) -> io::Result<SysfsDeviceNumber> {
    if bytes.len() > MAX_DEV_FILE_BYTES {
        return Err(invalid_data("sysfs dev attribute exceeds its canonical byte bound"));
    }
    let body = terminated_body(bytes, "sysfs dev attribute")?;
    let Some(separator) = body.iter().position(|byte| *byte == b':') else {
        return Err(invalid_data("sysfs dev attribute lacks its major:minor separator"));
    };
    if body[separator + 1..].contains(&b':') {
        return Err(invalid_data("sysfs dev attribute has more than one separator"));
    }
    let major = canonical_u32(&body[..separator], false, "sysfs major number")?;
    let minor = canonical_u32(&body[separator + 1..], false, "sysfs minor number")?;
    Ok(SysfsDeviceNumber::from_major_minor(major, minor))
}

/// Parse the complete contents of a sysfs partition `partition` attribute.
pub(crate) fn parse_sysfs_partition_number(bytes: &[u8]) -> io::Result<SysfsPartitionNumber> {
    if bytes.len() > MAX_PARTITION_FILE_BYTES {
        return Err(invalid_data(
            "sysfs partition attribute exceeds its canonical byte bound",
        ));
    }
    let body = terminated_body(bytes, "sysfs partition attribute")?;
    canonical_positive_u32(body, "sysfs partition number")
}

pub(super) fn canonical_u32(bytes: &[u8], positive: bool, field: &'static str) -> io::Result<u32> {
    if bytes.is_empty() || bytes.len() > MAX_U32_DECIMAL_BYTES {
        return Err(invalid_data(format!("{field} is not a bounded canonical u32")));
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return Err(invalid_data(format!("{field} has a leading zero")));
    }

    let mut value = 0_u32;
    for byte in bytes {
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
    Ok(value)
}

pub(super) fn canonical_positive_u32(bytes: &[u8], field: &'static str) -> io::Result<SysfsPartitionNumber> {
    let value = canonical_u32(bytes, true, field)?;
    Ok(SysfsPartitionNumber(
        NonZeroU32::new(value).expect("positive u32 is nonzero"),
    ))
}

pub(super) fn canonical_positive_u64(bytes: &[u8], field: &'static str) -> io::Result<SysfsDiskSequence> {
    if bytes.is_empty() || bytes.len() > MAX_U64_DECIMAL_BYTES {
        return Err(invalid_data(format!("{field} is not a bounded canonical u64")));
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return Err(invalid_data(format!("{field} has a leading zero")));
    }

    let mut value = 0_u64;
    for byte in bytes {
        let digit = byte
            .checked_sub(b'0')
            .filter(|digit| *digit <= 9)
            .ok_or_else(|| invalid_data(format!("{field} contains a non-decimal byte")))?;
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(digit)))
            .ok_or_else(|| invalid_data(format!("{field} exceeds u64")))?;
    }
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
