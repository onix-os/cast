//! Strict, bounded authentication of one GPT partition role from an image.
//!
//! This foundation accepts only the GPT revision-1 layout used by the boot
//! publication policy.  It requires matching primary and backup metadata and
//! returns semantic facts for exactly one selected entry.  It performs no
//! discovery and returns no image contents or reusable read capability.

use std::{io, time::Instant};

mod constants;
mod guid;
mod parser;
mod reader;

use guid::Guid;

/// Declarative GPT role admitted by the boot publication policy.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum GptPartitionRole {
    Esp,
    Xbootldr,
}

impl GptPartitionRole {
    const fn type_guid(self) -> Guid {
        match self {
            Self::Esp => Guid::from_disk_bytes(constants::ESP_TYPE_GUID),
            Self::Xbootldr => Guid::from_disk_bytes(constants::XBOOTLDR_TYPE_GUID),
        }
    }
}

/// Closed semantic evidence for one exact selected GPT entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedGptPartitionRole {
    role: GptPartitionRole,
    partition_uuid: [u8; 36],
    start_lba: u64,
    size_lba: u64,
}

impl ValidatedGptPartitionRole {
    pub(crate) const fn role(&self) -> GptPartitionRole {
        self.role
    }

    pub(crate) fn partition_uuid(&self) -> &str {
        std::str::from_utf8(&self.partition_uuid).expect("validated GPT UUID is ASCII")
    }

    pub(crate) const fn start_lba(&self) -> u64 {
        self.start_lba
    }

    pub(crate) const fn size_lba(&self) -> u64 {
        self.size_lba
    }
}

/// Authenticate one exact GPT entry from a complete, caller-owned image.
///
/// Every read, allocation, validation step, and terminal check shares the
/// supplied absolute deadline.  The logical block size must be a power of two
/// from 512 through 65,536 bytes, inclusive.
pub(crate) fn authenticate_gpt_partition_role_image_until(
    image: &[u8],
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: &str,
    expected_role: GptPartitionRole,
    deadline: Instant,
) -> io::Result<ValidatedGptPartitionRole> {
    let expected_uuid = Guid::parse_canonical(expected_partition_uuid, deadline)?;
    let mut source = reader::SliceImage::new(image);
    let selected = parser::authenticate_until(
        &mut source,
        logical_block_size,
        expected_partition_number,
        expected_uuid,
        expected_role,
        reader::Limits::production(),
        deadline,
    )?;
    Ok(ValidatedGptPartitionRole {
        role: expected_role,
        partition_uuid: selected.partition_uuid.canonical_bytes(),
        start_lba: selected.start_lba,
        size_lba: selected.size_lba,
    })
}

#[cfg(test)]
pub(crate) use reader::FixtureLimits as FixtureGptPartitionRoleLimits;

#[cfg(test)]
pub(crate) fn authenticate_gpt_partition_role_fixture_until(
    image: &[u8],
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: &str,
    expected_role: GptPartitionRole,
    limits: FixtureGptPartitionRoleLimits,
    deadline: Instant,
) -> io::Result<ValidatedGptPartitionRole> {
    let expected_uuid = Guid::parse_canonical(expected_partition_uuid, deadline)?;
    let mut source = reader::SliceImage::new(image);
    let selected = parser::authenticate_until(
        &mut source,
        logical_block_size,
        expected_partition_number,
        expected_uuid,
        expected_role,
        limits.into(),
        deadline,
    )?;
    Ok(ValidatedGptPartitionRole {
        role: expected_role,
        partition_uuid: selected.partition_uuid.canonical_bytes(),
        start_lba: selected.start_lba,
        size_lba: selected.size_lba,
    })
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn authenticate_gpt_partition_role_chunked_fixture_until(
    image: &[u8],
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: &str,
    expected_role: GptPartitionRole,
    max_chunk: usize,
    stop_after: Option<usize>,
    deadline: Instant,
) -> io::Result<ValidatedGptPartitionRole> {
    let expected_uuid = Guid::parse_canonical(expected_partition_uuid, deadline)?;
    let mut source = reader::ChunkedSliceImage::new(image, max_chunk, stop_after);
    let selected = parser::authenticate_until(
        &mut source,
        logical_block_size,
        expected_partition_number,
        expected_uuid,
        expected_role,
        reader::Limits::production(),
        deadline,
    )?;
    Ok(ValidatedGptPartitionRole {
        role: expected_role,
        partition_uuid: selected.partition_uuid.canonical_bytes(),
        start_lba: selected.start_lba,
        size_lba: selected.size_lba,
    })
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn authenticate_gpt_partition_role_fixture_with_clock_until(
    image: &[u8],
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: &str,
    expected_role: GptPartitionRole,
    limits: FixtureGptPartitionRoleLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> io::Result<ValidatedGptPartitionRole> {
    let expected_uuid = Guid::parse_canonical(expected_partition_uuid, deadline)?;
    let mut source = reader::SliceImage::new(image);
    let selected = parser::authenticate_with_clock_until(
        &mut source,
        logical_block_size,
        expected_partition_number,
        expected_uuid,
        expected_role,
        limits.into(),
        deadline,
        Some(clock),
    )?;
    Ok(ValidatedGptPartitionRole {
        role: expected_role,
        partition_uuid: selected.partition_uuid.canonical_bytes(),
        start_lba: selected.start_lba,
        size_lba: selected.size_lba,
    })
}
