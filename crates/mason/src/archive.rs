// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

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

fn scan_archive(
    source: &mut File,
    strip_components: usize,
    limits: ArchiveLimits,
    extraction_root: Option<&File>,
    source_date_epoch: i64,
    deadline: ArchiveDeadline,
) -> Result<ScanResult, Error> {
    deadline.checkpoint()?;
    source.seek(SeekFrom::Start(0))?;
    let decoded = decoded_reader(source, limits, deadline)?;
    let mut archive = Archive::new(decoded);
    archive.set_ignore_zeros(false);
    let mut entries = archive.entries()?.raw(true);
    let mut budget = ScanBudget::new(limits);
    let mut pending = PendingExtensions::default();
    let mut manifest = Vec::new();
    let mut topology = BTreeMap::<Vec<Vec<u8>>, ManifestKind>::new();
    let mut materialization = MaterializationTrie::default();

    while let Some(entry) = entries.next() {
        deadline.checkpoint()?;
        let mut entry = entry?;
        let entry_index = budget.entry()?;
        let entry_type = entry.header().entry_type();

        if entry_type.is_gnu_sparse() {
            return Err(Error::SparseEntry { entry: entry_index });
        }
        if entry_type.is_pax_global_extensions() {
            return Err(Error::GlobalPaxEntry { entry: entry_index });
        }
        if entry_type.is_gnu_longname() {
            if pending.path.is_some() {
                return Err(Error::DuplicateExtension {
                    entry: entry_index,
                    field: "path",
                });
            }
            pending.path = Some(read_extension_bytes(&mut entry, entry_index, &mut budget)?);
            continue;
        }
        if entry_type.is_gnu_longlink() {
            if pending.link.is_some() {
                return Err(Error::DuplicateExtension {
                    entry: entry_index,
                    field: "linkpath",
                });
            }
            pending.link = Some(read_extension_bytes(&mut entry, entry_index, &mut budget)?);
            continue;
        }
        if entry_type.is_pax_local_extensions() {
            read_pax_extensions(&mut entry, entry_index, &mut budget, &mut pending)?;
            continue;
        }

        let raw_path = pending.path.take().unwrap_or_else(|| entry.path_bytes().into_owned());
        let raw_link = pending
            .link
            .take()
            .or_else(|| entry.link_name_bytes().map(|value| value.into_owned()));
        budget.paths(raw_path.len())?;

        let kind = classify_entry(entry_type, entry_index)?;
        let original = canonical_relative_components(&raw_path, kind == ManifestKind::Directory, entry_index, limits)?;
        let path = original.get(strip_components..).unwrap_or(&[]).to_vec();
        let mode = entry.header().mode()? & 0o777;
        let logical_bytes = entry.size();
        let physical_bytes = entry.size();
        budget.data(logical_bytes, physical_bytes)?;

        let link_target = match kind {
            ManifestKind::Symlink => {
                if logical_bytes != 0 {
                    return Err(Error::DataOnNonRegular { entry: entry_index });
                }
                let raw_link = require_link(raw_link, entry_index)?;
                budget.paths(raw_link.len())?;
                validate_symlink_target(&original, &raw_link, entry_index, limits)?;
                if !path.is_empty() {
                    validate_symlink_target(&path, &raw_link, entry_index, limits)?;
                }
                Some(raw_link)
            }
            ManifestKind::Hardlink => {
                if logical_bytes != 0 {
                    return Err(Error::DataOnNonRegular { entry: entry_index });
                }
                let raw_link = require_link(raw_link, entry_index)?;
                budget.paths(raw_link.len())?;
                let original_target = canonical_relative_components(&raw_link, false, entry_index, limits)?;
                let target = original_target.get(strip_components..).unwrap_or(&[]).to_vec();
                if !path.is_empty() && target.is_empty() {
                    return Err(Error::StrippedHardlinkTarget { entry: entry_index });
                }
                Some(join_components(&target))
            }
            ManifestKind::Regular | ManifestKind::Directory => {
                if raw_link.is_some() {
                    return Err(Error::UnexpectedLinkTarget { entry: entry_index });
                }
                if kind == ManifestKind::Directory && logical_bytes != 0 {
                    return Err(Error::DataOnNonRegular { entry: entry_index });
                }
                None
            }
        };

        let hardlink_target = if path.is_empty() || kind != ManifestKind::Hardlink {
            None
        } else {
            Some(split_joined_components(link_target.as_deref().unwrap_or_default()))
        };
        if !path.is_empty() {
            validate_topology(&topology, &path, kind, entry_index)?;
            materialization.admit(&path, kind, mode, limits.materialized_nodes)?;
            if let Some(target) = &hardlink_target {
                let Some(target_kind) = topology.get(target) else {
                    return Err(Error::ForwardHardlink { entry: entry_index });
                };
                if !matches!(target_kind, ManifestKind::Regular | ManifestKind::Hardlink) {
                    return Err(Error::InvalidHardlinkTargetType { entry: entry_index });
                }
            }
        }

        let mut digest = None;
        if kind == ManifestKind::Regular {
            let mut hasher = Sha256::new();
            let mut writer = extraction_root
                .filter(|_| !path.is_empty())
                .map(|root| create_regular_beneath(root, &path, source_date_epoch))
                .transpose()?;
            let copied = copy_entry(&mut entry, writer.as_mut(), &mut hasher, logical_bytes, deadline)?;
            if copied != logical_bytes {
                return Err(Error::EntrySizeMismatch {
                    entry: entry_index,
                    expected: logical_bytes,
                    found: copied,
                });
            }
            if let Some(file) = writer {
                set_file_mode_and_time(&file, mode, source_date_epoch)?;
                file.sync_all()?;
            }
            digest = Some(hasher.finalize().into());
        } else {
            ensure_entry_consumed(&mut entry, entry_index)?;
        }

        if path.is_empty() {
            continue;
        }
        if kind == ManifestKind::Hardlink {
            let target = hardlink_target.as_deref().expect("validated hardlink target");
            if let Some(root) = extraction_root {
                create_hardlink_beneath(root, target, &path)?;
            }
        } else if kind == ManifestKind::Symlink {
            if let Some(root) = extraction_root {
                create_symlink_beneath(
                    root,
                    link_target.as_deref().unwrap_or_default(),
                    &path,
                    source_date_epoch,
                )?;
            }
        } else if kind == ManifestKind::Directory {
            if let Some(root) = extraction_root {
                ensure_directories(root, &path)?;
            }
        }
        topology.insert(path.clone(), kind);
        manifest.push(ManifestEntry {
            path,
            kind,
            mode,
            logical_bytes,
            physical_bytes,
            link_target,
            sha256: digest,
        });
    }

    if !pending.is_empty() {
        return Err(Error::DanglingExtension);
    }
    drop(entries);
    let mut decoded = archive.into_inner();
    io::copy(&mut decoded, &mut io::sink())?;
    let decoded_bytes = decoded.consumed;

    if let Some(root) = extraction_root {
        normalize_materialized_directories(
            root,
            &materialization.root,
            &mut Vec::new(),
            source_date_epoch,
            deadline,
        )?;
        set_file_mode_and_time(root, 0o755, source_date_epoch)?;
        root.sync_all()?;
    }
    Ok(ScanResult {
        manifest,
        usage: ScanUsage {
            decoded_bytes,
            entries: budget.entries,
            path_bytes: budget.path_bytes,
            extension_bytes: budget.extension_bytes,
            logical_bytes: budget.total_logical_bytes,
            physical_bytes: budget.total_physical_bytes,
            materialized_nodes: materialization.nodes,
        },
    })
}

fn normalize_materialized_directories(
    root: &File,
    node: &MaterializationNode,
    path: &mut Vec<Vec<u8>>,
    source_date_epoch: i64,
    deadline: ArchiveDeadline,
) -> Result<(), Error> {
    for (component, child) in &node.children {
        deadline.checkpoint()?;
        path.push(component.clone());
        normalize_materialized_directories(root, child, path, source_date_epoch, deadline)?;
        if let Some(mode) = child.directory_mode {
            let directory = open_directory_beneath(root, path, "extracted directory")?;
            set_file_mode_and_time(&directory, mode, source_date_epoch)?;
            directory.sync_all()?;
        }
        path.pop();
    }
    Ok(())
}

fn decoded_reader<'a>(
    source: &'a mut File,
    limits: ArchiveLimits,
    deadline: ArchiveDeadline,
) -> Result<DecodedLimit<Box<dyn Read + 'a>>, Error> {
    deadline.checkpoint()?;
    let mut magic = [0u8; 8];
    let found = source.read(&mut magic)?;
    source.seek(SeekFrom::Start(0))?;
    let input = source.take(limits.compressed_bytes.saturating_add(1));
    let decoded: Box<dyn Read + 'a> = if magic[..found].starts_with(&[0x1f, 0x8b]) {
        Box::new(MultiGzDecoder::new(input))
    } else if magic[..found].starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        let stream = XzStream::new_stream_decoder(limits.xz_decoder_memory_bytes, XZ_CONCATENATED)
            .map_err(|source| io::Error::new(io::ErrorKind::InvalidInput, source))?;
        Box::new(XzDecoder::new_stream(input, stream))
    } else if magic[..found].starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        let mut decoder = zstd::stream::read::Decoder::new(input)?;
        decoder.window_log_max(limits.zstd_window_log_max)?;
        Box::new(decoder)
    } else if unsupported_compression_magic(&magic[..found]) {
        return Err(Error::UnsupportedArchiveCompression);
    } else {
        Box::new(input)
    };
    Ok(DecodedLimit::new(decoded, limits.decoded_bytes, deadline))
}

fn unsupported_compression_magic(magic: &[u8]) -> bool {
    magic.starts_with(b"BZh")
        || magic.starts_with(b"PK\x03\x04")
        || magic.starts_with(b"7z\xbc\xaf\x27\x1c")
        || magic.starts_with(b"Rar!\x1a\x07")
        || magic.starts_with(b"LZIP")
        || magic.starts_with(&[0x1f, 0x9d])
}

struct DecodedLimit<R> {
    inner: R,
    remaining: u64,
    consumed: u64,
    exhausted: bool,
    deadline: ArchiveDeadline,
}

impl<R> DecodedLimit<R> {
    fn new(inner: R, limit: u64, deadline: ArchiveDeadline) -> Self {
        Self {
            inner,
            remaining: limit,
            consumed: 0,
            exhausted: false,
            deadline,
        }
    }
}

impl<R: Read> Read for DecodedLimit<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.deadline.checkpoint_io()?;
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            if self.exhausted {
                return Ok(0);
            }
            let mut probe = [0u8; 1];
            let found = self.inner.read(&mut probe)?;
            self.deadline.checkpoint_io()?;
            if found == 0 {
                self.exhausted = true;
                return Ok(0);
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "decoded archive byte limit exceeded",
            ));
        }
        let allowed = usize::try_from(self.remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let found = self.inner.read(&mut buffer[..allowed])?;
        self.deadline.checkpoint_io()?;
        self.remaining -= found as u64;
        self.consumed = self
            .consumed
            .checked_add(found as u64)
            .ok_or_else(|| io::Error::other("decoded archive byte counter overflowed"))?;
        Ok(found)
    }
}

fn read_extension_bytes<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    entry_index: u64,
    budget: &mut ScanBudget,
) -> Result<Vec<u8>, Error> {
    budget.extension(entry.size())?;
    let mut value = Vec::with_capacity(usize::try_from(entry.size()).map_err(|_| Error::ArithmeticOverflow)?);
    entry.read_to_end(&mut value)?;
    if value.last() == Some(&0) {
        value.pop();
    }
    if value.is_empty() || value.contains(&0) {
        return Err(Error::InvalidExtensionValue { entry: entry_index });
    }
    Ok(value)
}

fn read_pax_extensions<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    entry_index: u64,
    budget: &mut ScanBudget,
    pending: &mut PendingExtensions,
) -> Result<(), Error> {
    budget.extension(entry.size())?;
    let extensions = entry
        .pax_extensions()?
        .ok_or(Error::InvalidPaxEntry { entry: entry_index })?;
    let mut keys = BTreeSet::new();
    for extension in extensions {
        let extension = extension?;
        let key = extension.key_bytes();
        if !keys.insert(key.to_vec()) {
            return Err(Error::DuplicatePaxKey { entry: entry_index });
        }
        if key.starts_with(b"GNU.sparse.") {
            return Err(Error::SparseEntry { entry: entry_index });
        }
        match key {
            b"path" => {
                if pending.path.replace(extension.value_bytes().to_vec()).is_some() {
                    return Err(Error::DuplicateExtension {
                        entry: entry_index,
                        field: "path",
                    });
                }
            }
            b"linkpath" => {
                if pending.link.replace(extension.value_bytes().to_vec()).is_some() {
                    return Err(Error::DuplicateExtension {
                        entry: entry_index,
                        field: "linkpath",
                    });
                }
            }
            // These fields do not affect the extracted byte graph.  Ownership,
            // archive timestamps, and comments are intentionally discarded.
            b"uid" | b"gid" | b"uname" | b"gname" | b"mtime" | b"atime" | b"ctime" | b"charset" | b"comment" => {}
            _ => return Err(Error::UnsupportedPaxKey { entry: entry_index }),
        }
    }
    Ok(())
}

fn classify_entry(entry_type: EntryType, entry: u64) -> Result<ManifestKind, Error> {
    if entry_type.is_file() {
        Ok(ManifestKind::Regular)
    } else if entry_type.is_dir() {
        Ok(ManifestKind::Directory)
    } else if entry_type.is_symlink() {
        Ok(ManifestKind::Symlink)
    } else if entry_type.is_hard_link() {
        Ok(ManifestKind::Hardlink)
    } else {
        Err(Error::UnsupportedInodeType { entry })
    }
}

fn canonical_relative_components(
    path: &[u8],
    allow_trailing_slash: bool,
    entry: u64,
    limits: ArchiveLimits,
) -> Result<Vec<Vec<u8>>, Error> {
    if path.is_empty() || path[0] == b'/' || path.contains(&0) || path.contains(&b'\\') {
        return Err(Error::UnsafePath { entry });
    }
    require_usize_limit("one archive path bytes", path.len(), limits.one_path_bytes)?;
    let mut raw = path.split(|byte| *byte == b'/').collect::<Vec<_>>();
    if allow_trailing_slash && raw.last().is_some_and(|component| component.is_empty()) {
        raw.pop();
    }
    if raw.is_empty()
        || raw
            .iter()
            .any(|component| component.is_empty() || *component == b"." || *component == b"..")
    {
        return Err(Error::UnsafePath { entry });
    }
    require_usize_limit("archive path depth", raw.len(), limits.path_depth)?;
    Ok(raw.into_iter().map(<[u8]>::to_vec).collect())
}

fn validate_symlink_target(
    link_path: &[Vec<u8>],
    target: &[u8],
    entry: u64,
    limits: ArchiveLimits,
) -> Result<(), Error> {
    if target.is_empty() || target[0] == b'/' || target.contains(&0) || target.contains(&b'\\') {
        return Err(Error::EscapingSymlink { entry });
    }
    require_usize_limit("archive link bytes", target.len(), limits.link_bytes)?;
    let mut resolved = link_path[..link_path.len().saturating_sub(1)].to_vec();
    for component in target.split(|byte| *byte == b'/') {
        match component {
            b"" => return Err(Error::EscapingSymlink { entry }),
            b"." => {}
            b".." => {
                if resolved.pop().is_none() {
                    return Err(Error::EscapingSymlink { entry });
                }
            }
            value => resolved.push(value.to_vec()),
        }
        require_usize_limit("archive link depth", resolved.len(), limits.path_depth)?;
    }
    Ok(())
}

fn require_link(link: Option<Vec<u8>>, entry: u64) -> Result<Vec<u8>, Error> {
    link.filter(|value| !value.is_empty())
        .ok_or(Error::MissingLinkTarget { entry })
}

fn validate_topology(
    topology: &BTreeMap<Vec<Vec<u8>>, ManifestKind>,
    path: &[Vec<u8>],
    kind: ManifestKind,
    entry: u64,
) -> Result<(), Error> {
    if topology.contains_key(path) {
        return Err(Error::DuplicatePath { entry });
    }
    for depth in 1..path.len() {
        if let Some(parent_kind) = topology.get(&path[..depth])
            && *parent_kind != ManifestKind::Directory
        {
            return Err(Error::PathTypeCollision { entry });
        }
    }
    if kind != ManifestKind::Directory {
        let next = topology
            .range((std::ops::Bound::Excluded(path.to_vec()), std::ops::Bound::Unbounded))
            .next()
            .map(|(path, _)| path);
        if next.is_some_and(|other| other.len() > path.len() && other.starts_with(path)) {
            return Err(Error::PathTypeCollision { entry });
        }
    }
    Ok(())
}

fn copy_entry<R: Read>(
    entry: &mut R,
    mut output: Option<&mut File>,
    hasher: &mut Sha256,
    limit: u64,
    deadline: ArchiveDeadline,
) -> Result<u64, Error> {
    let mut total = 0u64;
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    loop {
        deadline.checkpoint()?;
        let found = entry.read(&mut buffer)?;
        deadline.checkpoint()?;
        if found == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(found).map_err(|_| Error::ArithmeticOverflow)?)
            .ok_or(Error::ArithmeticOverflow)?;
        if total > limit {
            return Err(Error::EntryStreamExceededDeclaredSize);
        }
        hasher.update(&buffer[..found]);
        if let Some(output) = output.as_deref_mut() {
            output.write_all(&buffer[..found])?;
        }
    }
    Ok(total)
}

fn ensure_entry_consumed<R: Read>(entry: &mut R, index: u64) -> Result<(), Error> {
    let mut buffer = [0u8; 1];
    if entry.read(&mut buffer)? == 0 {
        Ok(())
    } else {
        Err(Error::DataOnNonRegular { entry: index })
    }
}

fn require_archive_digest(
    source: &mut File,
    expected: &str,
    limit: u64,
    deadline: ArchiveDeadline,
) -> Result<(), Error> {
    source.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    loop {
        deadline.checkpoint()?;
        let found = source.read(&mut buffer)?;
        deadline.checkpoint()?;
        if found == 0 {
            break;
        }
        total = total.checked_add(found as u64).ok_or(Error::ArithmeticOverflow)?;
        require_limit("compressed archive bytes", total, limit)?;
        hasher.update(&buffer[..found]);
    }
    let found = hex::encode(hasher.finalize());
    if found == expected {
        source.seek(SeekFrom::Start(0))?;
        Ok(())
    } else {
        Err(Error::ArchiveDigestMismatch)
    }
}

fn sealed_archive_snapshot(
    mut source: File,
    expected: &str,
    limit: u64,
    deadline: ArchiveDeadline,
) -> Result<File, Error> {
    let descriptor = unsafe {
        libc::memfd_create(
            c"cast-locked-archive".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if descriptor == -1 {
        return Err(Error::DescriptorOperation {
            operation: "create sealed archive snapshot",
            source: io::Error::last_os_error(),
        });
    }
    let mut snapshot = unsafe { File::from_raw_fd(descriptor) };
    source.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    loop {
        deadline.checkpoint()?;
        let found = source.read(&mut buffer)?;
        deadline.checkpoint()?;
        if found == 0 {
            break;
        }
        total = total.checked_add(found as u64).ok_or(Error::ArithmeticOverflow)?;
        require_limit("compressed archive bytes", total, limit)?;
        snapshot.write_all(&buffer[..found])?;
        hasher.update(&buffer[..found]);
    }
    if hex::encode(hasher.finalize()) != expected {
        return Err(Error::ArchiveDigestMismatch);
    }
    if unsafe { libc::fchmod(snapshot.as_raw_fd(), 0o400) } == -1 {
        return Err(io::Error::last_os_error().into());
    }
    let required_seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if unsafe { libc::fcntl(snapshot.as_raw_fd(), libc::F_ADD_SEALS, required_seals) } == -1 {
        return Err(Error::DescriptorOperation {
            operation: "seal immutable archive snapshot",
            source: io::Error::last_os_error(),
        });
    }
    let found_seals = unsafe { libc::fcntl(snapshot.as_raw_fd(), libc::F_GET_SEALS) };
    if found_seals == -1 {
        return Err(Error::DescriptorOperation {
            operation: "verify immutable archive snapshot seals",
            source: io::Error::last_os_error(),
        });
    }
    if found_seals & required_seals != required_seals {
        return Err(Error::ArchiveSnapshotNotSealed {
            expected: required_seals,
            found: found_seals,
        });
    }
    snapshot.seek(SeekFrom::Start(0))?;
    Ok(snapshot)
}

fn validate_archive_file(file: &File, limits: ArchiveLimits) -> Result<(), Error> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(Error::ArchiveNotRegular);
    }
    require_limit("compressed archive bytes", metadata.len(), limits.compressed_bytes)
}

fn set_file_mode_and_time(file: &File, mode: u32, source_date_epoch: i64) -> Result<(), Error> {
    let result = unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) };
    if result == -1 {
        return Err(io::Error::last_os_error().into());
    }
    let timestamp = filetime::FileTime::from_unix_time(source_date_epoch, 0);
    filetime::set_file_handle_times(file, Some(timestamp), Some(timestamp))?;
    Ok(())
}

fn open_directory(path: &Path, operation: &'static str) -> Result<File, Error> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| Error::DescriptorOperation { operation, source })?;
    validate_directory(&file, operation)?;
    Ok(file)
}

fn validate_directory(file: &File, operation: &'static str) -> Result<(), Error> {
    if file.metadata()?.file_type().is_dir() {
        Ok(())
    } else {
        Err(Error::NotDirectory { operation })
    }
}

fn open_regular_beneath(root: &File, components: &[Vec<u8>], operation: &'static str) -> Result<File, Error> {
    let relative = cstring_path(components)?;
    let fd = openat2(
        root.as_raw_fd(),
        &relative,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )
    .map_err(|source| Error::DescriptorOperation { operation, source })?;
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.file_type().is_file() {
        return Err(Error::ArchiveNotRegular);
    }
    Ok(file)
}

fn open_directory_beneath(root: &File, components: &[Vec<u8>], operation: &'static str) -> Result<File, Error> {
    if components.is_empty() {
        return root.try_clone().map_err(Error::from);
    }
    let relative = cstring_path(components)?;
    let fd = openat2(
        root.as_raw_fd(),
        &relative,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )
    .map_err(|source| Error::DescriptorOperation { operation, source })?;
    let file = unsafe { File::from_raw_fd(fd) };
    validate_directory(&file, operation)?;
    Ok(file)
}

fn ensure_directories(root: &File, components: &[Vec<u8>]) -> Result<File, Error> {
    let mut current = root.try_clone()?;
    for component in components {
        let component = CString::new(component.as_slice()).map_err(|_| Error::InteriorNul)?;
        let result = unsafe { libc::mkdirat(current.as_raw_fd(), component.as_ptr(), 0o700) };
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::AlreadyExists {
                return Err(Error::DescriptorOperation {
                    operation: "create archive directory",
                    source,
                });
            }
        }
        let path = openat2(
            current.as_raw_fd(),
            &component,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        )
        .map_err(|source| Error::DescriptorOperation {
            operation: "pin archive directory",
            source,
        })?;
        let path = unsafe { File::from_raw_fd(path) };
        validate_directory(&path, "pin archive directory")?;
        chmod_path_descriptor(&path, 0o700)?;
        let fd = openat2(
            current.as_raw_fd(),
            &component,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )
        .map_err(|source| Error::DescriptorOperation {
            operation: "open archive directory",
            source,
        })?;
        current = unsafe { File::from_raw_fd(fd) };
        validate_directory(&current, "open archive directory")?;
    }
    Ok(current)
}

fn normalize_destination_parents(root: &File, components: &[Vec<u8>], source_date_epoch: i64) -> Result<(), Error> {
    for depth in (1..=components.len()).rev() {
        let directory = open_directory_beneath(root, &components[..depth], "archive destination parent")?;
        set_file_mode_and_time(&directory, 0o755, source_date_epoch)?;
        directory.sync_all()?;
    }
    Ok(())
}

fn chmod_path_descriptor(file: &File, mode: u32) -> Result<(), Error> {
    let result = unsafe {
        libc::syscall(
            libc::SYS_fchmodat2,
            file.as_raw_fd(),
            c"".as_ptr(),
            mode,
            libc::AT_EMPTY_PATH,
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}

fn create_regular_beneath(root: &File, path: &[Vec<u8>], source_date_epoch: i64) -> Result<File, Error> {
    inject_test_stage_write_failure()?;
    let (name, parents) = path.split_last().ok_or(Error::UnsafeInternalPath)?;
    let parent = ensure_directories(root, parents)?;
    let name = CString::new(name.as_slice()).map_err(|_| Error::InteriorNul)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd == -1 {
        return Err(Error::DescriptorOperation {
            operation: "create extracted regular file",
            source: io::Error::last_os_error(),
        });
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let timestamp = filetime::FileTime::from_unix_time(source_date_epoch, 0);
    filetime::set_file_handle_times(&file, Some(timestamp), Some(timestamp))?;
    Ok(file)
}

#[cfg(test)]
std::thread_local! {
    static TEST_STAGE_WRITES_BEFORE_FAILURE: std::cell::Cell<u64> = const { std::cell::Cell::new(u64::MAX) };
    static TEST_FAIL_STAGE_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TEST_FAIL_PUBLISH_AFTER_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn inject_test_stage_write_failure() -> Result<(), Error> {
    TEST_STAGE_WRITES_BEFORE_FAILURE.with(|remaining| {
        let value = remaining.get();
        if value == 0 {
            Err(io::Error::other("injected archive stage write failure").into())
        } else {
            remaining.set(value - 1);
            Ok(())
        }
    })
}

#[cfg(test)]
fn inject_test_stage_open_failure() -> Result<(), Error> {
    TEST_FAIL_STAGE_OPEN.with(|fail| {
        if fail.replace(false) {
            Err(io::Error::other("injected private archive stage open failure").into())
        } else {
            Ok(())
        }
    })
}

#[cfg(test)]
fn inject_test_publish_failure_after_rename() -> Result<(), Error> {
    TEST_FAIL_PUBLISH_AFTER_RENAME.with(|fail| {
        if fail.replace(false) {
            Err(io::Error::other("injected archive publication durability failure").into())
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
fn inject_test_stage_write_failure() -> Result<(), Error> {
    Ok(())
}

#[cfg(not(test))]
fn inject_test_stage_open_failure() -> Result<(), Error> {
    Ok(())
}

#[cfg(not(test))]
fn inject_test_publish_failure_after_rename() -> Result<(), Error> {
    Ok(())
}

fn create_symlink_beneath(root: &File, target: &[u8], path: &[Vec<u8>], source_date_epoch: i64) -> Result<(), Error> {
    let (name, parents) = path.split_last().ok_or(Error::UnsafeInternalPath)?;
    let parent = ensure_directories(root, parents)?;
    let name = CString::new(name.as_slice()).map_err(|_| Error::InteriorNul)?;
    let target = CString::new(target).map_err(|_| Error::InteriorNul)?;
    let result = unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), name.as_ptr()) };
    if result == -1 {
        return Err(Error::DescriptorOperation {
            operation: "create extracted symlink",
            source: io::Error::last_os_error(),
        });
    }
    let timestamp = libc::timespec {
        tv_sec: source_date_epoch as libc::time_t,
        tv_nsec: 0,
    };
    let times = [timestamp, timestamp];
    let result = unsafe {
        libc::utimensat(
            parent.as_raw_fd(),
            name.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == -1 {
        Err(Error::DescriptorOperation {
            operation: "normalize extracted symlink timestamp",
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

fn create_hardlink_beneath(root: &File, target: &[Vec<u8>], path: &[Vec<u8>]) -> Result<(), Error> {
    let (target_name, target_parents) = target.split_last().ok_or(Error::UnsafeInternalPath)?;
    let target_parent = open_directory_beneath(root, target_parents, "open hardlink target parent")?;
    let (name, parents) = path.split_last().ok_or(Error::UnsafeInternalPath)?;
    let parent = ensure_directories(root, parents)?;
    let target_name = CString::new(target_name.as_slice()).map_err(|_| Error::InteriorNul)?;
    let name = CString::new(name.as_slice()).map_err(|_| Error::InteriorNul)?;
    let result = unsafe {
        libc::linkat(
            target_parent.as_raw_fd(),
            target_name.as_ptr(),
            parent.as_raw_fd(),
            name.as_ptr(),
            0,
        )
    };
    if result == -1 {
        Err(Error::DescriptorOperation {
            operation: "create extracted hardlink",
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

fn cstring_path(components: &[Vec<u8>]) -> Result<CString, Error> {
    CString::new(join_components(components)).map_err(|_| Error::InteriorNul)
}

fn join_components(components: &[Vec<u8>]) -> Vec<u8> {
    let length = components.iter().map(Vec::len).sum::<usize>() + components.len().saturating_sub(1);
    let mut joined = Vec::with_capacity(length);
    for (index, component) in components.iter().enumerate() {
        if index != 0 {
            joined.push(b'/');
        }
        joined.extend_from_slice(component);
    }
    joined
}

fn split_joined_components(path: &[u8]) -> Vec<Vec<u8>> {
    path.split(|byte| *byte == b'/').map(<[u8]>::to_vec).collect()
}

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

const RESOLVE_NO_XDEV: u64 = 0x01;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;

fn openat2(parent: RawFd, path: &CStr, flags: i32, mode: libc::mode_t) -> io::Result<RawFd> {
    let how = OpenHow {
        flags: flags as u64,
        mode: mode as u64,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV,
    };
    let result = unsafe { libc::syscall(libc::SYS_openat2, parent, path.as_ptr(), &how, size_of::<OpenHow>()) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result as RawFd)
    }
}

struct PrivateStage {
    parent: File,
    root: File,
    name: CString,
    active: bool,
}

impl PrivateStage {
    fn create(parent: File) -> Result<Self, Error> {
        static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let sequence = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
            let name = CString::new(format!(".cast-archive-stage-{}-{sequence}", std::process::id()))
                .expect("stage name has no NUL");
            let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
            if result == -1 {
                let source = io::Error::last_os_error();
                if source.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(Error::DescriptorOperation {
                    operation: "create private archive stage",
                    source,
                });
            }
            let opened = (|| -> Result<File, Error> {
                inject_test_stage_open_failure()?;
                let path = openat2(
                    parent.as_raw_fd(),
                    &name,
                    libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    0,
                )
                .map_err(|source| Error::DescriptorOperation {
                    operation: "pin private archive stage",
                    source,
                })?;
                let path = unsafe { File::from_raw_fd(path) };
                validate_directory(&path, "pin private archive stage")?;
                chmod_path_descriptor(&path, 0o700)?;
                let fd = openat2(
                    parent.as_raw_fd(),
                    &name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                    0,
                )
                .map_err(|source| Error::DescriptorOperation {
                    operation: "open private archive stage",
                    source,
                })?;
                let root = unsafe { File::from_raw_fd(fd) };
                validate_directory(&root, "open private archive stage")?;
                Ok(root)
            })();
            let root = match opened {
                Ok(root) => root,
                Err(failure) => {
                    let cleanup = remove_empty_stage(&parent, &name);
                    return match cleanup {
                        Ok(()) => Err(failure),
                        Err(cleanup) => Err(Error::CleanupAfterFailure {
                            failure: Box::new(failure),
                            cleanup,
                        }),
                    };
                }
            };
            return Ok(Self {
                parent,
                root,
                name,
                active: true,
            });
        }
        Err(Error::StageNameExhausted)
    }

    fn root(&self) -> &File {
        &self.root
    }

    fn publish(&mut self, destination: &[u8]) -> Result<(), Error> {
        self.root.sync_all()?;
        let destination = CString::new(destination).map_err(|_| Error::InteriorNul)?;
        let removed = unsafe { libc::unlinkat(self.parent.as_raw_fd(), destination.as_ptr(), libc::AT_REMOVEDIR) };
        if removed == -1 {
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::NotFound {
                return Err(Error::DestinationNotEmpty { source });
            }
        }
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                self.parent.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == -1 {
            return Err(Error::DescriptorOperation {
                operation: "publish verified archive stage",
                source: io::Error::last_os_error(),
            });
        }
        // The rename is the irreversible publication point. A later durability
        // error must not run stage-name cleanup against a name which no longer
        // owns this descriptor or destructively empty the published tree.
        self.active = false;
        inject_test_publish_failure_after_rename()?;
        self.parent.sync_all()?;
        Ok(())
    }

    fn discard(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        let mut budget = StageCleanupBudget::new(self.root.metadata()?.dev());
        purge_directory_contents(&self.root, &mut budget, 0)?;
        budget.operation()?;
        let result = unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), libc::AT_REMOVEDIR) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        self.parent.sync_all()?;
        self.active = false;
        Ok(())
    }
}

fn remove_empty_stage(parent: &File, name: &CStr) -> io::Result<()> {
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl Drop for PrivateStage {
    fn drop(&mut self) {
        if self.active {
            // Only an empty failed stage can be removed without walking
            // attacker-authored entries. Explicit error paths call the bounded
            // descriptor purge above; this is only a last-resort unwind guard.
            unsafe {
                libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), libc::AT_REMOVEDIR);
            }
        }
    }
}

const STAGE_CLEANUP_MAX_ENTRIES: u64 = 1_000_000;
const STAGE_CLEANUP_MAX_OPERATIONS: u64 = 2_000_000;
const STAGE_CLEANUP_MAX_NAME_BYTES: u64 = 64 * MIB;
const STAGE_CLEANUP_MAX_DEPTH: usize = 128;
const STAGE_CLEANUP_WALL_TIME: Duration = Duration::from_secs(300);

struct StageCleanupBudget {
    entries: u64,
    operations: u64,
    name_bytes: u64,
    device: u64,
    deadline: Instant,
}

impl StageCleanupBudget {
    fn new(device: u64) -> Self {
        Self {
            entries: 0,
            operations: 0,
            name_bytes: 0,
            device,
            deadline: Instant::now() + STAGE_CLEANUP_WALL_TIME,
        }
    }

    fn entry(&mut self, name_bytes: usize) -> io::Result<()> {
        self.entries = self.entries.checked_add(1).ok_or_else(cleanup_limit_error)?;
        self.name_bytes = self
            .name_bytes
            .checked_add(u64::try_from(name_bytes).map_err(|_| cleanup_limit_error())?)
            .ok_or_else(cleanup_limit_error)?;
        self.operation()?;
        self.require_limits()
    }

    fn operation(&mut self) -> io::Result<()> {
        self.operations = self.operations.checked_add(1).ok_or_else(cleanup_limit_error)?;
        self.require_limits()
    }

    fn require_limits(&self) -> io::Result<()> {
        if self.entries > STAGE_CLEANUP_MAX_ENTRIES
            || self.operations > STAGE_CLEANUP_MAX_OPERATIONS
            || self.name_bytes > STAGE_CLEANUP_MAX_NAME_BYTES
            || Instant::now() > self.deadline
        {
            Err(cleanup_limit_error())
        } else {
            Ok(())
        }
    }
}

fn cleanup_limit_error() -> io::Error {
    io::Error::other("private archive stage exceeds bounded cleanup limits")
}

fn purge_directory_contents(directory: &File, budget: &mut StageCleanupBudget, depth: usize) -> io::Result<()> {
    if depth > STAGE_CLEANUP_MAX_DEPTH {
        return Err(cleanup_limit_error());
    }
    for name in sorted_directory_names(directory, budget)? {
        purge_named_entry(directory, &name, budget, depth + 1)?;
    }
    Ok(())
}

fn purge_named_entry(parent: &File, name: &CStr, budget: &mut StageCleanupBudget, depth: usize) -> io::Result<()> {
    budget.operation()?;
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == -1 {
        let source = io::Error::last_os_error();
        return if source.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(source)
        };
    }
    let metadata = unsafe { metadata.assume_init() };
    if metadata.st_dev != budget.device {
        return Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            "private archive stage crosses a mount boundary",
        ));
    }
    if metadata.st_mode & libc::S_IFMT == libc::S_IFDIR {
        if depth > STAGE_CLEANUP_MAX_DEPTH {
            return Err(cleanup_limit_error());
        }
        let fd = openat2(
            parent.as_raw_fd(),
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )?;
        let child = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::fchmod(child.as_raw_fd(), 0o700) } == -1 {
            return Err(io::Error::last_os_error());
        }
        purge_directory_contents(&child, budget, depth)?;
        budget.operation()?;
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } == -1 {
            return Err(io::Error::last_os_error());
        }
    } else {
        budget.operation()?;
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn sorted_directory_names(directory: &File, budget: &mut StageCleanupBudget) -> io::Result<Vec<CString>> {
    let fd = openat2(
        directory.as_raw_fd(),
        c".",
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )?;
    let cursor = unsafe { File::from_raw_fd(fd) };
    let raw = cursor.into_raw_fd();
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        unsafe { libc::close(raw) };
        return Err(source);
    }
    let result = (|| -> io::Result<Vec<CString>> {
        let mut names = Vec::new();
        loop {
            unsafe { *libc::__errno_location() = 0 };
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                let error = unsafe { *libc::__errno_location() };
                if error != 0 {
                    return Err(io::Error::from_raw_os_error(error));
                }
                break;
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            budget.entry(name.to_bytes().len())?;
            names.push(name.to_owned());
        }
        names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        Ok(names)
    })();
    let close = unsafe { libc::closedir(stream) };
    let names = result?;
    if close == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(names)
    }
}

fn require_limit(resource: &'static str, actual: u64, limit: u64) -> Result<(), Error> {
    if actual <= limit {
        Ok(())
    } else {
        Err(Error::LimitExceeded {
            resource,
            actual,
            limit,
        })
    }
}

fn aggregate_add(resource: &'static str, total: &mut u64, value: u64, limit: u64) -> Result<(), Error> {
    let next = total.checked_add(value).ok_or(Error::ArithmeticOverflow)?;
    require_limit(resource, next, limit)?;
    *total = next;
    Ok(())
}

fn require_usize_limit(resource: &'static str, actual: usize, limit: usize) -> Result<(), Error> {
    if actual <= limit {
        Ok(())
    } else {
        Err(Error::LimitExceeded {
            resource,
            actual: u64::try_from(actual).unwrap_or(u64::MAX),
            limit: u64::try_from(limit).unwrap_or(u64::MAX),
        })
    }
}

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
mod tests {
    use std::{
        fs,
        io::Cursor,
        os::unix::fs::MetadataExt,
        path::{Path, PathBuf},
        process::Command,
    };

    use flate2::{Compression, write::GzEncoder};
    use tar::{Builder, Header};
    use xz2::{
        stream::{Check, Filters, LzmaOptions, Stream},
        write::XzEncoder,
    };

    use super::*;

    fn limits() -> ArchiveLimits {
        ArchiveLimits {
            compressed_bytes: 1024 * 1024,
            decoded_bytes: 1024 * 1024,
            xz_decoder_memory_bytes: 16 * 1024 * 1024,
            zstd_window_log_max: 20,
            entries: 64,
            path_bytes: 64 * 1024,
            path_depth: 16,
            one_path_bytes: 4095,
            link_bytes: 4095,
            extension_bytes: 64 * 1024,
            total_extension_bytes: 64 * 1024,
            file_logical_bytes: 1024 * 1024,
            file_physical_bytes: 1024 * 1024,
            total_logical_bytes: 1024 * 1024,
            total_physical_bytes: 1024 * 1024,
            sparse_extents: 0,
            materialized_nodes: 64,
            wall_time: Duration::from_secs(30),
        }
    }

    fn archive(build: impl FnOnce(&mut Builder<Vec<u8>>)) -> Vec<u8> {
        let mut builder = Builder::new(Vec::new());
        build(&mut builder);
        builder.finish().unwrap();
        builder.into_inner().unwrap()
    }

    fn archive_extension_entry_sizes(bytes: &[u8]) -> Vec<u64> {
        let mut archive = Archive::new(Cursor::new(bytes));
        let mut entries = archive.entries().unwrap().raw(true);
        let mut sizes = Vec::new();
        while let Some(entry) = entries.next() {
            let entry = entry.unwrap();
            let kind = entry.header().entry_type();
            if kind.is_pax_local_extensions() || kind.is_gnu_longname() || kind.is_gnu_longlink() {
                sizes.push(entry.size());
            }
        }
        sizes
    }

    fn append(builder: &mut Builder<Vec<u8>>, path: &str, kind: EntryType, link: Option<&str>, data: &[u8]) {
        let mut header = Header::new_ustar();
        header.set_path(path).unwrap();
        header.set_entry_type(kind);
        header.set_mode(if kind.is_dir() { 0o755 } else { 0o644 });
        header.set_size(data.len() as u64);
        if let Some(link) = link {
            header.set_link_name(link).unwrap();
        }
        header.set_cksum();
        builder.append(&header, Cursor::new(data)).unwrap();
    }

    fn append_raw_path(builder: &mut Builder<Vec<u8>>, path: &[u8], kind: EntryType, link: Option<&[u8]>, data: &[u8]) {
        assert!(path.len() <= 100);
        let mut header = Header::new_ustar();
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..path.len()].copy_from_slice(path);
        header.set_entry_type(kind);
        header.set_mode(0o644);
        header.set_size(data.len() as u64);
        if let Some(link) = link {
            assert!(link.len() <= 100);
            header.as_mut_bytes()[157..257].fill(0);
            header.as_mut_bytes()[157..157 + link.len()].copy_from_slice(link);
        }
        header.set_cksum();
        builder.append(&header, Cursor::new(data)).unwrap();
    }

    fn scan_result(bytes: &[u8], strip_components: usize, limits: ArchiveLimits) -> Result<ScanResult, Error> {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        scan_archive(
            &mut file,
            strip_components,
            limits,
            None,
            1_700_000_000,
            ArchiveDeadline::new(limits.wall_time),
        )
    }

    fn scan(bytes: &[u8], strip_components: usize, limits: ArchiveLimits) -> Result<Vec<ManifestEntry>, Error> {
        scan_result(bytes, strip_components, limits).map(|result| result.manifest)
    }

    fn two_files() -> Vec<u8> {
        archive(|builder| {
            append(builder, "root/a", EntryType::Regular, None, b"a");
            append(builder, "root/b", EntryType::Regular, None, b"b");
        })
    }

    fn install(bytes: &[u8], limits: ArchiveLimits) -> Result<(tempfile::TempDir, PathBuf), Error> {
        let root = crate::private_tempdir();
        let sources = root.path().join("sources");
        let build = root.path().join("build");
        fs::create_dir(&sources).unwrap();
        fs::create_dir(&build).unwrap();
        fs::create_dir(build.join("out")).unwrap();
        fs::write(sources.join("source.tar"), bytes).unwrap();
        let digest = hex::encode(Sha256::digest(bytes));
        let mut session = ArchiveSessionBudget::production();
        extract_locked_tar_with_limits(
            &sources,
            "source.tar",
            &digest,
            &build,
            "out",
            1,
            1_700_000_000,
            limits,
            &mut session,
        )?;
        let output = build.join("out");
        Ok((root, output))
    }

    fn assert_limit(bytes: &[u8], mutate: impl FnOnce(&mut ArchiveLimits), resource: &'static str) {
        let mut exact = limits();
        mutate(&mut exact);
        scan(bytes, 0, exact).unwrap();
        let mut exceeded = exact;
        match resource {
            "archive entries" => exceeded.entries -= 1,
            "archive path bytes" => exceeded.path_bytes -= 1,
            "archive path depth" => exceeded.path_depth -= 1,
            "one archive path bytes" => exceeded.one_path_bytes -= 1,
            "one file logical bytes" => exceeded.file_logical_bytes -= 1,
            "one file physical bytes" => exceeded.file_physical_bytes -= 1,
            "aggregate expanded logical bytes" => exceeded.total_logical_bytes -= 1,
            "aggregate expanded physical bytes" => exceeded.total_physical_bytes -= 1,
            "archive link bytes" => exceeded.link_bytes -= 1,
            "materialized archive nodes" => exceeded.materialized_nodes -= 1,
            _ => panic!("unknown test resource"),
        }
        assert!(matches!(
            scan(bytes, 0, exceeded),
            Err(Error::LimitExceeded { resource: found, .. }) if found == resource
        ));
    }

    #[test]
    fn exact_entry_path_and_depth_limits_accept_n_and_reject_n_plus_one() {
        let entries = two_files();
        assert_limit(&entries, |value| value.entries = 2, "archive entries");
        assert_limit(&entries, |value| value.path_bytes = 12, "archive path bytes");
        assert_limit(
            &entries,
            |value| value.materialized_nodes = 3,
            "materialized archive nodes",
        );

        let depth = archive(|builder| append(builder, "a/b/c", EntryType::Regular, None, b""));
        assert_limit(&depth, |value| value.path_depth = 3, "archive path depth");
        assert_limit(
            &depth,
            |value| value.one_path_bytes = "a/b/c".len(),
            "one archive path bytes",
        );
    }

    #[test]
    fn exact_per_file_and_aggregate_expansion_limits_accept_n_and_reject_n_plus_one() {
        let one = archive(|builder| append(builder, "root/file", EntryType::Regular, None, b"ab"));
        assert_limit(&one, |value| value.file_logical_bytes = 2, "one file logical bytes");
        assert_limit(&one, |value| value.file_physical_bytes = 2, "one file physical bytes");

        let two = two_files();
        assert_limit(
            &two,
            |value| value.total_logical_bytes = 2,
            "aggregate expanded logical bytes",
        );
        assert_limit(
            &two,
            |value| value.total_physical_bytes = 2,
            "aggregate expanded physical bytes",
        );
    }

    #[test]
    fn derivation_archive_budget_accepts_exact_totals_and_rejects_the_next_extraction() {
        let bytes = two_files();
        let usage = scan_result(&bytes, 1, limits()).unwrap().usage;
        let aggregate = ArchiveAggregateLimits {
            extractions: 1,
            compressed_bytes: bytes.len() as u64,
            decoded_bytes: usage.decoded_bytes,
            entries: usage.entries,
            path_bytes: usage.path_bytes,
            extension_bytes: usage.extension_bytes,
            logical_bytes: usage.logical_bytes,
            physical_bytes: usage.physical_bytes,
            materialized_nodes: usage.materialized_nodes,
            wall_time: Duration::from_secs(30),
        };
        let mut exact = ArchiveSessionBudget::new(aggregate);
        exact.admit(bytes.len() as u64, usage).unwrap();
        assert!(matches!(
            exact.admit(bytes.len() as u64, usage),
            Err(Error::LimitExceeded {
                resource: "derivation archive extractions",
                actual: 2,
                limit: 1,
            })
        ));

        let mut too_small = ArchiveSessionBudget::new(ArchiveAggregateLimits {
            decoded_bytes: usage.decoded_bytes - 1,
            ..aggregate
        });
        assert!(matches!(
            too_small.admit(bytes.len() as u64, usage),
            Err(Error::LimitExceeded {
                resource: "derivation decoded archive bytes",
                ..
            })
        ));
    }

    #[test]
    fn exact_link_limit_accepts_n_and_rejects_n_plus_one() {
        let bytes = archive(|builder| append(builder, "root/link", EntryType::Symlink, Some("target"), b""));
        assert_limit(&bytes, |value| value.link_bytes = 6, "archive link bytes");
    }

    #[test]
    fn sparse_limit_zero_accepts_dense_and_rejects_the_first_sparse_extent() {
        let dense = archive(|builder| append(builder, "root/file", EntryType::Regular, None, b"x"));
        scan(&dense, 1, limits()).unwrap();

        let sparse = archive(|builder| {
            builder
                .append_pax_extensions([("GNU.sparse.map", b"0,1".as_slice())])
                .unwrap();
            append(builder, "root/file", EntryType::Regular, None, b"x");
        });
        assert!(matches!(scan(&sparse, 1, limits()), Err(Error::SparseEntry { .. })));
    }

    #[test]
    fn traversal_absolute_paths_and_escaping_links_are_rejected() {
        for path in [
            b"/absolute".as_slice(),
            b"root/../escape",
            b"root//ambiguous",
            b"root/./ambiguous",
        ] {
            let bytes = archive(|builder| append_raw_path(builder, path, EntryType::Regular, None, b"x"));
            assert!(matches!(scan(&bytes, 1, limits()), Err(Error::UnsafePath { .. })));
        }

        // The target is valid relative to the authored archive path only if
        // the stripped top-level component is still considered. It escapes
        // from the final extraction path and must therefore be rejected.
        let symlink = archive(|builder| append(builder, "top/a/link", EntryType::Symlink, Some("../../outside"), b""));
        assert!(matches!(
            scan(&symlink, 1, limits()),
            Err(Error::EscapingSymlink { .. })
        ));

        let hardlink =
            archive(|builder| append_raw_path(builder, b"root/link", EntryType::Link, Some(b"../outside"), b""));
        assert!(matches!(scan(&hardlink, 1, limits()), Err(Error::UnsafePath { .. })));
    }

    #[test]
    fn duplicate_and_parent_child_type_collisions_are_rejected() {
        let duplicate = archive(|builder| {
            append(builder, "root/file", EntryType::Regular, None, b"a");
            append(builder, "root/file", EntryType::Regular, None, b"b");
        });
        assert!(matches!(
            scan(&duplicate, 1, limits()),
            Err(Error::DuplicatePath { .. })
        ));

        let collision = archive(|builder| {
            append(builder, "root/node", EntryType::Regular, None, b"a");
            append(builder, "root/node/child", EntryType::Regular, None, b"b");
        });
        assert!(matches!(
            scan(&collision, 1, limits()),
            Err(Error::PathTypeCollision { .. })
        ));
    }

    #[test]
    fn special_unknown_and_forward_hardlink_entries_are_rejected() {
        for kind in [
            EntryType::Fifo,
            EntryType::Char,
            EntryType::Block,
            EntryType::Continuous,
            EntryType::new(b's'),
        ] {
            let bytes = archive(|builder| append(builder, "root/node", kind, None, b""));
            assert!(matches!(
                scan(&bytes, 1, limits()),
                Err(Error::UnsupportedInodeType { .. })
            ));
        }

        let forward = archive(|builder| {
            append(builder, "root/link", EntryType::Link, Some("root/file"), b"");
            append(builder, "root/file", EntryType::Regular, None, b"x");
        });
        assert!(matches!(
            scan(&forward, 1, limits()),
            Err(Error::ForwardHardlink { .. })
        ));
    }

    #[test]
    fn decoded_stream_limit_accepts_exact_n_and_rejects_n_plus_one() {
        let bytes = two_files();
        let mut exact = limits();
        exact.decoded_bytes = bytes.len() as u64;
        scan(&bytes, 1, exact).unwrap();
        exact.decoded_bytes -= 1;
        assert!(scan(&bytes, 1, exact).is_err());
    }

    #[test]
    fn plain_gzip_xz_and_zstd_tar_streams_share_one_parser_and_publication_boundary() {
        let plain = two_files();
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&plain).unwrap();
        let gzip = gzip.finish().unwrap();
        let mut xz = XzEncoder::new(Vec::new(), 6);
        xz.write_all(&plain).unwrap();
        let xz = xz.finish().unwrap();
        // Keep this fixture inside the deliberately small test decoder
        // ceiling. `encode_all` inherits zstd's default window, which is not
        // part of this test's contract and can legitimately exceed 2^20 even
        // for a tiny input. The dedicated boundary test below proves that
        // oversized frames are rejected.
        let mut zstd = zstd::stream::Encoder::new(Vec::new(), 3).unwrap();
        zstd.window_log(limits().zstd_window_log_max).unwrap();
        zstd.include_contentsize(false).unwrap();
        zstd.write_all(&plain).unwrap();
        let zstd = zstd.finish().unwrap();

        for bytes in [&plain, &gzip, &xz, &zstd] {
            assert_eq!(scan(bytes, 1, limits()).unwrap().len(), 2);
            let (_root, output) = install(bytes, limits()).unwrap();
            assert_eq!(fs::read(output.join("a")).unwrap(), b"a");
            assert_eq!(fs::read(output.join("b")).unwrap(), b"b");
        }
    }

    #[test]
    fn xz_decoder_memory_and_zstd_window_limits_accept_the_minimum_and_reject_less() {
        let plain = two_files();
        let mut options = LzmaOptions::new_preset(0).unwrap();
        options.dict_size(1024 * 1024);
        let mut filters = Filters::new();
        filters.lzma2(&options);
        let stream = Stream::new_stream_encoder(&filters, Check::Crc64).unwrap();
        let mut encoder = XzEncoder::new_stream(Vec::new(), stream);
        encoder.write_all(&plain).unwrap();
        let xz = encoder.finish().unwrap();

        let mut lower = 1u64;
        let mut upper = 16 * 1024 * 1024u64;
        while lower < upper {
            let midpoint = lower + (upper - lower) / 2;
            let mut candidate = limits();
            candidate.xz_decoder_memory_bytes = midpoint;
            if scan(&xz, 1, candidate).is_ok() {
                upper = midpoint;
            } else {
                lower = midpoint + 1;
            }
        }
        let mut exact = limits();
        exact.xz_decoder_memory_bytes = lower;
        scan(&xz, 1, exact).unwrap();
        exact.xz_decoder_memory_bytes -= 1;
        assert!(scan(&xz, 1, exact).is_err());

        let mut encoder = zstd::stream::Encoder::new(Vec::new(), 1).unwrap();
        encoder.window_log(20).unwrap();
        encoder.include_contentsize(false).unwrap();
        encoder.write_all(&plain).unwrap();
        let zstd = encoder.finish().unwrap();
        let minimum = (10..=30)
            .find(|window| {
                let mut candidate = limits();
                candidate.zstd_window_log_max = *window;
                scan(&zstd, 1, candidate).is_ok()
            })
            .expect("test zstd frame must fit a finite decoder window");
        let mut exact = limits();
        exact.zstd_window_log_max = minimum;
        scan(&zstd, 1, exact).unwrap();
        exact.zstd_window_log_max -= 1;
        assert!(scan(&zstd, 1, exact).is_err());
    }

    #[test]
    fn pax_size_is_rejected_and_extension_budgets_are_aggregate() {
        let pax_size = archive(|builder| {
            builder.append_pax_extensions([("size", b"1".as_slice())]).unwrap();
            append(builder, "root/file", EntryType::Regular, None, b"x");
        });
        assert!(matches!(
            scan(&pax_size, 1, limits()),
            Err(Error::UnsupportedPaxKey { .. })
        ));

        let long_path = b"root/this-is-a-long-extension-path";
        let extensions = archive(|builder| {
            builder.append_pax_extensions([("path", long_path.as_slice())]).unwrap();
            append(builder, "placeholder", EntryType::Regular, None, b"x");
            builder
                .append_pax_extensions([("comment", b"bounded".as_slice())])
                .unwrap();
            append(builder, "root/second", EntryType::Regular, None, b"y");
        });
        let admitted = scan(&extensions, 1, limits()).unwrap();
        assert_eq!(join_components(&admitted[0].path), b"this-is-a-long-extension-path");

        let extension_sizes = archive_extension_entry_sizes(&extensions);
        let total = extension_sizes.iter().sum::<u64>();
        let largest = *extension_sizes.iter().max().unwrap();
        let mut exact = limits();
        exact.extension_bytes = largest;
        exact.total_extension_bytes = total;
        scan(&extensions, 1, exact).unwrap();
        exact.total_extension_bytes -= 1;
        assert!(matches!(
            scan(&extensions, 1, exact),
            Err(Error::LimitExceeded {
                resource: "aggregate archive extension bytes",
                ..
            })
        ));
    }

    #[test]
    fn unsupported_compressed_container_has_no_external_fallback() {
        let mut bytes = b"BZh".to_vec();
        bytes.extend_from_slice(&[0; 1024]);
        assert!(matches!(
            scan(&bytes, 0, limits()),
            Err(Error::UnsupportedArchiveCompression)
        ));
    }

    #[test]
    fn verified_manifest_is_published_with_safe_links_and_exact_contents() {
        let bytes = archive(|builder| {
            append(builder, "root", EntryType::Directory, None, b"");
            append(builder, "root/file", EntryType::Regular, None, b"payload");
            append(builder, "root/symlink", EntryType::Symlink, Some("file"), b"");
            append(builder, "root/hardlink", EntryType::Link, Some("root/file"), b"");
        });
        let (_root, output) = install(&bytes, limits()).unwrap();
        assert_eq!(fs::read(output.join("file")).unwrap(), b"payload");
        assert_eq!(fs::read_link(output.join("symlink")).unwrap(), Path::new("file"));
        assert_eq!(
            fs::metadata(output.join("file")).unwrap().ino(),
            fs::metadata(output.join("hardlink")).unwrap().ino()
        );
        assert!(!output.with_file_name(".cast-archive-stage").exists());
    }

    #[test]
    fn implicit_directories_stage_root_and_symlinks_have_normalized_metadata() {
        let epoch = 1_700_000_000;
        let bytes = archive(|builder| {
            append(builder, "root/nested/file", EntryType::Regular, None, b"payload");
            append(builder, "root/nested/link", EntryType::Symlink, Some("file"), b"");
        });
        let (_root, output) = install(&bytes, limits()).unwrap();
        let output_metadata = fs::symlink_metadata(&output).unwrap();
        let nested_metadata = fs::symlink_metadata(output.join("nested")).unwrap();
        let link_metadata = fs::symlink_metadata(output.join("nested/link")).unwrap();

        assert_eq!(output_metadata.mode() & 0o777, 0o755);
        assert_eq!(nested_metadata.mode() & 0o777, 0o755);
        assert_eq!(output_metadata.mtime(), epoch);
        assert_eq!(nested_metadata.mtime(), epoch);
        assert_eq!(link_metadata.mtime(), epoch);
        assert_eq!(fs::read(output.join("nested/link")).unwrap(), b"payload");
    }

    #[test]
    fn compressed_input_limit_accepts_exact_n_and_rejects_n_plus_one() {
        let bytes = two_files();
        let mut exact = limits();
        exact.compressed_bytes = bytes.len() as u64;
        install(&bytes, exact).unwrap();
        exact.compressed_bytes -= 1;
        assert!(matches!(
            install(&bytes, exact),
            Err(Error::LimitExceeded {
                resource: "compressed archive bytes",
                ..
            })
        ));
    }

    #[test]
    fn zero_wall_time_is_rejected_at_the_first_checkpoint() {
        let mut expired = limits();
        expired.wall_time = Duration::ZERO;
        assert!(matches!(
            scan(&two_files(), 1, expired),
            Err(Error::WallTimeExceeded { limit }) if limit == Duration::ZERO
        ));
    }

    #[test]
    fn archive_snapshot_is_immutable_before_either_parser_pass() {
        let bytes = two_files();
        let mut source = tempfile::tempfile().unwrap();
        source.write_all(&bytes).unwrap();
        source.seek(SeekFrom::Start(0)).unwrap();
        let digest = hex::encode(Sha256::digest(&bytes));
        let mut snapshot = sealed_archive_snapshot(
            source,
            &digest,
            bytes.len() as u64,
            ArchiveDeadline::new(Duration::from_secs(30)),
        )
        .unwrap();

        assert!(snapshot.write_all(b"mutation").is_err());
        assert!(snapshot.set_len(0).is_err());
        snapshot.seek(SeekFrom::Start(0)).unwrap();
        let mut retained = Vec::new();
        snapshot.read_to_end(&mut retained).unwrap();
        assert_eq!(retained, bytes);
    }

    #[test]
    fn failed_stage_open_removes_the_just_created_empty_stage() {
        let bytes = two_files();
        let root = crate::private_tempdir();
        let sources = root.path().join("sources");
        let build = root.path().join("build");
        fs::create_dir(&sources).unwrap();
        fs::create_dir(&build).unwrap();
        fs::write(sources.join("source.tar"), &bytes).unwrap();
        let digest = hex::encode(Sha256::digest(&bytes));

        TEST_FAIL_STAGE_OPEN.with(|fail| fail.set(true));
        let mut session = ArchiveSessionBudget::production();
        let result = extract_locked_tar_with_limits(
            &sources,
            "source.tar",
            &digest,
            &build,
            "out",
            1,
            1_700_000_000,
            limits(),
            &mut session,
        );
        TEST_FAIL_STAGE_OPEN.with(|fail| fail.set(false));

        assert!(result.is_err());
        assert!(fs::read_dir(&build).unwrap().next().is_none());
    }

    #[test]
    fn post_rename_durability_failure_never_purges_the_published_tree() {
        let bytes = two_files();
        let root = crate::private_tempdir();
        let sources = root.path().join("sources");
        let build = root.path().join("build");
        fs::create_dir(&sources).unwrap();
        fs::create_dir(&build).unwrap();
        fs::create_dir(build.join("out")).unwrap();
        fs::write(sources.join("source.tar"), &bytes).unwrap();
        let digest = hex::encode(Sha256::digest(&bytes));

        TEST_FAIL_PUBLISH_AFTER_RENAME.with(|fail| fail.set(true));
        let mut session = ArchiveSessionBudget::production();
        let result = extract_locked_tar_with_limits(
            &sources,
            "source.tar",
            &digest,
            &build,
            "out",
            1,
            1_700_000_000,
            limits(),
            &mut session,
        );
        TEST_FAIL_PUBLISH_AFTER_RENAME.with(|fail| fail.set(false));

        assert!(result.is_err());
        assert_eq!(fs::read(build.join("out/a")).unwrap(), b"a");
        assert_eq!(fs::read(build.join("out/b")).unwrap(), b"b");
        assert_eq!(
            fs::read_dir(&build)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            ["out"]
        );
    }

    #[test]
    fn restrictive_umask_cannot_change_nested_destination_metadata() {
        const CHILD: &str = "CAST_ARCHIVE_UMASK_CHILD";
        const TEST: &str = "archive::tests::restrictive_umask_cannot_change_nested_destination_metadata";
        if std::env::var_os(CHILD).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .arg(TEST)
                .arg("--exact")
                .arg("--nocapture")
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "restrictive-umask archive child failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let bytes = two_files();
        let root = crate::private_tempdir();
        let sources = root.path().join("sources");
        let build = root.path().join("build");
        fs::create_dir(&sources).unwrap();
        fs::create_dir(&build).unwrap();
        fs::write(sources.join("source.tar"), &bytes).unwrap();
        let digest = hex::encode(Sha256::digest(&bytes));
        let mut session = ArchiveSessionBudget::production();

        // Apply the hostile mask only after creating the trusted test inputs.
        // This keeps the setup readable while ensuring every destination
        // parent and extracted node is created under the restrictive mask.
        unsafe { libc::umask(0o777) };
        extract_locked_tar_with_limits(
            &sources,
            "source.tar",
            &digest,
            &build,
            "nested/parents/out",
            1,
            1_700_000_000,
            limits(),
            &mut session,
        )
        .unwrap();

        for directory in ["nested", "nested/parents", "nested/parents/out"] {
            let metadata = fs::metadata(build.join(directory)).unwrap();
            assert_eq!(metadata.mode() & 0o777, 0o755, "{directory}");
            assert_eq!(metadata.mtime(), 1_700_000_000, "{directory}");
        }
        assert_eq!(fs::read(build.join("nested/parents/out/a")).unwrap(), b"a");
    }

    #[test]
    fn stage_cleanup_budgets_accept_exact_boundaries_and_reject_n_plus_one() {
        let root = crate::private_tempdir();
        let directory = File::open(root.path()).unwrap();
        let device = directory.metadata().unwrap().dev();

        let mut entries = StageCleanupBudget::new(device);
        entries.entries = STAGE_CLEANUP_MAX_ENTRIES - 1;
        entries.entry(0).unwrap();
        assert_eq!(entries.entries, STAGE_CLEANUP_MAX_ENTRIES);
        assert!(entries.entry(0).is_err());

        let mut operations = StageCleanupBudget::new(device);
        operations.operations = STAGE_CLEANUP_MAX_OPERATIONS - 1;
        operations.operation().unwrap();
        assert_eq!(operations.operations, STAGE_CLEANUP_MAX_OPERATIONS);
        assert!(operations.operation().is_err());

        let mut names = StageCleanupBudget::new(device);
        names.name_bytes = STAGE_CLEANUP_MAX_NAME_BYTES - 1;
        names.entry(1).unwrap();
        assert_eq!(names.name_bytes, STAGE_CLEANUP_MAX_NAME_BYTES);
        assert!(names.entry(1).is_err());

        let mut depth = StageCleanupBudget::new(device);
        purge_directory_contents(&directory, &mut depth, STAGE_CLEANUP_MAX_DEPTH).unwrap();
        assert!(purge_directory_contents(&directory, &mut depth, STAGE_CLEANUP_MAX_DEPTH + 1).is_err());

        let mut deadline = StageCleanupBudget::new(device);
        deadline.deadline = Instant::now() - Duration::from_nanos(1);
        assert!(deadline.require_limits().is_err());
    }

    #[test]
    fn partial_private_stage_is_removed_after_extraction_failure() {
        let bytes = two_files();
        let root = crate::private_tempdir();
        let sources = root.path().join("sources");
        let build = root.path().join("build");
        fs::create_dir(&sources).unwrap();
        fs::create_dir(&build).unwrap();
        fs::create_dir(build.join("out")).unwrap();
        fs::write(sources.join("source.tar"), &bytes).unwrap();
        let digest = hex::encode(Sha256::digest(&bytes));

        TEST_STAGE_WRITES_BEFORE_FAILURE.with(|remaining| remaining.set(1));
        let mut session = ArchiveSessionBudget::production();
        let result = extract_locked_tar_with_limits(
            &sources,
            "source.tar",
            &digest,
            &build,
            "out",
            1,
            1_700_000_000,
            limits(),
            &mut session,
        );
        TEST_STAGE_WRITES_BEFORE_FAILURE.with(|remaining| remaining.set(u64::MAX));

        assert!(result.is_err());
        assert!(fs::read_dir(build.join("out")).unwrap().next().is_none());
        let build_entries = fs::read_dir(&build)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(build_entries, ["out"]);
    }
}
