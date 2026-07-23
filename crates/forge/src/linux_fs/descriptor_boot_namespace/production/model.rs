use std::mem::size_of;

pub(super) const RAW_DIRECTORY_RECORD_ALIGNMENT_BYTES: usize = size_of::<usize>();
const _: () = assert!(RAW_DIRECTORY_RECORD_ALIGNMENT_BYTES == 4 || RAW_DIRECTORY_RECORD_ALIGNMENT_BYTES == 8);
pub(super) const RAW_DIRECTORY_MAXIMUM_NAME_BYTES: usize = 255;
pub(super) const RAW_DIRECTORY_MAXIMUM_RECORD_BYTES: usize = {
    const UNALIGNED: usize = 19 + RAW_DIRECTORY_MAXIMUM_NAME_BYTES + 1;
    UNALIGNED.div_ceil(RAW_DIRECTORY_RECORD_ALIGNMENT_BYTES) * RAW_DIRECTORY_RECORD_ALIGNMENT_BYTES
};
pub(super) const RAW_DIRECTORY_READ_BUFFER_BYTES: usize = 32 * 1024;
pub(super) const HARD_MAX_RAW_DIRECTORY_RECORDS: usize = 65_536;
pub(super) const HARD_MAX_RAW_DIRECTORY_NAME_BYTES: usize = 8 * 1024 * 1024;
pub(super) const HARD_MAX_RAW_DIRECTORY_READ_BYTES: usize = 32 * 1024 * 1024;
pub(super) const HARD_MAX_RAW_DIRECTORY_READ_CALLS: usize = 131_072;
pub(super) const HARD_MAX_RAW_DIRECTORY_WORK: usize = 64 * 1024 * 1024;
pub(super) const HARD_MAX_RAW_DIRECTORY_ALLOCATION_ATTEMPTS: usize = 131_072;
pub(super) const HARD_MAX_RAW_DIRECTORY_ALLOCATION_BYTES: usize = 16 * 1024 * 1024;

/// Independent limits for one raw inventory pass.
///
/// Every field is validated against a compile-time production ceiling before
/// the source clock or source reader is observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProductionRawDirectoryInventoryLimits {
    pub(crate) max_records: usize,
    pub(crate) max_name_bytes: usize,
    pub(crate) max_read_bytes: usize,
    pub(crate) max_read_calls: usize,
    pub(crate) max_work: usize,
    pub(crate) max_allocation_attempts: usize,
    pub(crate) max_allocation_bytes: usize,
}

impl Default for ProductionRawDirectoryInventoryLimits {
    fn default() -> Self {
        Self {
            max_records: HARD_MAX_RAW_DIRECTORY_RECORDS,
            max_name_bytes: HARD_MAX_RAW_DIRECTORY_NAME_BYTES,
            max_read_bytes: HARD_MAX_RAW_DIRECTORY_READ_BYTES,
            max_read_calls: HARD_MAX_RAW_DIRECTORY_READ_CALLS,
            max_work: HARD_MAX_RAW_DIRECTORY_WORK,
            max_allocation_attempts: HARD_MAX_RAW_DIRECTORY_ALLOCATION_ATTEMPTS,
            max_allocation_bytes: HARD_MAX_RAW_DIRECTORY_ALLOCATION_BYTES,
        }
    }
}

/// Exact per-pass accounting. A future retained observer must subtract this
/// from its operation-wide budget before starting another inventory pass.
/// `read_bytes` counts accepted record bytes; bounded kernel-read admission is
/// `read_bytes + eof_probe_capacity_bytes` because the terminal probe may
/// transfer one maximum-size record before proving that the byte limit ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProductionRawDirectoryInventoryUsage {
    pub(crate) records: usize,
    pub(crate) name_bytes: usize,
    pub(crate) read_bytes: usize,
    pub(crate) read_calls: usize,
    pub(crate) eof_probes: usize,
    pub(crate) eof_probe_capacity_bytes: usize,
    pub(crate) work: usize,
    pub(crate) allocation_attempts: usize,
    pub(crate) allocation_bytes: usize,
}
