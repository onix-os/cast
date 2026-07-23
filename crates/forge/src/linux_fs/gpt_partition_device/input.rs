use super::super::gpt_partition_role::GptPartitionRole;

pub(super) struct ExpectedPartition<'a> {
    pub(super) parent_major: u32,
    pub(super) parent_minor: u32,
    pub(super) partition_number: u32,
    pub(super) partition_uuid: &'a str,
    pub(super) start_512_sectors: u64,
    pub(super) size_512_sectors: u64,
}

pub(super) struct ValidatedPartition<'a> {
    pub(super) role: GptPartitionRole,
    pub(super) partition_number: u32,
    pub(super) partition_uuid: &'a str,
    pub(super) start_lba: u64,
    pub(super) size_lba: u64,
    pub(super) logical_block_size: u32,
    pub(super) image_bytes: u64,
    pub(super) table_sha256: [u8; 32],
}
