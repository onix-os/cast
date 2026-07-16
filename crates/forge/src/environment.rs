/// Stable user-facing identity of the Cast executable.
pub const NAME: &str = "cast";
/// Max concurrency for disk tasks
pub const MAX_DISK_CONCURRENCY: usize = 16;
/// Max concurrency for network tasks
pub const MAX_NETWORK_CONCURRENCY: usize = 8;
/// Buffer size used when reading a file, 4 MiB
pub const FILE_READ_BUFFER_SIZE: usize = 4 * 1024 * 1024;
/// Threshold to begin chunking file during read, 16 KiB
pub const FILE_READ_CHUNK_THRESHOLD: usize = 16 * 1024;
