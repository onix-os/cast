//! Canonical parsing of Linux partition `start` and `size` attributes.
//!
//! Linux exposes both values in fixed 512-byte sectors. These scalar facts do
//! not identify a partition table, logical block size, GPT role, or block
//! device and grant no read or mutation authority.

use std::{io, time::Instant};

use super::{SYSFS_PARTITION_GEOMETRY_ATTRIBUTE_MAX_BYTES, invalid_data, require_deadline, unexpected_eof};

const MAX_U64_DECIMAL_BYTES: usize = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SysfsPartitionGeometry {
    start_512_sectors: u64,
    size_512_sectors: u64,
}

impl SysfsPartitionGeometry {
    pub(crate) const fn start_512_sectors(self) -> u64 {
        self.start_512_sectors
    }

    pub(crate) const fn size_512_sectors(self) -> u64 {
        self.size_512_sectors
    }
}

pub(crate) fn parse_sysfs_partition_geometry(
    start_bytes: &[u8],
    size_bytes: &[u8],
) -> io::Result<SysfsPartitionGeometry> {
    parse_with_deadline(start_bytes, size_bytes, None)
}

pub(crate) fn parse_sysfs_partition_geometry_until(
    start_bytes: &[u8],
    size_bytes: &[u8],
    deadline: Instant,
) -> io::Result<SysfsPartitionGeometry> {
    parse_with_deadline(start_bytes, size_bytes, Some(deadline))
}

fn parse_with_deadline(
    start_bytes: &[u8],
    size_bytes: &[u8],
    deadline: Option<Instant>,
) -> io::Result<SysfsPartitionGeometry> {
    let start_512_sectors = parse_attribute(start_bytes, "sysfs partition start", false, deadline)?;
    let size_512_sectors = parse_attribute(size_bytes, "sysfs partition size", true, deadline)?;
    require_deadline(deadline)?;
    Ok(SysfsPartitionGeometry {
        start_512_sectors,
        size_512_sectors,
    })
}

fn parse_attribute(bytes: &[u8], field: &'static str, positive: bool, deadline: Option<Instant>) -> io::Result<u64> {
    require_deadline(deadline)?;
    if bytes.len() > SYSFS_PARTITION_GEOMETRY_ATTRIBUTE_MAX_BYTES {
        return Err(invalid_data(format!("{field} exceeds its canonical byte bound")));
    }
    let body = bytes
        .strip_suffix(b"\n")
        .ok_or_else(|| unexpected_eof(format!("{field} lacks its terminating newline")))?;
    if body.is_empty() || body.len() > MAX_U64_DECIMAL_BYTES {
        return Err(invalid_data(format!("{field} is not a bounded canonical u64")));
    }
    if body.len() > 1 && body[0] == b'0' {
        return Err(invalid_data(format!("{field} has a leading zero")));
    }

    let mut value = 0_u64;
    for byte in body {
        require_deadline(deadline)?;
        let digit = byte
            .checked_sub(b'0')
            .filter(|digit| *digit <= 9)
            .ok_or_else(|| invalid_data(format!("{field} contains a non-decimal byte")))?;
        value = value
            .checked_mul(10)
            .and_then(|current| current.checked_add(u64::from(digit)))
            .ok_or_else(|| invalid_data(format!("{field} exceeds u64")))?;
    }
    if positive && value == 0 {
        return Err(invalid_data(format!("{field} must be positive")));
    }
    require_deadline(deadline)?;
    Ok(value)
}
