use std::time::Instant;

use super::{
    budget::ProductionRawDirectoryOperation,
    error::ProductionRawDirectoryInventoryError,
    inventory::ProductionRawDirectoryInventory,
    model::{
        ProductionRawDirectoryInventoryLimits, RAW_DIRECTORY_MAXIMUM_NAME_BYTES, RAW_DIRECTORY_MAXIMUM_RECORD_BYTES,
        RAW_DIRECTORY_READ_BUFFER_BYTES, RAW_DIRECTORY_RECORD_ALIGNMENT_BYTES,
    },
    source::ProductionRawDirectorySource,
};

use super::model::ProductionRawDirectoryInventoryUsage;

const RECORD_LENGTH_OFFSET: usize = 16;
const RECORD_LENGTH_BYTES: usize = 2;
const RAW_NAME_OFFSET: usize = 19;
const MINIMUM_RECORD_BYTES: usize = RAW_NAME_OFFSET + 1;

pub(crate) fn parse_production_raw_directory_inventory_until<Source: ProductionRawDirectorySource>(
    source: &mut Source,
    limits: ProductionRawDirectoryInventoryLimits,
    deadline: Instant,
) -> Result<ProductionRawDirectoryInventory, ProductionRawDirectoryInventoryError> {
    parse_raw_directory_inventory(source, limits, deadline).map(|(inventory, _)| inventory)
}

fn parse_raw_directory_inventory<Source: ProductionRawDirectorySource>(
    source: &mut Source,
    limits: ProductionRawDirectoryInventoryLimits,
    deadline: Instant,
) -> Result<(ProductionRawDirectoryInventory, ProductionRawDirectoryInventoryUsage), ProductionRawDirectoryInventoryError>
{
    let mut operation = ProductionRawDirectoryOperation::new(source, limits, deadline)?;
    let mut inventory = ProductionRawDirectoryInventory::default();
    let mut buffer = [0u8; RAW_DIRECTORY_READ_BUFFER_BYTES];
    loop {
        if operation.remaining_read_bytes() < RAW_DIRECTORY_MAXIMUM_RECORD_BYTES {
            operation.probe_end(&mut buffer[..RAW_DIRECTORY_MAXIMUM_RECORD_BYTES])?;
            break;
        }
        let found = operation.read_chunk(&mut buffer)?;
        if found == 0 {
            break;
        }
        parse_chunk(&mut operation, &mut inventory, &buffer[..found])?;
    }
    operation.checkpoint()?;
    let usage = operation.usage();
    Ok((inventory, usage))
}

pub(crate) fn parse_production_raw_directory_inventory_with_usage_until<Source: ProductionRawDirectorySource>(
    source: &mut Source,
    limits: ProductionRawDirectoryInventoryLimits,
    deadline: Instant,
) -> Result<(ProductionRawDirectoryInventory, ProductionRawDirectoryInventoryUsage), ProductionRawDirectoryInventoryError>
{
    parse_raw_directory_inventory(source, limits, deadline)
}

fn parse_chunk<Source: ProductionRawDirectorySource>(
    operation: &mut ProductionRawDirectoryOperation<'_, Source>,
    inventory: &mut ProductionRawDirectoryInventory,
    chunk: &[u8],
) -> Result<(), ProductionRawDirectoryInventoryError> {
    let mut offset = 0usize;
    while offset < chunk.len() {
        let remaining = chunk.len() - offset;
        if remaining < RAW_NAME_OFFSET {
            return Err(ProductionRawDirectoryInventoryError::TruncatedRecordHeader { offset, remaining });
        }
        let length_start = offset + RECORD_LENGTH_OFFSET;
        let length_end = length_start + RECORD_LENGTH_BYTES;
        let record_length = usize::from(u16::from_ne_bytes(
            chunk[length_start..length_end]
                .try_into()
                .expect("validated getdents64 record-length field bounds"),
        ));
        if record_length < MINIMUM_RECORD_BYTES {
            return Err(ProductionRawDirectoryInventoryError::RecordLengthTooSmall {
                offset,
                found: record_length,
            });
        }
        if record_length % RAW_DIRECTORY_RECORD_ALIGNMENT_BYTES != 0 {
            return Err(ProductionRawDirectoryInventoryError::RecordLengthUnaligned {
                offset,
                found: record_length,
            });
        }
        if record_length > remaining {
            return Err(ProductionRawDirectoryInventoryError::RecordOverrun {
                offset,
                found: record_length,
                remaining,
            });
        }
        operation.charge_record(record_length)?;
        let record_end = offset + record_length;
        let name_region = &chunk[offset + RAW_NAME_OFFSET..record_end];
        let name_length = name_region
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(ProductionRawDirectoryInventoryError::MissingNameTerminator { offset })?;
        if name_length == 0 {
            return Err(ProductionRawDirectoryInventoryError::EmptyName { offset });
        }
        if name_length > RAW_DIRECTORY_MAXIMUM_NAME_BYTES {
            return Err(ProductionRawDirectoryInventoryError::NameTooLong {
                offset,
                limit: RAW_DIRECTORY_MAXIMUM_NAME_BYTES,
                found: name_length,
            });
        }
        let raw_name = &name_region[..name_length];
        if raw_name.contains(&b'/') {
            return Err(ProductionRawDirectoryInventoryError::NameContainsSlash { offset });
        }
        operation.charge_name(name_length)?;
        if raw_name != b"." && raw_name != b".." {
            let (names, entries) = inventory.vectors_mut();
            operation.reserve_entry(names, entries, name_length)?;
            inventory.push_reserved(raw_name);
        }
        offset = record_end;
        operation.checkpoint()?;
    }
    Ok(())
}
