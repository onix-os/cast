pub(super) const MIN_LOGICAL_BLOCK_SIZE: u32 = 512;
pub(super) const MAX_LOGICAL_BLOCK_SIZE: u32 = 65_536;

pub(super) const PMBR_BYTES: usize = 512;
pub(super) const PMBR_ENTRY_OFFSET: usize = 446;
pub(super) const PMBR_ENTRY_BYTES: usize = 16;
pub(super) const PMBR_SIGNATURE_OFFSET: usize = 510;

pub(super) const GPT_SIGNATURE: [u8; 8] = *b"EFI PART";
pub(super) const GPT_REVISION_1_0: u32 = 0x0001_0000;
pub(super) const GPT_HEADER_BYTES: usize = 92;
pub(super) const GPT_ENTRY_BYTES: u32 = 128;
pub(super) const MIN_GPT_ENTRIES: u32 = 128;
pub(super) const MAX_GPT_ENTRIES: u32 = 4_096;
pub(super) const MAX_ENTRY_ARRAY_BYTES: usize = 512 * 1024;

pub(super) const READ_CHUNK_BYTES: usize = 64 * 1024;
// These are cumulative hard ceilings for both accepted-table passes, exact
// snapshot comparison, and fingerprinting.  Fixture limits may lower but
// never raise them.
pub(super) const MAX_READ_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MAX_READ_CALLS: usize = 1_024;
pub(super) const MAX_WORK: usize = 16 * 1024 * 1024;
// Two authenticated passes share one cumulative ledger.  Four MiB admits two
// maximum-profile snapshots plus their temporary validation indexes while
// remaining a hard ceiling rather than a caller-controlled allocation size.
pub(super) const MAX_ALLOCATION_BYTES: usize = 4 * 1024 * 1024;

pub(super) const ESP_TYPE_GUID: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];

pub(super) const XBOOTLDR_TYPE_GUID: [u8; 16] = [
    0xff, 0xc2, 0x13, 0xbc, 0xe6, 0x59, 0x62, 0x42, 0xa3, 0x52, 0xb2, 0x75, 0xfd, 0x6f, 0x71, 0x72,
];
