use std::{io, time::Instant};

use super::{SysfsPartitionUuid, invalid_data, require_deadline};

pub(super) fn canonical_partition_uuid(bytes: &[u8]) -> io::Result<SysfsPartitionUuid> {
    canonical_partition_uuid_with_deadline(bytes, None)
}

pub(super) fn canonical_partition_uuid_until(bytes: &[u8], deadline: Instant) -> io::Result<SysfsPartitionUuid> {
    canonical_partition_uuid_with_deadline(bytes, Some(deadline))
}

fn canonical_partition_uuid_with_deadline(bytes: &[u8], deadline: Option<Instant>) -> io::Result<SysfsPartitionUuid> {
    require_deadline(deadline)?;
    if bytes.len() != 36 {
        return Err(invalid_data(
            "sysfs PARTUUID is not one lowercase canonical 8-4-4-4-12 UUID",
        ));
    }

    let mut nonzero = false;
    for (index, byte) in bytes.iter().enumerate() {
        require_deadline(deadline)?;
        let canonical = if matches!(index, 8 | 13 | 18 | 23) {
            *byte == b'-'
        } else {
            nonzero |= *byte != b'0';
            byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
        };
        if !canonical {
            return Err(invalid_data(
                "sysfs PARTUUID is not one lowercase canonical 8-4-4-4-12 UUID",
            ));
        }
    }
    if !nonzero {
        return Err(invalid_data("the nil UUID is not a partition identity"));
    }

    let mut uuid = [0_u8; 36];
    uuid.copy_from_slice(bytes);
    require_deadline(deadline)?;
    Ok(SysfsPartitionUuid(uuid))
}
