use std::io;

use super::{budget::Operation, input::ExpectedPartition, input::ValidatedPartition};

const SYSFS_SECTOR_BYTES: u64 = 512;
const MIN_LOGICAL_BLOCK_SIZE: u32 = 512;
const MAX_LOGICAL_BLOCK_SIZE: u32 = 65_536;
pub(super) const GEOMETRY_WORK_UNITS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReconciledGeometry {
    pub(super) start_bytes: u64,
    pub(super) size_bytes: u64,
}

pub(super) fn require_exact_geometry(
    expected: &ExpectedPartition<'_>,
    validated: &ValidatedPartition<'_>,
    operation: &mut Operation<'_>,
) -> io::Result<ReconciledGeometry> {
    operation.charge_work(GEOMETRY_WORK_UNITS)?;
    let logical_block_size = validated.logical_block_size;
    let device_byte_length = validated.image_bytes;
    require_sane_parent_geometry(logical_block_size, device_byte_length)?;

    let logical_block_size = u64::from(logical_block_size);
    let sysfs_start = expected
        .start_512_sectors
        .checked_mul(SYSFS_SECTOR_BYTES)
        .ok_or_else(|| invalid("sysfs partition start overflows bytes"))?;
    let sysfs_size = expected
        .size_512_sectors
        .checked_mul(SYSFS_SECTOR_BYTES)
        .ok_or_else(|| invalid("sysfs partition size overflows bytes"))?;
    let gpt_start = validated
        .start_lba
        .checked_mul(logical_block_size)
        .ok_or_else(|| invalid("GPT partition start overflows bytes"))?;
    let gpt_size = validated
        .size_lba
        .checked_mul(logical_block_size)
        .ok_or_else(|| invalid("GPT partition size overflows bytes"))?;

    if sysfs_size == 0 || gpt_size == 0 {
        return Err(invalid("partition size must be positive"));
    }
    if (sysfs_start, sysfs_size) != (gpt_start, gpt_size) {
        return Err(invalid("sysfs 512-sector geometry disagrees with GPT geometry"));
    }
    let partition_end = gpt_start
        .checked_add(gpt_size)
        .ok_or_else(|| invalid("GPT partition byte range overflows"))?;
    if partition_end > device_byte_length {
        return Err(invalid("GPT partition byte range exceeds the parent device"));
    }
    operation.checkpoint()?;
    Ok(ReconciledGeometry {
        start_bytes: gpt_start,
        size_bytes: gpt_size,
    })
}

pub(super) fn require_sane_parent_observation(
    logical_block_size: u32,
    device_byte_length: u64,
    operation: &mut Operation<'_>,
) -> io::Result<()> {
    operation.charge_work(GEOMETRY_WORK_UNITS)?;
    require_sane_parent_geometry(logical_block_size, device_byte_length)?;
    operation.checkpoint()
}

fn require_sane_parent_geometry(logical_block_size: u32, device_byte_length: u64) -> io::Result<()> {
    if !(MIN_LOGICAL_BLOCK_SIZE..=MAX_LOGICAL_BLOCK_SIZE).contains(&logical_block_size)
        || !logical_block_size.is_power_of_two()
    {
        return Err(invalid("block device reports an unsupported logical block size"));
    }
    let logical_block_size = u64::from(logical_block_size);
    if device_byte_length == 0 || device_byte_length > i64::MAX as u64 || device_byte_length % logical_block_size != 0 {
        return Err(invalid(
            "block device byte length is zero, unaddressable, or not aligned to its logical block size",
        ));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
