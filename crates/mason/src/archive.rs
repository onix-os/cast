//! Fail-closed extraction for locked authored-source tar archives.
//!
//! Archive bytes are parsed twice.  The first pass admits a finite canonical
//! manifest without touching the destination.  The second pass writes only
//! beneath a private descriptor-rooted stage and must reproduce that manifest
//! exactly before the stage can replace the empty source destination.

// Archive verification and publication are intentionally descriptor-first.
// `std::fs::File` owns those authenticated descriptors without adding a
// pathname context which could imply that later path lookup remains trusted.
#![allow(clippy::disallowed_types)]

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString},
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    mem::{MaybeUninit, size_of},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use flate2::read::MultiGzDecoder;
use nix::libc;
use sha2::{Digest, Sha256};
use tar::{Archive, EntryType};
use thiserror::Error;
use xz2::{read::XzDecoder, stream::Stream as XzStream};

use crate::linux_fs::chmod_path_descriptor;

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const XZ_CONCATENATED: u32 = 0x08;

/// Stable production ceilings for one archive extraction.
///
/// Sparse files are deliberately unsupported.  Their finite extent limit is
/// therefore zero rather than an accidentally unbounded parser default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArchiveLimits {
    pub compressed_bytes: u64,
    pub decoded_bytes: u64,
    pub xz_decoder_memory_bytes: u64,
    pub zstd_window_log_max: u32,
    pub entries: u64,
    pub path_bytes: u64,
    pub path_depth: usize,
    pub one_path_bytes: usize,
    pub link_bytes: usize,
    pub extension_bytes: u64,
    pub total_extension_bytes: u64,
    pub file_logical_bytes: u64,
    pub file_physical_bytes: u64,
    pub total_logical_bytes: u64,
    pub total_physical_bytes: u64,
    pub sparse_extents: u64,
    pub materialized_nodes: u64,
    pub wall_time: Duration,
}

#[derive(Debug, Clone, Copy)]
struct ArchiveAggregateLimits {
    extractions: u64,
    compressed_bytes: u64,
    decoded_bytes: u64,
    entries: u64,
    path_bytes: u64,
    extension_bytes: u64,
    logical_bytes: u64,
    physical_bytes: u64,
    materialized_nodes: u64,
    wall_time: Duration,
}

/// One finite budget shared by every archive expanded for a derivation.
pub(crate) struct ArchiveSessionBudget {
    limits: ArchiveAggregateLimits,
    started: Instant,
    extractions: u64,
    compressed_bytes: u64,
    decoded_bytes: u64,
    entries: u64,
    path_bytes: u64,
    extension_bytes: u64,
    logical_bytes: u64,
    physical_bytes: u64,
    materialized_nodes: u64,
}

impl ArchiveSessionBudget {
    pub(crate) fn production() -> Self {
        Self::new(ArchiveAggregateLimits {
            extractions: 64,
            compressed_bytes: 8 * GIB,
            decoded_bytes: 80 * GIB,
            entries: 1_500_000,
            path_bytes: 128 * MIB,
            extension_bytes: 32 * MIB,
            logical_bytes: 64 * GIB,
            physical_bytes: 64 * GIB,
            materialized_nodes: 1_000_000,
            wall_time: Duration::from_secs(2 * 60 * 60),
        })
    }

    fn new(limits: ArchiveAggregateLimits) -> Self {
        Self {
            limits,
            started: Instant::now(),
            extractions: 0,
            compressed_bytes: 0,
            decoded_bytes: 0,
            entries: 0,
            path_bytes: 0,
            extension_bytes: 0,
            logical_bytes: 0,
            physical_bytes: 0,
            materialized_nodes: 0,
        }
    }

    fn remaining_wall_time(&self) -> Result<Duration, Error> {
        self.limits
            .wall_time
            .checked_sub(self.started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(Error::AggregateWallTimeExceeded {
                limit: self.limits.wall_time,
            })
    }

    fn admit(&mut self, compressed_bytes: u64, usage: ScanUsage) -> Result<(), Error> {
        aggregate_add(
            "derivation archive extractions",
            &mut self.extractions,
            1,
            self.limits.extractions,
        )?;
        aggregate_add(
            "derivation compressed archive bytes",
            &mut self.compressed_bytes,
            compressed_bytes,
            self.limits.compressed_bytes,
        )?;
        aggregate_add(
            "derivation decoded archive bytes",
            &mut self.decoded_bytes,
            usage.decoded_bytes,
            self.limits.decoded_bytes,
        )?;
        aggregate_add(
            "derivation archive entries",
            &mut self.entries,
            usage.entries,
            self.limits.entries,
        )?;
        aggregate_add(
            "derivation archive path bytes",
            &mut self.path_bytes,
            usage.path_bytes,
            self.limits.path_bytes,
        )?;
        aggregate_add(
            "derivation archive extension bytes",
            &mut self.extension_bytes,
            usage.extension_bytes,
            self.limits.extension_bytes,
        )?;
        aggregate_add(
            "derivation archive logical bytes",
            &mut self.logical_bytes,
            usage.logical_bytes,
            self.limits.logical_bytes,
        )?;
        aggregate_add(
            "derivation archive physical bytes",
            &mut self.physical_bytes,
            usage.physical_bytes,
            self.limits.physical_bytes,
        )?;
        aggregate_add(
            "derivation materialized archive nodes",
            &mut self.materialized_nodes,
            usage.materialized_nodes,
            self.limits.materialized_nodes,
        )?;
        self.remaining_wall_time().map(drop)
    }
}

impl ArchiveLimits {
    const fn production() -> Self {
        Self {
            compressed_bytes: 2 * GIB,
            decoded_bytes: 40 * GIB,
            xz_decoder_memory_bytes: 128 * MIB,
            zstd_window_log_max: 27,
            entries: 1_000_000,
            path_bytes: 64 * MIB,
            path_depth: 128,
            one_path_bytes: 4095,
            link_bytes: 4095,
            extension_bytes: 64 * 1024,
            total_extension_bytes: 16 * MIB,
            file_logical_bytes: 8 * GIB,
            file_physical_bytes: 8 * GIB,
            total_logical_bytes: 32 * GIB,
            total_physical_bytes: 32 * GIB,
            sparse_extents: 0,
            // Stage cleanup accounts one directory-listing operation, one
            // metadata operation, and one removal operation per node. Keep
            // this strictly below its two-million-operation boundary.
            materialized_nodes: 600_000,
            wall_time: Duration::from_secs(30 * 60),
        }
    }
}

/// Extract one locked archive from `source_dir` beneath `build_dir`.
pub(crate) fn extract_locked_tar(
    source_dir: &Path,
    source_name: &str,
    expected_sha256: &str,
    build_dir: &Path,
    destination: &str,
    strip_components: u32,
    source_date_epoch: i64,
    session: &mut ArchiveSessionBudget,
) -> Result<(), Error> {
    extract_locked_tar_with_limits(
        source_dir,
        source_name,
        expected_sha256,
        build_dir,
        destination,
        strip_components,
        source_date_epoch,
        ArchiveLimits::production(),
        session,
    )
}

#[allow(clippy::too_many_arguments)]
fn extract_locked_tar_with_limits(
    source_dir: &Path,
    source_name: &str,
    expected_sha256: &str,
    build_dir: &Path,
    destination: &str,
    strip_components: u32,
    source_date_epoch: i64,
    limits: ArchiveLimits,
    session: &mut ArchiveSessionBudget,
) -> Result<(), Error> {
    if limits.sparse_extents != 0 {
        return Err(Error::UnsupportedSparseLimit {
            found: limits.sparse_extents,
        });
    }
    let strip_components = usize::try_from(strip_components).map_err(|_| Error::StripComponentsTooLarge)?;
    let deadline = ArchiveDeadline::new(limits.wall_time.min(session.remaining_wall_time()?));
    let source_root = open_directory(source_dir, "source root")?;
    let source_components = canonical_relative_components(source_name.as_bytes(), false, u64::MAX, limits)?;
    let source = open_regular_beneath(&source_root, &source_components, "locked archive")?;
    validate_archive_file(&source, limits)?;
    let mut source = sealed_archive_snapshot(source, expected_sha256, limits.compressed_bytes, deadline)?;
    let compressed_bytes = source.metadata()?.len();

    require_archive_digest(&mut source, expected_sha256, limits.compressed_bytes, deadline)?;
    let admitted = scan_archive(&mut source, strip_components, limits, None, source_date_epoch, deadline)?;
    session.admit(compressed_bytes, admitted.usage)?;
    require_archive_digest(&mut source, expected_sha256, limits.compressed_bytes, deadline)?;

    let build_root = open_directory(build_dir, "build root")?;
    let destination_components = canonical_relative_components(destination.as_bytes(), false, u64::MAX, limits)?;
    let (parent_components, destination_name) = destination_components
        .split_last()
        .map(|(name, parent)| (parent, name))
        .ok_or(Error::EmptyDestination)?;
    let destination_parent = ensure_directories(&build_root, parent_components)?;
    let mut stage = PrivateStage::create(destination_parent)?;

    let extraction = (|| -> Result<(), Error> {
        source.seek(SeekFrom::Start(0))?;
        let extracted = scan_archive(
            &mut source,
            strip_components,
            limits,
            Some(stage.root()),
            source_date_epoch,
            deadline,
        )?;
        if extracted != admitted {
            return Err(Error::ArchiveChangedDuringExtraction);
        }
        require_archive_digest(&mut source, expected_sha256, limits.compressed_bytes, deadline)?;
        stage.publish(destination_name)?;
        normalize_destination_parents(&build_root, parent_components, source_date_epoch)
    })();
    match extraction {
        Ok(()) => Ok(()),
        Err(failure) => match stage.discard() {
            Ok(()) => Err(failure),
            Err(cleanup) => Err(Error::CleanupAfterFailure {
                failure: Box::new(failure),
                cleanup,
            }),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestEntry {
    path: Vec<Vec<u8>>,
    kind: ManifestKind,
    mode: u32,
    logical_bytes: u64,
    physical_bytes: u64,
    link_target: Option<Vec<u8>>,
    sha256: Option<[u8; 32]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScanUsage {
    decoded_bytes: u64,
    entries: u64,
    path_bytes: u64,
    extension_bytes: u64,
    logical_bytes: u64,
    physical_bytes: u64,
    materialized_nodes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScanResult {
    manifest: Vec<ManifestEntry>,
    usage: ScanUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestKind {
    Regular,
    Directory,
    Symlink,
    Hardlink,
}

#[derive(Default)]
struct PendingExtensions {
    path: Option<Vec<u8>>,
    link: Option<Vec<u8>>,
}

impl PendingExtensions {
    fn is_empty(&self) -> bool {
        self.path.is_none() && self.link.is_none()
    }
}

struct ScanBudget {
    limits: ArchiveLimits,
    entries: u64,
    path_bytes: u64,
    extension_bytes: u64,
    total_logical_bytes: u64,
    total_physical_bytes: u64,
}

#[derive(Default)]
struct MaterializationTrie {
    root: MaterializationNode,
    nodes: u64,
}

#[derive(Default)]
struct MaterializationNode {
    children: BTreeMap<Vec<u8>, MaterializationNode>,
    directory_mode: Option<u32>,
}

impl MaterializationTrie {
    fn admit(&mut self, path: &[Vec<u8>], kind: ManifestKind, mode: u32, limit: u64) -> Result<(), Error> {
        let mut current = &mut self.root;
        let mut added = 0u64;
        for (index, component) in path.iter().enumerate() {
            let vacant = !current.children.contains_key(component);
            current = current.children.entry(component.clone()).or_default();
            if vacant {
                added = added.checked_add(1).ok_or(Error::ArithmeticOverflow)?;
            }
            if index + 1 < path.len() {
                current.directory_mode.get_or_insert(0o755);
            }
        }
        if kind == ManifestKind::Directory {
            current.directory_mode = Some(mode);
        }
        self.nodes = self.nodes.checked_add(added).ok_or(Error::ArithmeticOverflow)?;
        require_limit("materialized archive nodes", self.nodes, limit)
    }
}

impl ScanBudget {
    fn new(limits: ArchiveLimits) -> Self {
        Self {
            limits,
            entries: 0,
            path_bytes: 0,
            extension_bytes: 0,
            total_logical_bytes: 0,
            total_physical_bytes: 0,
        }
    }

    fn entry(&mut self) -> Result<u64, Error> {
        self.entries = self.entries.checked_add(1).ok_or(Error::ArithmeticOverflow)?;
        require_limit("archive entries", self.entries, self.limits.entries)?;
        Ok(self.entries - 1)
    }

    fn paths(&mut self, bytes: usize) -> Result<(), Error> {
        self.path_bytes = self
            .path_bytes
            .checked_add(u64::try_from(bytes).map_err(|_| Error::ArithmeticOverflow)?)
            .ok_or(Error::ArithmeticOverflow)?;
        require_limit("archive path bytes", self.path_bytes, self.limits.path_bytes)
    }

    fn extension(&mut self, bytes: u64) -> Result<(), Error> {
        require_limit("one archive extension bytes", bytes, self.limits.extension_bytes)?;
        self.extension_bytes = self
            .extension_bytes
            .checked_add(bytes)
            .ok_or(Error::ArithmeticOverflow)?;
        require_limit(
            "aggregate archive extension bytes",
            self.extension_bytes,
            self.limits.total_extension_bytes,
        )
    }

    fn data(&mut self, logical: u64, physical: u64) -> Result<(), Error> {
        require_limit("one file logical bytes", logical, self.limits.file_logical_bytes)?;
        require_limit("one file physical bytes", physical, self.limits.file_physical_bytes)?;
        self.total_logical_bytes = self
            .total_logical_bytes
            .checked_add(logical)
            .ok_or(Error::ArithmeticOverflow)?;
        self.total_physical_bytes = self
            .total_physical_bytes
            .checked_add(physical)
            .ok_or(Error::ArithmeticOverflow)?;
        require_limit(
            "aggregate expanded logical bytes",
            self.total_logical_bytes,
            self.limits.total_logical_bytes,
        )?;
        require_limit(
            "aggregate expanded physical bytes",
            self.total_physical_bytes,
            self.limits.total_physical_bytes,
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct ArchiveDeadline {
    started: Instant,
    limit: Duration,
}

impl ArchiveDeadline {
    fn new(limit: Duration) -> Self {
        Self {
            started: Instant::now(),
            limit,
        }
    }

    fn checkpoint(self) -> Result<(), Error> {
        if self.started.elapsed() < self.limit {
            Ok(())
        } else {
            Err(Error::WallTimeExceeded { limit: self.limit })
        }
    }

    fn checkpoint_io(self) -> io::Result<()> {
        self.checkpoint().map_err(io::Error::other)
    }
}

include!("archive/scanning.rs");
include!("archive/materialization.rs");
include!("archive/private_stage.rs");

#[derive(Debug, Error)]
pub enum Error {
    #[error("archive resource limit exceeded for {resource}: found {actual}, limit {limit}")]
    LimitExceeded {
        resource: &'static str,
        actual: u64,
        limit: u64,
    },
    #[error("archive extraction exceeded its wall-time limit of {limit:?}")]
    WallTimeExceeded { limit: Duration },
    #[error("all archive extraction for the derivation exceeded its wall-time limit of {limit:?}")]
    AggregateWallTimeExceeded { limit: Duration },
    #[error("archive sparse entries are unsupported (entry {entry})")]
    SparseEntry { entry: u64 },
    #[error("non-zero sparse extent limit {found} is unsupported")]
    UnsupportedSparseLimit { found: u64 },
    #[error("archive global PAX state is unsupported (entry {entry})")]
    GlobalPaxEntry { entry: u64 },
    #[error("archive entry {entry} repeats extension field {field}")]
    DuplicateExtension { entry: u64, field: &'static str },
    #[error("archive entry {entry} contains a duplicate PAX key")]
    DuplicatePaxKey { entry: u64 },
    #[error("archive entry {entry} contains an unsupported PAX key")]
    UnsupportedPaxKey { entry: u64 },
    #[error("archive entry {entry} contains malformed PAX data")]
    InvalidPaxEntry { entry: u64 },
    #[error("archive entry {entry} has an invalid extension value")]
    InvalidExtensionValue { entry: u64 },
    #[error("archive extension is not followed by an entry")]
    DanglingExtension,
    #[error("archive entry {entry} has an unsafe path")]
    UnsafePath { entry: u64 },
    #[error("archive entry {entry} has an escaping symlink target")]
    EscapingSymlink { entry: u64 },
    #[error("archive entry {entry} has no link target")]
    MissingLinkTarget { entry: u64 },
    #[error("archive entry {entry} unexpectedly has a link target")]
    UnexpectedLinkTarget { entry: u64 },
    #[error("archive entry {entry} hardlink target is removed by strip-components")]
    StrippedHardlinkTarget { entry: u64 },
    #[error("archive entry {entry} uses a forward hardlink")]
    ForwardHardlink { entry: u64 },
    #[error("archive entry {entry} hardlink target is not a regular file")]
    InvalidHardlinkTargetType { entry: u64 },
    #[error("archive entry {entry} uses an unsupported inode type")]
    UnsupportedInodeType { entry: u64 },
    #[error("archive entry {entry} carries data for a non-regular inode")]
    DataOnNonRegular { entry: u64 },
    #[error("archive entry {entry} duplicates an extracted path")]
    DuplicatePath { entry: u64 },
    #[error("archive entry {entry} collides with an incompatible parent or child path")]
    PathTypeCollision { entry: u64 },
    #[error("archive entry {entry} yielded {found} bytes, expected {expected}")]
    EntrySizeMismatch { entry: u64, expected: u64, found: u64 },
    #[error("archive entry stream exceeded its declared size")]
    EntryStreamExceededDeclaredSize,
    #[error("archive compression or container format is unsupported")]
    UnsupportedArchiveCompression,
    #[error("locked archive is not a regular file")]
    ArchiveNotRegular,
    #[error("locked archive digest does not match the frozen source")]
    ArchiveDigestMismatch,
    #[error("archive snapshot seals are incomplete: expected mask {expected:#x}, found {found:#x}")]
    ArchiveSnapshotNotSealed { expected: i32, found: i32 },
    #[error("archive changed between preflight and extraction")]
    ArchiveChangedDuringExtraction,
    #[error("{failure}; bounded archive-stage cleanup also failed: {cleanup}")]
    CleanupAfterFailure { failure: Box<Error>, cleanup: io::Error },
    #[error("archive strip-components does not fit the executor")]
    StripComponentsTooLarge,
    #[error("archive destination is empty")]
    EmptyDestination,
    #[error("archive arithmetic overflow")]
    ArithmeticOverflow,
    #[error("archive path contains an interior NUL")]
    InteriorNul,
    #[error("archive extractor constructed an unsafe internal path")]
    UnsafeInternalPath,
    #[error("could not allocate a unique private archive stage")]
    StageNameExhausted,
    #[error("archive destination is not an empty directory: {source}")]
    DestinationNotEmpty { source: io::Error },
    #[error("{operation}: {source}")]
    DescriptorOperation { operation: &'static str, source: io::Error },
    #[error("{operation} is not a directory")]
    NotDirectory { operation: &'static str },
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests;
