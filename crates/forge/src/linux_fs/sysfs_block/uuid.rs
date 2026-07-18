use std::io;

use super::{SysfsPartitionUuid, invalid_data};

pub(super) fn canonical_partition_uuid(bytes: &[u8]) -> io::Result<SysfsPartitionUuid> {
    let canonical = bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
            }
        });
    if !canonical {
        return Err(invalid_data(
            "sysfs PARTUUID is not one lowercase canonical 8-4-4-4-12 UUID",
        ));
    }
    if bytes.iter().filter(|byte| **byte != b'-').all(|byte| *byte == b'0') {
        return Err(invalid_data("the nil UUID is not a partition identity"));
    }

    let mut uuid = [0_u8; 36];
    uuid.copy_from_slice(bytes);
    Ok(SysfsPartitionUuid(uuid))
}
