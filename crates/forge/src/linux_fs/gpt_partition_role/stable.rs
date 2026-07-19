use std::{io, time::Instant};

use super::{
    GptPartitionRole, fingerprint,
    guid::Guid,
    parser::{self, SelectedEntry},
    reader::{Image, Limits, Operation},
};

pub(super) struct StableSelectedEntry {
    pub(super) selected: SelectedEntry,
    pub(super) logical_block_size: u32,
    pub(super) image_bytes: u64,
    pub(super) table_sha256: [u8; 32],
}

#[allow(clippy::too_many_arguments)]
pub(super) fn authenticate_until(
    first_source: &mut impl Image,
    second_source: &mut impl Image,
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: Guid,
    expected_role: GptPartitionRole,
    limits: Limits,
    deadline: Instant,
) -> io::Result<StableSelectedEntry> {
    let mut interpass = |_deadline| Ok(());
    authenticate_with_interpass_and_clock_until(
        first_source,
        second_source,
        logical_block_size,
        expected_partition_number,
        expected_partition_uuid,
        expected_role,
        limits,
        deadline,
        &mut interpass,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn authenticate_with_interpass_until(
    first_source: &mut impl Image,
    second_source: &mut impl Image,
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: Guid,
    expected_role: GptPartitionRole,
    limits: Limits,
    deadline: Instant,
    interpass: &mut impl FnMut(Instant) -> io::Result<()>,
) -> io::Result<StableSelectedEntry> {
    authenticate_with_interpass_and_clock_until(
        first_source,
        second_source,
        logical_block_size,
        expected_partition_number,
        expected_partition_uuid,
        expected_role,
        limits,
        deadline,
        interpass,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn authenticate_with_clock_until(
    first_source: &mut impl Image,
    second_source: &mut impl Image,
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: Guid,
    expected_role: GptPartitionRole,
    limits: Limits,
    deadline: Instant,
    clock: Option<&mut dyn FnMut() -> Instant>,
) -> io::Result<StableSelectedEntry> {
    let mut interpass = |_deadline| Ok(());
    authenticate_with_interpass_and_clock_until(
        first_source,
        second_source,
        logical_block_size,
        expected_partition_number,
        expected_partition_uuid,
        expected_role,
        limits,
        deadline,
        &mut interpass,
        clock,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn authenticate_with_interpass_and_clock_until(
    first_source: &mut impl Image,
    second_source: &mut impl Image,
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: Guid,
    expected_role: GptPartitionRole,
    limits: Limits,
    deadline: Instant,
    interpass: &mut impl FnMut(Instant) -> io::Result<()>,
    clock: Option<&mut dyn FnMut() -> Instant>,
) -> io::Result<StableSelectedEntry> {
    let mut operation = Operation::new_with_clock(limits, deadline, clock)?;
    let first = parser::authenticate_with_operation(
        first_source,
        logical_block_size,
        expected_partition_number,
        expected_partition_uuid,
        expected_role,
        &mut operation,
    )?;
    operation.checkpoint()?;
    interpass(deadline)?;
    operation.checkpoint()?;
    let second = parser::authenticate_with_operation(
        second_source,
        logical_block_size,
        expected_partition_number,
        expected_partition_uuid,
        expected_role,
        &mut operation,
    )?;
    first.require_exact_match(&second, &mut operation)?;
    let table_sha256 = fingerprint::table_sha256(&first, &mut operation)?;
    let selected = first.selected();
    let logical_block_size = first.logical_block_size();
    let image_bytes = first.image_bytes();
    operation.checkpoint()?;
    Ok(StableSelectedEntry {
        selected,
        logical_block_size,
        image_bytes,
        table_sha256,
    })
}
