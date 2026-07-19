//! Strict, bounded authentication of one GPT partition role from an image.
//!
//! This foundation accepts only the GPT revision-1 layout used by the boot
//! publication policy.  It requires matching primary and backup metadata and
//! returns semantic facts for exactly one selected entry plus a
//! role-independent SHA-256 identity of the complete accepted table.  It
//! performs no discovery and returns no image contents or reusable read
//! capability.

use std::{io, time::Instant};

mod constants;
mod fingerprint;
mod guid;
mod parser;
mod reader;
mod snapshot;
mod stable;

use guid::Guid;
pub(in crate::linux_fs) use reader::Image as GptPartitionRoleImage;

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

/// Closed scalar evidence for one selected entry and its exact accepted table.
///
/// `table_sha256` identifies the full table but does not retain table bytes,
/// an image, a descriptor, a path, or any reusable read authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedGptPartitionRole {
    role: GptPartitionRole,
    partition_uuid: [u8; 36],
    start_lba: u64,
    size_lba: u64,
    table_sha256: [u8; 32],
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

    /// Domain-separated SHA-256 of the complete accepted GPT table.
    pub(crate) const fn table_sha256(&self) -> &[u8; 32] {
        &self.table_sha256
    }
}

/// Authenticate one exact GPT entry and table identity from a complete,
/// caller-owned image.
///
/// The immutable slice is parsed twice; every read, allocation, validation,
/// exact comparison, hash step, and terminal check shares one hard resource
/// ledger and the supplied absolute deadline.  The logical block size must be
/// a power of two from 512 through 65,536 bytes, inclusive.
pub(crate) fn authenticate_gpt_partition_role_image_until(
    image: &[u8],
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: &str,
    expected_role: GptPartitionRole,
    deadline: Instant,
) -> io::Result<ValidatedGptPartitionRole> {
    let mut first_source = reader::SliceImage::new(image);
    let mut second_source = reader::SliceImage::new(image);
    authenticate_gpt_partition_role_sources_until(
        &mut first_source,
        &mut second_source,
        logical_block_size,
        expected_partition_number,
        expected_partition_uuid,
        expected_role,
        deadline,
    )
}

/// Pure two-pass seam for a future retained block-device reader.
///
/// Visibility is deliberately limited to the `linux_fs` module tree.  Both
/// sources must independently authenticate to one exact bounded snapshot, and
/// only scalar evidence survives their exact comparison.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(in crate::linux_fs) fn authenticate_gpt_partition_role_sources_until(
    first_source: &mut impl GptPartitionRoleImage,
    second_source: &mut impl GptPartitionRoleImage,
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: &str,
    expected_role: GptPartitionRole,
    deadline: Instant,
) -> io::Result<ValidatedGptPartitionRole> {
    let expected_uuid = Guid::parse_canonical(expected_partition_uuid, deadline)?;
    let stable = stable::authenticate_until(
        first_source,
        second_source,
        logical_block_size,
        expected_partition_number,
        expected_uuid,
        expected_role,
        reader::Limits::production(),
        deadline,
    )?;
    Ok(validated_from_stable(stable))
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
    let mut first_source = reader::SliceImage::new(image);
    let mut second_source = reader::SliceImage::new(image);
    let stable = stable::authenticate_until(
        &mut first_source,
        &mut second_source,
        logical_block_size,
        expected_partition_number,
        expected_uuid,
        expected_role,
        limits.into(),
        deadline,
    )?;
    Ok(validated_from_stable(stable))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn authenticate_gpt_partition_role_two_image_fixture_until(
    first_image: &[u8],
    second_image: &[u8],
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: &str,
    expected_role: GptPartitionRole,
    limits: FixtureGptPartitionRoleLimits,
    deadline: Instant,
) -> io::Result<ValidatedGptPartitionRole> {
    let expected_uuid = Guid::parse_canonical(expected_partition_uuid, deadline)?;
    let mut first_source = reader::SliceImage::new(first_image);
    let mut second_source = reader::SliceImage::new(second_image);
    let stable = stable::authenticate_until(
        &mut first_source,
        &mut second_source,
        logical_block_size,
        expected_partition_number,
        expected_uuid,
        expected_role,
        limits.into(),
        deadline,
    )?;
    Ok(validated_from_stable(stable))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(in crate::linux_fs) fn authenticate_gpt_partition_role_two_sources_fixture_with_clock_until(
    first_source: &mut impl GptPartitionRoleImage,
    second_source: &mut impl GptPartitionRoleImage,
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: &str,
    expected_role: GptPartitionRole,
    limits: FixtureGptPartitionRoleLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> io::Result<ValidatedGptPartitionRole> {
    let expected_uuid = Guid::parse_canonical(expected_partition_uuid, deadline)?;
    let stable = stable::authenticate_with_clock_until(
        first_source,
        second_source,
        logical_block_size,
        expected_partition_number,
        expected_uuid,
        expected_role,
        limits.into(),
        deadline,
        Some(clock),
    )?;
    Ok(validated_from_stable(stable))
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
    let mut first_source = reader::ChunkedSliceImage::new(image, max_chunk, stop_after);
    let mut second_source = reader::ChunkedSliceImage::new(image, max_chunk, stop_after);
    let stable = stable::authenticate_until(
        &mut first_source,
        &mut second_source,
        logical_block_size,
        expected_partition_number,
        expected_uuid,
        expected_role,
        reader::Limits::production(),
        deadline,
    )?;
    Ok(validated_from_stable(stable))
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
    let mut first_source = reader::SliceImage::new(image);
    let mut second_source = reader::SliceImage::new(image);
    let stable = stable::authenticate_with_clock_until(
        &mut first_source,
        &mut second_source,
        logical_block_size,
        expected_partition_number,
        expected_uuid,
        expected_role,
        limits.into(),
        deadline,
        Some(clock),
    )?;
    Ok(validated_from_stable(stable))
}

fn validated_from_stable(stable: stable::StableSelectedEntry) -> ValidatedGptPartitionRole {
    let selected = stable.selected;
    ValidatedGptPartitionRole {
        role: selected.role,
        partition_uuid: selected.partition_uuid.canonical_bytes(),
        start_lba: selected.start_lba,
        size_lba: selected.size_lba,
        table_sha256: stable.table_sha256,
    }
}
