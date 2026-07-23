use std::{cmp, io};

use super::{
    GptPartitionRole, constants,
    guid::Guid,
    reader::{Image, Operation},
    snapshot::AuthenticatedGptTableSnapshot,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SelectedEntry {
    pub(super) partition_number: u32,
    pub(super) role: GptPartitionRole,
    pub(super) partition_uuid: Guid,
    pub(super) start_lba: u64,
    pub(super) size_lba: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Header {
    current_lba: u64,
    alternate_lba: u64,
    first_usable_lba: u64,
    last_usable_lba: u64,
    disk_guid: Guid,
    entry_lba: u64,
    entry_count: u32,
    entry_bytes: u32,
    entry_crc32: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UsedRange {
    start: u64,
    end: u64,
    index: u32,
}

pub(super) fn authenticate_with_operation(
    source: &mut impl Image,
    logical_block_size: u32,
    expected_partition_number: u32,
    expected_partition_uuid: Guid,
    expected_role: GptPartitionRole,
    operation: &mut Operation<'_>,
) -> io::Result<AuthenticatedGptTableSnapshot> {
    operation.checkpoint()?;
    let block_bytes = validate_logical_block_size(logical_block_size)?;
    if expected_partition_number == 0 {
        return Err(invalid_input("expected GPT partition number must be positive"));
    }
    if expected_partition_uuid.is_zero() {
        return Err(invalid_input("expected GPT partition UUID must be nonzero"));
    }

    let image_bytes = source.length();
    let block_bytes_u64: u64 = block_bytes
        .try_into()
        .map_err(|_| invalid_data("logical block size is not representable"))?;
    if image_bytes % block_bytes_u64 != 0 {
        return Err(invalid_data("GPT image length is not a whole number of logical blocks"));
    }
    let total_lbas = image_bytes / block_bytes_u64;
    if total_lbas < 6 {
        return Err(invalid_data(
            "GPT image is too small for redundant metadata and usable space",
        ));
    }
    let last_lba = total_lbas - 1;

    let pmbr_bytes = validate_pmbr(source, total_lbas, block_bytes, operation)?;
    let primary_bytes = read_block(source, 1, block_bytes, operation, "reading primary GPT header")?;
    let backup_bytes = read_block(source, last_lba, block_bytes, operation, "reading backup GPT header")?;
    let primary = parse_header(&primary_bytes, operation, "primary GPT header")?;
    let backup = parse_header(&backup_bytes, operation, "backup GPT header")?;
    validate_header_pair(primary, backup, last_lba, block_bytes_u64)?;

    if expected_partition_number > primary.entry_count {
        return Err(invalid_input("expected GPT partition number exceeds the entry array"));
    }
    let array_bytes = entry_array_bytes(primary.entry_count, primary.entry_bytes)?;
    let array_bytes_u64: u64 = array_bytes
        .try_into()
        .map_err(|_| invalid_data("GPT entry array length is not representable"))?;
    if array_bytes_u64 % block_bytes_u64 != 0 {
        return Err(invalid_data(
            "GPT entry-array byte length is not aligned to the logical block size",
        ));
    }
    let array_lbas = div_ceil_u64(array_bytes_u64, block_bytes_u64)?;
    validate_metadata_layout(primary, backup, array_lbas, last_lba)?;

    let primary_array = read_region(
        source,
        primary.entry_lba,
        array_bytes,
        block_bytes_u64,
        operation,
        "reading primary GPT entry array",
    )?;
    let backup_array = read_region(
        source,
        backup.entry_lba,
        array_bytes,
        block_bytes_u64,
        operation,
        "reading backup GPT entry array",
    )?;
    if crc32(&primary_array, operation)? != primary.entry_crc32 {
        return Err(invalid_data("primary GPT entry-array CRC32 does not match"));
    }
    if crc32(&backup_array, operation)? != backup.entry_crc32 {
        return Err(invalid_data("backup GPT entry-array CRC32 does not match"));
    }
    operation.charge_work(array_bytes, "comparing redundant GPT entry arrays")?;
    if primary_array != backup_array {
        return Err(invalid_data("primary and backup GPT entry arrays disagree"));
    }

    let selected = validate_entries(
        &primary_array,
        primary,
        expected_partition_number,
        expected_partition_uuid,
        expected_role,
        operation,
    )?;
    operation.checkpoint()?;
    Ok(AuthenticatedGptTableSnapshot::new(
        image_bytes,
        logical_block_size,
        pmbr_bytes,
        primary_bytes,
        backup_bytes,
        primary_array,
        selected,
    ))
}

fn validate_logical_block_size(value: u32) -> io::Result<usize> {
    if value < constants::MIN_LOGICAL_BLOCK_SIZE
        || value > constants::MAX_LOGICAL_BLOCK_SIZE
        || !value.is_power_of_two()
    {
        return Err(invalid_input(
            "logical block size must be a power of two from 512 through 65536 bytes",
        ));
    }
    value
        .try_into()
        .map_err(|_| invalid_input("logical block size is not representable"))
}

fn validate_pmbr(
    source: &mut impl Image,
    total_lbas: u64,
    block_bytes: usize,
    operation: &mut Operation<'_>,
) -> io::Result<Vec<u8>> {
    let mut bytes = operation.allocate_zeroed(block_bytes, "protective MBR logical-block buffer")?;
    operation.read_exact(source, 0, &mut bytes, "reading protective MBR")?;
    operation.charge_work(bytes.len(), "validating protective MBR")?;
    if bytes[constants::PMBR_SIGNATURE_OFFSET..constants::PMBR_SIGNATURE_OFFSET + 2] != [0x55, 0xaa] {
        return Err(invalid_data("protective MBR signature is invalid"));
    }
    if bytes[440..446].iter().any(|byte| *byte != 0) {
        return Err(invalid_data(
            "protective MBR disk signature or reserved bytes are nonzero",
        ));
    }
    if bytes[constants::PMBR_BYTES..].iter().any(|byte| *byte != 0) {
        return Err(invalid_data("protective MBR logical-block padding is nonzero"));
    }

    let first = &bytes[constants::PMBR_ENTRY_OFFSET..constants::PMBR_ENTRY_OFFSET + constants::PMBR_ENTRY_BYTES];
    if first[0] != 0
        || first[1..4] != [0x00, 0x02, 0x00]
        || first[4] != 0xee
        || first[5..8] != [0xff, 0xff, 0xff]
        || le_u32(&first[8..12])? != 1
    {
        return Err(invalid_data(
            "protective MBR entry is not the strict GPT protective entry",
        ));
    }
    let expected_size = cmp::min(total_lbas - 1, u64::from(u32::MAX)) as u32;
    if le_u32(&first[12..16])? != expected_size {
        return Err(invalid_data("protective MBR size does not cover the GPT image"));
    }
    for index in 1..4 {
        let start = constants::PMBR_ENTRY_OFFSET + index * constants::PMBR_ENTRY_BYTES;
        if bytes[start..start + constants::PMBR_ENTRY_BYTES]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(invalid_data("protective MBR is hybrid rather than GPT-only"));
        }
    }
    operation.checkpoint()?;
    Ok(bytes)
}

fn read_block(
    source: &mut impl Image,
    lba: u64,
    block_bytes: usize,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<Vec<u8>> {
    let block_bytes_u64: u64 = block_bytes
        .try_into()
        .map_err(|_| invalid_data("logical block size is not representable"))?;
    let offset = lba
        .checked_mul(block_bytes_u64)
        .ok_or_else(|| invalid_data("GPT block offset overflowed"))?;
    let mut output = operation.allocate_zeroed(block_bytes, "GPT header block buffer")?;
    operation.read_exact(source, offset, &mut output, action)?;
    Ok(output)
}

fn read_region(
    source: &mut impl Image,
    lba: u64,
    bytes: usize,
    block_bytes: u64,
    operation: &mut Operation<'_>,
    action: &'static str,
) -> io::Result<Vec<u8>> {
    let offset = lba
        .checked_mul(block_bytes)
        .ok_or_else(|| invalid_data("GPT entry-array offset overflowed"))?;
    let length: u64 = bytes
        .try_into()
        .map_err(|_| invalid_data("GPT entry-array length is not representable"))?;
    let end = offset
        .checked_add(length)
        .ok_or_else(|| invalid_data("GPT entry-array extent overflowed"))?;
    if end > source.length() {
        return Err(invalid_data("GPT entry array extends beyond the image"));
    }
    let mut output = operation.allocate_zeroed(bytes, "GPT entry-array buffer")?;
    operation.read_exact(source, offset, &mut output, action)?;
    Ok(output)
}

fn parse_header(bytes: &[u8], operation: &mut Operation<'_>, label: &'static str) -> io::Result<Header> {
    if bytes.len() < constants::GPT_HEADER_BYTES {
        return Err(invalid_data("GPT header block is shorter than the strict header"));
    }
    operation.charge_work(bytes.len(), "validating GPT header block")?;
    if bytes[0..8] != constants::GPT_SIGNATURE {
        return Err(invalid_data(format!("{label} signature is invalid")));
    }
    if le_u32(&bytes[8..12])? != constants::GPT_REVISION_1_0 {
        return Err(invalid_data(format!("{label} revision is not exactly 1.0")));
    }
    if le_u32(&bytes[12..16])? != constants::GPT_HEADER_BYTES as u32 {
        return Err(invalid_data(format!("{label} size is not exactly 92 bytes")));
    }
    if le_u32(&bytes[20..24])? != 0 {
        return Err(invalid_data(format!("{label} reserved field is nonzero")));
    }
    if bytes[constants::GPT_HEADER_BYTES..].iter().any(|byte| *byte != 0) {
        return Err(invalid_data(format!("{label} trailing reserved bytes are nonzero")));
    }
    let expected_crc = le_u32(&bytes[16..20])?;
    let mut crc_bytes = [0_u8; constants::GPT_HEADER_BYTES];
    crc_bytes.copy_from_slice(&bytes[..constants::GPT_HEADER_BYTES]);
    crc_bytes[16..20].fill(0);
    if crc32(&crc_bytes, operation)? != expected_crc {
        return Err(invalid_data(format!("{label} CRC32 does not match")));
    }

    let disk_guid = Guid::from_disk_slice(&bytes[56..72])?;
    if disk_guid.is_zero() {
        return Err(invalid_data(format!("{label} disk GUID is zero")));
    }
    let entry_count = le_u32(&bytes[80..84])?;
    if !(constants::MIN_GPT_ENTRIES..=constants::MAX_GPT_ENTRIES).contains(&entry_count) {
        return Err(invalid_data(format!(
            "{label} entry count is outside the strict 128 through 4096 range"
        )));
    }
    let entry_bytes = le_u32(&bytes[84..88])?;
    if entry_bytes != constants::GPT_ENTRY_BYTES {
        return Err(invalid_data(format!("{label} entry size is not exactly 128 bytes")));
    }
    entry_array_bytes(entry_count, entry_bytes)?;

    Ok(Header {
        current_lba: le_u64(&bytes[24..32])?,
        alternate_lba: le_u64(&bytes[32..40])?,
        first_usable_lba: le_u64(&bytes[40..48])?,
        last_usable_lba: le_u64(&bytes[48..56])?,
        disk_guid,
        entry_lba: le_u64(&bytes[72..80])?,
        entry_count,
        entry_bytes,
        entry_crc32: le_u32(&bytes[88..92])?,
    })
}

fn validate_header_pair(primary: Header, backup: Header, last_lba: u64, block_bytes: u64) -> io::Result<()> {
    if primary.current_lba != 1 || primary.alternate_lba != last_lba {
        return Err(invalid_data("primary GPT header locations are not canonical"));
    }
    if backup.current_lba != last_lba || backup.alternate_lba != 1 {
        return Err(invalid_data("backup GPT header locations are not canonical"));
    }
    if primary.first_usable_lba != backup.first_usable_lba
        || primary.last_usable_lba != backup.last_usable_lba
        || primary.disk_guid != backup.disk_guid
        || primary.entry_count != backup.entry_count
        || primary.entry_bytes != backup.entry_bytes
        || primary.entry_crc32 != backup.entry_crc32
    {
        return Err(invalid_data("primary and backup GPT headers disagree"));
    }
    if primary.first_usable_lba > primary.last_usable_lba {
        return Err(invalid_data("GPT usable-LBA range is reversed"));
    }
    if block_bytes == 0 {
        return Err(invalid_data("logical block size is zero"));
    }
    Ok(())
}

fn validate_metadata_layout(primary: Header, backup: Header, array_lbas: u64, last_lba: u64) -> io::Result<()> {
    if primary.entry_lba != 2 {
        return Err(invalid_data("primary GPT entry array does not begin at LBA 2"));
    }
    let primary_array_end = primary
        .entry_lba
        .checked_add(array_lbas)
        .ok_or_else(|| invalid_data("primary GPT entry-array extent overflowed"))?;
    let expected_backup_lba = last_lba
        .checked_sub(array_lbas)
        .ok_or_else(|| invalid_data("backup GPT entry-array location underflowed"))?;
    if backup.entry_lba != expected_backup_lba {
        return Err(invalid_data(
            "backup GPT entry array is not immediately before its header",
        ));
    }
    if primary.first_usable_lba < primary_array_end {
        return Err(invalid_data("GPT first usable LBA overlaps primary metadata"));
    }
    if primary.last_usable_lba >= backup.entry_lba {
        return Err(invalid_data("GPT last usable LBA overlaps backup metadata"));
    }
    Ok(())
}

fn validate_entries(
    bytes: &[u8],
    header: Header,
    expected_partition_number: u32,
    expected_partition_uuid: Guid,
    expected_role: GptPartitionRole,
    operation: &mut Operation<'_>,
) -> io::Result<SelectedEntry> {
    let count: usize = header
        .entry_count
        .try_into()
        .map_err(|_| invalid_data("GPT entry count is not representable"))?;
    let mut guids = operation.reserve_items::<Guid>(count, "used GPT GUID index")?;
    let mut ranges = operation.reserve_items::<UsedRange>(count, "used GPT range index")?;
    let mut selected = None;

    for index in 0..count {
        if index % 128 == 0 {
            operation.checkpoint()?;
        }
        operation.charge_work(constants::GPT_ENTRY_BYTES as usize, "validating GPT entry")?;
        let start = index
            .checked_mul(constants::GPT_ENTRY_BYTES as usize)
            .ok_or_else(|| invalid_data("GPT entry offset overflowed"))?;
        let entry = &bytes[start..start + constants::GPT_ENTRY_BYTES as usize];
        let type_guid = Guid::from_disk_slice(&entry[0..16])?;
        if type_guid == Guid::ZERO {
            if entry[16..].iter().any(|byte| *byte != 0) {
                return Err(invalid_data("unused GPT entry contains nonzero fields"));
            }
            continue;
        }
        let partition_uuid = Guid::from_disk_slice(&entry[16..32])?;
        if partition_uuid.is_zero() {
            return Err(invalid_data("used GPT entry has a zero unique GUID"));
        }
        let first_lba = le_u64(&entry[32..40])?;
        let last_lba = le_u64(&entry[40..48])?;
        let attributes = le_u64(&entry[48..56])?;
        const RESERVED_ATTRIBUTE_BITS: u64 = ((1_u64 << 48) - 1) & !0b111;
        if attributes & RESERVED_ATTRIBUTE_BITS != 0 {
            return Err(invalid_data("used GPT entry has nonzero reserved attribute bits"));
        }
        if first_lba > last_lba {
            return Err(invalid_data("used GPT entry has a reversed LBA range"));
        }
        if first_lba < header.first_usable_lba || last_lba > header.last_usable_lba {
            return Err(invalid_data("used GPT entry lies outside the usable-LBA range"));
        }
        let partition_number: u32 = (index + 1)
            .try_into()
            .map_err(|_| invalid_data("GPT partition number is not representable"))?;
        guids.push(partition_uuid);
        ranges.push(UsedRange {
            start: first_lba,
            end: last_lba,
            index: partition_number,
        });

        if partition_number == expected_partition_number {
            if partition_uuid != expected_partition_uuid {
                return Err(invalid_data("selected GPT entry unique GUID does not match PARTUUID"));
            }
            if type_guid != expected_role.type_guid() {
                return Err(invalid_data("selected GPT entry has the wrong boot partition role"));
            }
            let size_lba = last_lba
                .checked_sub(first_lba)
                .and_then(|size| size.checked_add(1))
                .ok_or_else(|| invalid_data("selected GPT entry size overflowed"))?;
            selected = Some(SelectedEntry {
                partition_number,
                role: expected_role,
                partition_uuid,
                start_lba: first_lba,
                size_lba,
            });
        }
    }

    let sort_work = count
        .checked_mul(64)
        .ok_or_else(|| invalid_data("GPT sort work accounting overflowed"))?;
    operation.charge_work(sort_work, "bounding GPT uniqueness and overlap sorts")?;
    guids.sort_unstable();
    for pair in guids.windows(2) {
        if pair[0] == pair[1] {
            return Err(invalid_data("used GPT entries repeat a unique GUID"));
        }
    }
    ranges.sort_unstable_by_key(|range| (range.start, range.end, range.index));
    for pair in ranges.windows(2) {
        if pair[1].start <= pair[0].end {
            return Err(invalid_data("used GPT entries overlap"));
        }
    }
    operation.checkpoint()?;
    selected.ok_or_else(|| invalid_data("selected GPT partition entry is unused"))
}

fn entry_array_bytes(count: u32, entry_bytes: u32) -> io::Result<usize> {
    let bytes = count
        .checked_mul(entry_bytes)
        .ok_or_else(|| invalid_data("GPT entry-array byte count overflowed"))?;
    let bytes: usize = bytes
        .try_into()
        .map_err(|_| invalid_data("GPT entry-array byte count is not representable"))?;
    if bytes > constants::MAX_ENTRY_ARRAY_BYTES {
        return Err(invalid_data("GPT entry array exceeds the 512 KiB limit"));
    }
    Ok(bytes)
}

fn div_ceil_u64(value: u64, divisor: u64) -> io::Result<u64> {
    if divisor == 0 {
        return Err(invalid_data("GPT division uses a zero logical block size"));
    }
    value
        .checked_add(divisor - 1)
        .map(|adjusted| adjusted / divisor)
        .ok_or_else(|| invalid_data("GPT rounded block count overflowed"))
}

fn crc32(bytes: &[u8], operation: &mut Operation<'_>) -> io::Result<u32> {
    operation.charge_work(bytes.len(), "calculating GPT CRC32")?;
    let mut crc = u32::MAX;
    for (index, byte) in bytes.iter().enumerate() {
        if index % 4_096 == 0 {
            operation.checkpoint()?;
        }
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    operation.checkpoint()?;
    Ok(!crc)
}

fn le_u32(bytes: &[u8]) -> io::Result<u32> {
    let bytes: [u8; 4] = bytes
        .try_into()
        .map_err(|_| invalid_data("GPT 32-bit field has an invalid width"))?;
    Ok(u32::from_le_bytes(bytes))
}

fn le_u64(bytes: &[u8]) -> io::Result<u64> {
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| invalid_data("GPT 64-bit field has an invalid width"))?;
    Ok(u64::from_le_bytes(bytes))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
