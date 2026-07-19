use std::time::{Duration, Instant};

use crate::linux_fs::gpt_partition_role::{GptPartitionRole, ValidatedGptPartitionRole};

pub(super) const ESP_TYPE_GUID: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];
pub(super) const XBOOTLDR_TYPE_GUID: [u8; 16] = [
    0xff, 0xc2, 0x13, 0xbc, 0xe6, 0x59, 0x62, 0x42, 0xa3, 0x52, 0xb2, 0x75, 0xfd, 0x6f, 0x71, 0x72,
];
pub(super) const ESP_UUID: &str = "00112233-4455-6677-8899-aabbccddeeff";
pub(super) const XBOOTLDR_UUID: &str = "10213243-5465-7687-98a9-bacbdcedfe0f";
pub(super) const SECOND_UUID: &str = "fedcba98-7654-3210-fedc-ba9876543210";

const HEADER_BYTES: usize = 92;
const ENTRY_BYTES: usize = 128;
const DEFAULT_ENTRY_COUNT: u32 = 128;

pub(super) fn live_deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

pub(super) struct Fixture {
    pub(super) bytes: Vec<u8>,
    pub(super) block_size: u32,
    pub(super) entry_count: u32,
    pub(super) primary_header_lba: u64,
    pub(super) backup_header_lba: u64,
    pub(super) primary_array_lba: u64,
    pub(super) backup_array_lba: u64,
    pub(super) first_usable_lba: u64,
    pub(super) last_usable_lba: u64,
    pub(super) selected_start_lba: u64,
    pub(super) selected_size_lba: u64,
    pub(super) selected_uuid: &'static str,
    pub(super) selected_role: GptPartitionRole,
}

impl Fixture {
    pub(super) fn esp(block_size: u32) -> Self {
        Self::new(
            block_size,
            aligned_default_entry_count(block_size),
            GptPartitionRole::Esp,
            ESP_UUID,
        )
    }

    pub(super) fn xbootldr(block_size: u32) -> Self {
        Self::new(
            block_size,
            aligned_default_entry_count(block_size),
            GptPartitionRole::Xbootldr,
            XBOOTLDR_UUID,
        )
    }

    pub(super) fn with_entry_count(block_size: u32, entry_count: u32) -> Self {
        Self::new(block_size, entry_count, GptPartitionRole::Esp, ESP_UUID)
    }

    fn new(block_size: u32, entry_count: u32, selected_role: GptPartitionRole, selected_uuid: &'static str) -> Self {
        let block_bytes = block_size as usize;
        let array_bytes = entry_count as usize * ENTRY_BYTES;
        let array_lbas = array_bytes.div_ceil(block_bytes) as u64;
        let total_lbas = array_lbas * 2 + 256;
        let backup_header_lba = total_lbas - 1;
        let primary_array_lba = 2;
        let backup_array_lba = backup_header_lba - array_lbas;
        let first_usable_lba = primary_array_lba + array_lbas;
        let last_usable_lba = backup_array_lba - 1;
        let selected_start_lba = first_usable_lba + 8;
        let selected_size_lba = 32;
        let mut fixture = Self {
            bytes: vec![0_u8; total_lbas as usize * block_bytes],
            block_size,
            entry_count,
            primary_header_lba: 1,
            backup_header_lba,
            primary_array_lba,
            backup_array_lba,
            first_usable_lba,
            last_usable_lba,
            selected_start_lba,
            selected_size_lba,
            selected_uuid,
            selected_role,
        };
        fixture.write_pmbr();
        let type_guid = match selected_role {
            GptPartitionRole::Esp => ESP_TYPE_GUID,
            GptPartitionRole::Xbootldr => XBOOTLDR_TYPE_GUID,
        };
        fixture.write_entry(
            1,
            type_guid,
            guid_disk_bytes(selected_uuid),
            selected_start_lba,
            selected_start_lba + selected_size_lba - 1,
        );
        fixture.write_entry(
            2,
            [0x5a; 16],
            guid_disk_bytes(SECOND_UUID),
            selected_start_lba + 64,
            selected_start_lba + 95,
        );
        fixture.copy_primary_array_to_backup();
        fixture.rebuild_headers();
        fixture
    }

    pub(super) fn authenticate(&self) -> std::io::Result<ValidatedGptPartitionRole> {
        crate::linux_fs::gpt_partition_role::authenticate_gpt_partition_role_image_until(
            &self.bytes,
            self.block_size,
            1,
            self.selected_uuid,
            self.selected_role,
            live_deadline(),
        )
    }

    pub(super) fn block_offset(&self, lba: u64) -> usize {
        lba as usize * self.block_size as usize
    }

    pub(super) fn primary_header_offset(&self) -> usize {
        self.block_offset(self.primary_header_lba)
    }

    pub(super) fn backup_header_offset(&self) -> usize {
        self.block_offset(self.backup_header_lba)
    }

    pub(super) fn primary_array_offset(&self) -> usize {
        self.block_offset(self.primary_array_lba)
    }

    pub(super) fn backup_array_offset(&self) -> usize {
        self.block_offset(self.backup_array_lba)
    }

    pub(super) fn array_bytes(&self) -> usize {
        self.entry_count as usize * ENTRY_BYTES
    }

    pub(super) fn write_entry(
        &mut self,
        partition_number: u32,
        type_guid: [u8; 16],
        unique_guid: [u8; 16],
        start_lba: u64,
        end_lba: u64,
    ) {
        let entry_offset = self.primary_array_offset() + (partition_number as usize - 1) * ENTRY_BYTES;
        let entry = &mut self.bytes[entry_offset..entry_offset + ENTRY_BYTES];
        entry.fill(0);
        entry[0..16].copy_from_slice(&type_guid);
        entry[16..32].copy_from_slice(&unique_guid);
        entry[32..40].copy_from_slice(&start_lba.to_le_bytes());
        entry[40..48].copy_from_slice(&end_lba.to_le_bytes());
    }

    pub(super) fn clear_entry(&mut self, partition_number: u32) {
        let entry_offset = self.primary_array_offset() + (partition_number as usize - 1) * ENTRY_BYTES;
        self.bytes[entry_offset..entry_offset + ENTRY_BYTES].fill(0);
    }

    pub(super) fn copy_primary_array_to_backup(&mut self) {
        let primary = self.primary_array_offset();
        let backup = self.backup_array_offset();
        let count = self.array_bytes();
        self.bytes.copy_within(primary..primary + count, backup);
    }

    pub(super) fn rebuild_arrays_and_headers(&mut self) {
        self.copy_primary_array_to_backup();
        self.rebuild_headers();
    }

    pub(super) fn rebuild_headers(&mut self) {
        let array_start = self.primary_array_offset();
        let array_crc = crc32(&self.bytes[array_start..array_start + self.array_bytes()]);
        self.write_header(
            self.primary_header_lba,
            self.backup_header_lba,
            self.primary_array_lba,
            array_crc,
        );
        self.write_header(
            self.backup_header_lba,
            self.primary_header_lba,
            self.backup_array_lba,
            array_crc,
        );
    }

    pub(super) fn set_disk_guid(&mut self, disk_guid: [u8; 16]) {
        for lba in [self.primary_header_lba, self.backup_header_lba] {
            let offset = self.block_offset(lba) + 56;
            self.bytes[offset..offset + 16].copy_from_slice(&disk_guid);
            self.repair_header_crc(lba);
        }
    }

    pub(super) fn repair_header_crc(&mut self, lba: u64) {
        let offset = self.block_offset(lba);
        self.bytes[offset + 16..offset + 20].fill(0);
        let checksum = crc32(&self.bytes[offset..offset + HEADER_BYTES]);
        self.bytes[offset + 16..offset + 20].copy_from_slice(&checksum.to_le_bytes());
    }

    fn write_pmbr(&mut self) {
        self.bytes[440..446].fill(0);
        let total_lbas = self.bytes.len() as u64 / u64::from(self.block_size);
        let entry = &mut self.bytes[446..462];
        entry.fill(0);
        entry[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
        entry[4] = 0xee;
        entry[5..8].copy_from_slice(&[0xff, 0xff, 0xff]);
        entry[8..12].copy_from_slice(&1_u32.to_le_bytes());
        let covered = (total_lbas - 1).min(u64::from(u32::MAX)) as u32;
        entry[12..16].copy_from_slice(&covered.to_le_bytes());
        self.bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
    }

    fn write_header(&mut self, current_lba: u64, alternate_lba: u64, array_lba: u64, array_crc: u32) {
        let offset = self.block_offset(current_lba);
        let block = &mut self.bytes[offset..offset + self.block_size as usize];
        block.fill(0);
        block[0..8].copy_from_slice(b"EFI PART");
        block[8..12].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
        block[12..16].copy_from_slice(&(HEADER_BYTES as u32).to_le_bytes());
        block[24..32].copy_from_slice(&current_lba.to_le_bytes());
        block[32..40].copy_from_slice(&alternate_lba.to_le_bytes());
        block[40..48].copy_from_slice(&self.first_usable_lba.to_le_bytes());
        block[48..56].copy_from_slice(&self.last_usable_lba.to_le_bytes());
        block[56..72].copy_from_slice(&[0x3c; 16]);
        block[72..80].copy_from_slice(&array_lba.to_le_bytes());
        block[80..84].copy_from_slice(&self.entry_count.to_le_bytes());
        block[84..88].copy_from_slice(&(ENTRY_BYTES as u32).to_le_bytes());
        block[88..92].copy_from_slice(&array_crc.to_le_bytes());
        let checksum = crc32(&block[..HEADER_BYTES]);
        block[16..20].copy_from_slice(&checksum.to_le_bytes());
    }
}

fn aligned_default_entry_count(block_size: u32) -> u32 {
    DEFAULT_ENTRY_COUNT.max(block_size / ENTRY_BYTES as u32)
}

pub(super) fn guid_disk_bytes(value: &str) -> [u8; 16] {
    let compact: Vec<u8> = value.bytes().filter(|byte| *byte != b'-').collect();
    let mut canonical = [0_u8; 16];
    for (index, pair) in compact.chunks_exact(2).enumerate() {
        canonical[index] = (hex(pair[0]) << 4) | hex(pair[1]);
    }
    [
        canonical[3],
        canonical[2],
        canonical[1],
        canonical[0],
        canonical[5],
        canonical[4],
        canonical[7],
        canonical[6],
        canonical[8],
        canonical[9],
        canonical[10],
        canonical[11],
        canonical[12],
        canonical[13],
        canonical[14],
        canonical[15],
    ]
}

fn hex(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => panic!("fixture UUID must be lowercase hexadecimal"),
    }
}

pub(super) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

pub(super) fn change_bytes_without_changing_crc(bytes: &mut [u8]) {
    let original = bytes.to_vec();
    let original_crc = crc32(&original);
    let mut basis: [Option<(u32, u64)>; 32] = [None; 32];
    let mut dependency = None;
    for candidate in 0..33usize {
        let mut changed = original.clone();
        changed[48 + candidate / 8] ^= 1 << (candidate % 8);
        let mut delta = crc32(&changed) ^ original_crc;
        let mut combination = 1_u64 << candidate;
        for pivot in (0..32usize).rev() {
            if delta & (1_u32 << pivot) == 0 {
                continue;
            }
            if let Some((basis_delta, basis_combination)) = basis[pivot] {
                delta ^= basis_delta;
                combination ^= basis_combination;
            } else {
                basis[pivot] = Some((delta, combination));
                break;
            }
        }
        if delta == 0 {
            dependency = Some(combination);
            break;
        }
    }
    let dependency = dependency.expect("33 CRC deltas must be linearly dependent");
    for candidate in 0..33usize {
        if dependency & (1_u64 << candidate) != 0 {
            bytes[48 + candidate / 8] ^= 1 << (candidate % 8);
        }
    }
    assert_ne!(bytes, original);
    assert_eq!(crc32(bytes), original_crc);
}
