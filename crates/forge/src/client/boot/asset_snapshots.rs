//! Pre-claim, digest-authenticated CAS snapshots for boot projection inputs.

use std::{
    collections::BTreeSet,
    ffi::CStr,
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd},
    time::{Duration, Instant},
};

use nix::{
    fcntl::{FcntlArg, FdFlag, SealFlag, fcntl},
    sys::{
        memfd::{MemFdCreateFlag, memfd_create},
        stat::{Mode, fchmod, fstat},
    },
    unistd::{Whence, lseek},
};

use super::{
    AssetPool, EMPTY_FILE_DIGEST, Error as ClientError, OpenedAsset,
    active_reblit_boot_projection::{MAX_BOOT_PLAN_SNAPSHOT_DIGESTS, PreparedActiveReblitBootAssetPlan},
    copy_fd_exact, frozen_asset_path, require_asset_unchanged_until,
};
use crate::Installation;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const MAX_BOOT_ASSET_DECLARATIONS: usize = MAX_BOOT_PLAN_SNAPSHOT_DIGESTS;
const MAX_BOOT_ASSET_BYTES: u64 = 512 * MIB;
const MAX_TOTAL_BOOT_ASSET_BYTES: u64 = 2 * GIB;
// At most 256 unique non-empty inputs retain two authenticated source-chain
// descriptors and one eventual memfd each. Keep complete preparation below
// the common 1024-descriptor process limit, including a conservative allowance
// for AssetPool revalidation's temporary descriptors.
const MAX_BOOT_ASSET_DESCRIPTORS: usize = 800;
const RETAINED_SOURCE_DESCRIPTORS_PER_ASSET: usize = 2;
const ASSET_POOL_TRANSIENT_DESCRIPTORS: usize = 16;
const BOOT_ASSET_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(120);
const SNAPSHOT_MODE: u32 = 0o400;
const SNAPSHOT_NAME: &CStr = c"forge-boot-cas-snapshot";

const BOOT_ASSET_SNAPSHOT_POLICY: BootAssetSnapshotPolicy = BootAssetSnapshotPolicy {
    max_declarations: MAX_BOOT_ASSET_DECLARATIONS,
    max_asset_bytes: MAX_BOOT_ASSET_BYTES,
    max_total_bytes: MAX_TOTAL_BOOT_ASSET_BYTES,
    max_descriptors: MAX_BOOT_ASSET_DESCRIPTORS,
    timeout: BOOT_ASSET_SNAPSHOT_TIMEOUT,
};

/// A complete attempt-local set of sealed CAS inputs.
///
/// This owner is intentionally not `Clone`. Failed preparation drops every
/// already-created memfd, and successful preparation retains exactly one
/// descriptor for each unique digest.
pub(in crate::client) struct PreparedBootAssetSnapshots {
    snapshots: Vec<SealedBootAssetSnapshot>,
}

impl PreparedBootAssetSnapshots {
    pub(in crate::client) fn prepare(
        installation: &Installation,
        plan: &PreparedActiveReblitBootAssetPlan,
    ) -> Result<Self, BootAssetSnapshotError> {
        prepare_digests(installation, plan.snapshot_digests().iter().copied())
    }

    pub(in crate::client) fn snapshot_for(&self, digest: u128) -> Option<&SealedBootAssetSnapshot> {
        self.snapshots
            .binary_search_by_key(&digest, SealedBootAssetSnapshot::digest)
            .ok()
            .map(|index| &self.snapshots[index])
    }

    pub(in crate::client) fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub(in crate::client) fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    pub(in crate::client) fn snapshots(&self) -> impl ExactSizeIterator<Item = &SealedBootAssetSnapshot> {
        self.snapshots.iter()
    }
}

fn prepare_digests(
    installation: &Installation,
    digests: impl IntoIterator<Item = u128>,
) -> Result<PreparedBootAssetSnapshots, BootAssetSnapshotError> {
    let deadline = Instant::now().checked_add(BOOT_ASSET_SNAPSHOT_POLICY.timeout).ok_or(
        BootAssetSnapshotError::InvalidDeadline {
            timeout: BOOT_ASSET_SNAPSHOT_POLICY.timeout,
        },
    )?;
    prepare_with_policy_until(installation, digests, BOOT_ASSET_SNAPSHOT_POLICY, deadline)
}

/// One sealed, anonymous, digest-authenticated boot input.
///
/// The owned descriptor and its mutable source path are never exposed. A boot
/// worker may borrow only the sealed descriptor and the two declarative facts
/// needed to bind it into a projection.
pub(in crate::client) struct SealedBootAssetSnapshot {
    descriptor: OwnedFd,
    digest: u128,
    length: u64,
}

impl SealedBootAssetSnapshot {
    pub(in crate::client) fn descriptor(&self) -> BorrowedFd<'_> {
        self.descriptor.as_fd()
    }

    pub(in crate::client) fn digest(&self) -> u128 {
        self.digest
    }

    pub(in crate::client) fn length(&self) -> u64 {
        self.length
    }
}

#[derive(Clone, Copy)]
struct BootAssetSnapshotPolicy {
    max_declarations: usize,
    max_asset_bytes: u64,
    max_total_bytes: u64,
    max_descriptors: usize,
    timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AssetSnapshotCheckpoint {
    SourceOpened { digest: u128 },
    BytesCopied { digest: u128 },
    SnapshotSealed { digest: u128, descriptor: RawFd },
}

struct AuthenticatedAssetSource {
    digest: u128,
    asset: OpenedAsset,
}

fn prepare_with_policy_until(
    installation: &Installation,
    digests: impl IntoIterator<Item = u128>,
    policy: BootAssetSnapshotPolicy,
    deadline: Instant,
) -> Result<PreparedBootAssetSnapshots, BootAssetSnapshotError> {
    prepare_with_policy_until_and_checkpoint(installation, digests, policy, deadline, |_| {})
}

fn prepare_with_policy_until_and_checkpoint<F>(
    installation: &Installation,
    digests: impl IntoIterator<Item = u128>,
    policy: BootAssetSnapshotPolicy,
    deadline: Instant,
    mut checkpoint: F,
) -> Result<PreparedBootAssetSnapshots, BootAssetSnapshotError>
where
    F: FnMut(AssetSnapshotCheckpoint),
{
    // Phase 1: canonicalize the complete request and reject its count and
    // descriptor costs before opening the pool or creating any memfd.
    let digests = preflight_digests(digests, policy, deadline)?;
    require_snapshot_deadline(deadline, policy.timeout)?;

    // Phase 2: derive byte lengths only from authenticated, retained source
    // descriptors. All source descriptors remain live until their exact copy
    // and post-copy pathname revalidation; no memfd exists during this pass.
    let needs_pool = digests.iter().any(|digest| *digest != EMPTY_FILE_DIGEST);
    let (pool, sources) = if needs_pool {
        let pool = AssetPool::open_until(installation, deadline)
            .map_err(|source| map_pool_error(source, policy.timeout, deadline))?;
        require_snapshot_deadline(deadline, policy.timeout)?;
        let sources = authenticate_asset_sources(&pool, &digests, policy, deadline, &mut checkpoint)?;
        require_snapshot_deadline(deadline, policy.timeout)?;
        (Some(pool), sources)
    } else {
        (None, Vec::new())
    };

    // Phase 3: copy from retained capabilities, prove each public source name
    // and pool unchanged, then publish only fully sealed anonymous snapshots.
    let mut sources = sources.into_iter();
    let mut snapshots = Vec::with_capacity(digests.len());
    for digest in digests {
        require_snapshot_deadline(deadline, policy.timeout)?;
        let snapshot = if digest == EMPTY_FILE_DIGEST {
            create_empty_snapshot(digest, policy, deadline)?
        } else {
            let source = sources
                .next()
                .expect("each non-empty digest must retain one authenticated source");
            debug_assert_eq!(source.digest, digest);
            create_source_snapshot(
                pool.as_ref().expect("non-empty digests require an authenticated pool"),
                source,
                policy,
                deadline,
                &mut checkpoint,
            )?
        };
        checkpoint(AssetSnapshotCheckpoint::SnapshotSealed {
            digest,
            descriptor: snapshot.descriptor.as_raw_fd(),
        });
        snapshots.push(snapshot);
    }
    require_snapshot_deadline(deadline, policy.timeout)?;

    Ok(PreparedBootAssetSnapshots { snapshots })
}

fn preflight_digests(
    digests: impl IntoIterator<Item = u128>,
    policy: BootAssetSnapshotPolicy,
    deadline: Instant,
) -> Result<BTreeSet<u128>, BootAssetSnapshotError> {
    let mut canonical = BTreeSet::new();
    let mut declaration_count = 0usize;

    for digest in digests {
        require_snapshot_deadline(deadline, policy.timeout)?;
        declaration_count = declaration_count.saturating_add(1);
        if declaration_count > policy.max_declarations {
            return Err(BootAssetSnapshotError::AssetCountLimit {
                limit: policy.max_declarations,
                actual: declaration_count,
            });
        }
        canonical.insert(digest);
    }

    let non_empty = canonical.iter().filter(|digest| **digest != EMPTY_FILE_DIGEST).count();
    let source_descriptors = non_empty.saturating_mul(RETAINED_SOURCE_DESCRIPTORS_PER_ASSET);
    let transient = usize::from(non_empty != 0).saturating_mul(ASSET_POOL_TRANSIENT_DESCRIPTORS);
    let required_descriptors = canonical
        .len()
        .saturating_add(source_descriptors)
        .saturating_add(transient);
    if required_descriptors > policy.max_descriptors {
        return Err(BootAssetSnapshotError::DescriptorLimit {
            limit: policy.max_descriptors,
            actual: required_descriptors,
        });
    }

    Ok(canonical)
}

fn authenticate_asset_sources<F>(
    pool: &AssetPool,
    digests: &BTreeSet<u128>,
    policy: BootAssetSnapshotPolicy,
    deadline: Instant,
    checkpoint: &mut F,
) -> Result<Vec<AuthenticatedAssetSource>, BootAssetSnapshotError>
where
    F: FnMut(AssetSnapshotCheckpoint),
{
    let mut sources = Vec::with_capacity(digests.len());
    let mut total_bytes = 0u64;

    for digest in digests.iter().copied().filter(|digest| *digest != EMPTY_FILE_DIGEST) {
        require_snapshot_deadline(deadline, policy.timeout)?;
        let asset = pool
            .open_asset_until(&frozen_asset_path(digest), deadline)
            .map_err(|source| map_open_source_error(digest, source, policy.timeout, deadline))?;
        require_snapshot_deadline(deadline, policy.timeout)?;
        let length = asset.witness.length;
        if length > policy.max_asset_bytes {
            return Err(BootAssetSnapshotError::AssetByteLimit {
                digest,
                limit: policy.max_asset_bytes,
                actual: length,
            });
        }
        let actual_total = total_bytes.checked_add(length).unwrap_or(u64::MAX);
        if actual_total > policy.max_total_bytes {
            return Err(BootAssetSnapshotError::AggregateByteLimit {
                limit: policy.max_total_bytes,
                actual: actual_total,
            });
        }
        total_bytes = actual_total;
        checkpoint(AssetSnapshotCheckpoint::SourceOpened { digest });
        sources.push(AuthenticatedAssetSource { digest, asset });
    }
    pool.revalidate_until(deadline)
        .map_err(|source| map_pool_error(source, policy.timeout, deadline))?;

    Ok(sources)
}

fn create_empty_snapshot(
    digest: u128,
    policy: BootAssetSnapshotPolicy,
    deadline: Instant,
) -> Result<SealedBootAssetSnapshot, BootAssetSnapshotError> {
    debug_assert_eq!(digest, EMPTY_FILE_DIGEST);
    require_snapshot_deadline(deadline, policy.timeout)?;
    let descriptor = create_snapshot_descriptor(digest)?;
    finish_snapshot(descriptor, digest, 0, policy.timeout, deadline)
}

fn create_source_snapshot<F>(
    pool: &AssetPool,
    source: AuthenticatedAssetSource,
    policy: BootAssetSnapshotPolicy,
    deadline: Instant,
    checkpoint: &mut F,
) -> Result<SealedBootAssetSnapshot, BootAssetSnapshotError>
where
    F: FnMut(AssetSnapshotCheckpoint),
{
    let AuthenticatedAssetSource { digest, asset } = source;
    let length = asset.witness.length;
    require_snapshot_deadline(deadline, policy.timeout)?;
    let descriptor = create_snapshot_descriptor(digest)?;
    copy_fd_exact(
        asset.file.as_raw_fd(),
        descriptor.as_raw_fd(),
        length,
        digest,
        Some(deadline),
    )
    .map_err(|source| map_copy_error(digest, source, policy.timeout, deadline))?;
    checkpoint(AssetSnapshotCheckpoint::BytesCopied { digest });

    require_snapshot_deadline(deadline, policy.timeout)?;
    require_asset_unchanged_until(pool, &asset, deadline)
        .map_err(|source| map_source_changed_error(digest, source, policy.timeout, deadline))?;
    require_snapshot_deadline(deadline, policy.timeout)?;

    finish_snapshot(descriptor, digest, length, policy.timeout, deadline)
}

fn create_snapshot_descriptor(digest: u128) -> Result<OwnedFd, BootAssetSnapshotError> {
    memfd_create(
        SNAPSHOT_NAME,
        MemFdCreateFlag::MFD_CLOEXEC | MemFdCreateFlag::MFD_ALLOW_SEALING,
    )
    .map_err(|source| BootAssetSnapshotError::CreateSnapshot { digest, source })
}

fn finish_snapshot(
    descriptor: OwnedFd,
    digest: u128,
    length: u64,
    timeout: Duration,
    deadline: Instant,
) -> Result<SealedBootAssetSnapshot, BootAssetSnapshotError> {
    require_snapshot_deadline(deadline, timeout)?;
    fchmod(descriptor.as_raw_fd(), Mode::from_bits_truncate(SNAPSHOT_MODE))
        .map_err(|source| BootAssetSnapshotError::ProtectSnapshot { digest, source })?;
    let position = lseek(descriptor.as_raw_fd(), 0, Whence::SeekSet)
        .map_err(|source| BootAssetSnapshotError::RewindSnapshot { digest, source })?;
    if position != 0 {
        return Err(BootAssetSnapshotError::RewindPosition {
            digest,
            actual: position,
        });
    }

    let required_seals =
        SealFlag::F_SEAL_WRITE | SealFlag::F_SEAL_GROW | SealFlag::F_SEAL_SHRINK | SealFlag::F_SEAL_SEAL;
    fcntl(descriptor.as_raw_fd(), FcntlArg::F_ADD_SEALS(required_seals))
        .map_err(|source| BootAssetSnapshotError::SealSnapshot { digest, source })?;
    let actual_seals = fcntl(descriptor.as_raw_fd(), FcntlArg::F_GET_SEALS)
        .map_err(|source| BootAssetSnapshotError::InspectSnapshotSeals { digest, source })?;
    if actual_seals & required_seals.bits() != required_seals.bits() {
        return Err(BootAssetSnapshotError::MissingSnapshotSeals {
            digest,
            expected: required_seals.bits(),
            actual: actual_seals,
        });
    }

    let descriptor_flags = fcntl(descriptor.as_raw_fd(), FcntlArg::F_GETFD)
        .map_err(|source| BootAssetSnapshotError::InspectSnapshotDescriptor { digest, source })?;
    let stat = fstat(descriptor.as_raw_fd())
        .map_err(|source| BootAssetSnapshotError::InspectSnapshotMetadata { digest, source })?;
    let actual_length = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
    let actual_kind = stat.st_mode & nix::libc::S_IFMT;
    let actual_mode = stat.st_mode & 0o777;
    if descriptor_flags & FdFlag::FD_CLOEXEC.bits() == 0
        || actual_kind != nix::libc::S_IFREG
        || actual_mode != SNAPSHOT_MODE
        || actual_length != length
    {
        return Err(BootAssetSnapshotError::InvalidSnapshotMetadata {
            digest,
            descriptor_flags,
            kind: actual_kind,
            mode: actual_mode,
            length: actual_length,
            expected_length: length,
        });
    }
    require_snapshot_deadline(deadline, timeout)?;

    Ok(SealedBootAssetSnapshot {
        descriptor,
        digest,
        length,
    })
}

fn require_snapshot_deadline(deadline: Instant, timeout: Duration) -> Result<(), BootAssetSnapshotError> {
    if Instant::now() > deadline {
        Err(BootAssetSnapshotError::DeadlineExceeded { timeout })
    } else {
        Ok(())
    }
}

fn client_error_is_deadline(source: &ClientError, deadline: Instant) -> bool {
    Instant::now() > deadline
        || matches!(source, ClientError::FrozenMaterializationTimeout { .. })
        || matches!(source, ClientError::Io(source) if source.kind() == std::io::ErrorKind::TimedOut)
}

fn map_pool_error(source: ClientError, timeout: Duration, deadline: Instant) -> BootAssetSnapshotError {
    if client_error_is_deadline(&source, deadline) {
        BootAssetSnapshotError::DeadlineExceeded { timeout }
    } else {
        BootAssetSnapshotError::OpenAssetPool {
            source: Box::new(source),
        }
    }
}

fn map_open_source_error(
    digest: u128,
    source: ClientError,
    timeout: Duration,
    deadline: Instant,
) -> BootAssetSnapshotError {
    if client_error_is_deadline(&source, deadline) {
        BootAssetSnapshotError::DeadlineExceeded { timeout }
    } else {
        BootAssetSnapshotError::OpenAssetSource {
            digest,
            source: Box::new(source),
        }
    }
}

fn map_copy_error(digest: u128, source: ClientError, timeout: Duration, deadline: Instant) -> BootAssetSnapshotError {
    if client_error_is_deadline(&source, deadline) {
        BootAssetSnapshotError::DeadlineExceeded { timeout }
    } else {
        BootAssetSnapshotError::CopyAssetSource {
            digest,
            source: Box::new(source),
        }
    }
}

fn map_source_changed_error(
    digest: u128,
    source: ClientError,
    timeout: Duration,
    deadline: Instant,
) -> BootAssetSnapshotError {
    if client_error_is_deadline(&source, deadline) {
        BootAssetSnapshotError::DeadlineExceeded { timeout }
    } else {
        BootAssetSnapshotError::RevalidateAssetSource {
            digest,
            source: Box::new(source),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum BootAssetSnapshotError {
    #[error("boot asset snapshot deadline could not be represented for timeout {timeout:?}")]
    InvalidDeadline { timeout: Duration },
    #[error("boot asset snapshot preparation exceeded its {timeout:?} deadline")]
    DeadlineExceeded { timeout: Duration },
    #[error("boot asset declarations exceed the count limit of {limit} (got {actual})")]
    AssetCountLimit { limit: usize, actual: usize },
    #[error("boot asset {digest:032x} exceeds the per-asset limit of {limit} bytes (got {actual})")]
    AssetByteLimit { digest: u128, limit: u64, actual: u64 },
    #[error("boot asset snapshots exceed the aggregate limit of {limit} bytes (got {actual})")]
    AggregateByteLimit { limit: u64, actual: u64 },
    #[error("boot asset snapshots require {actual} descriptors against a limit of {limit}")]
    DescriptorLimit { limit: usize, actual: usize },
    #[error("open the descriptor-authenticated boot asset pool")]
    OpenAssetPool {
        #[source]
        source: Box<ClientError>,
    },
    #[error("open boot asset {digest:032x} through the authenticated asset pool")]
    OpenAssetSource {
        digest: u128,
        #[source]
        source: Box<ClientError>,
    },
    #[error("create anonymous snapshot for boot asset {digest:032x}")]
    CreateSnapshot {
        digest: u128,
        #[source]
        source: nix::Error,
    },
    #[error("copy and authenticate boot asset {digest:032x}")]
    CopyAssetSource {
        digest: u128,
        #[source]
        source: Box<ClientError>,
    },
    #[error("revalidate boot asset {digest:032x} after copying")]
    RevalidateAssetSource {
        digest: u128,
        #[source]
        source: Box<ClientError>,
    },
    #[error("set read-only mode on boot asset snapshot {digest:032x}")]
    ProtectSnapshot {
        digest: u128,
        #[source]
        source: nix::Error,
    },
    #[error("rewind boot asset snapshot {digest:032x}")]
    RewindSnapshot {
        digest: u128,
        #[source]
        source: nix::Error,
    },
    #[error("rewinding boot asset snapshot {digest:032x} returned unexpected offset {actual}")]
    RewindPosition { digest: u128, actual: i64 },
    #[error("seal boot asset snapshot {digest:032x}")]
    SealSnapshot {
        digest: u128,
        #[source]
        source: nix::Error,
    },
    #[error("inspect seals on boot asset snapshot {digest:032x}")]
    InspectSnapshotSeals {
        digest: u128,
        #[source]
        source: nix::Error,
    },
    #[error("boot asset snapshot {digest:032x} lacks required seals {expected:#x}; got {actual:#x}")]
    MissingSnapshotSeals { digest: u128, expected: i32, actual: i32 },
    #[error("inspect descriptor flags on boot asset snapshot {digest:032x}")]
    InspectSnapshotDescriptor {
        digest: u128,
        #[source]
        source: nix::Error,
    },
    #[error("inspect metadata on boot asset snapshot {digest:032x}")]
    InspectSnapshotMetadata {
        digest: u128,
        #[source]
        source: nix::Error,
    },
    #[error(
        "boot asset snapshot {digest:032x} has fd flags {descriptor_flags:#x}, kind {kind:#o}, mode {mode:#o}, length {length}; expected CLOEXEC, regular mode 0400, length {expected_length}"
    )]
    InvalidSnapshotMetadata {
        digest: u128,
        descriptor_flags: i32,
        kind: u32,
        mode: u32,
        length: u64,
        expected_length: u64,
    },
}

#[cfg(test)]
#[path = "asset_snapshots_tests.rs"]
mod tests;
