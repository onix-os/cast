//! Canonical normalization and hashing for a checked-out Git tree.

use std::{
    collections::TryReserveError,
    ffi::{CStr, CString, OsStr},
    fs::Permissions,
    io::{self, Read},
    mem::{size_of, zeroed},
    os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use fs_err as fs;
use nix::{errno::Errno, libc};
use sha2::{Digest, Sha256};
use thiserror::Error;

const DOMAIN: &[u8] = b"os-tools-git-materialization-v1\0";
const DIRECTORY_TAG: u8 = 0;
const REGULAR_TAG: u8 = 1;
const SYMLINK_TAG: u8 = 2;
const DIRECTORY_MODE: u32 = 0o755;
const EXECUTABLE_MODE: u32 = 0o755;
const REGULAR_MODE: u32 = 0o644;
const SYMLINK_MODE: u32 = 0o777;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// Finite ceilings for normalizing one exported Git checkout.
///
/// The defaults match the outer Git acquisition policy: a checkout may have
/// at most four million entries and 64 GiB of regular-file data. Additional
/// path and symlink aggregates bound the in-memory canonical tree retained
/// between the audit, normalization, and confirmation passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MaterializationLimits {
    pub max_entries: u64,
    pub max_depth: usize,
    pub max_name_bytes: usize,
    pub max_path_bytes: usize,
    pub max_symlink_target_bytes: usize,
    pub max_total_name_bytes: u64,
    pub max_total_path_bytes: u64,
    pub max_total_symlink_target_bytes: u64,
    pub max_file_bytes: u64,
    pub max_total_regular_bytes: u64,
    pub max_duration: Duration,
}

impl Default for MaterializationLimits {
    fn default() -> Self {
        Self {
            max_entries: 4_000_000,
            max_depth: 256,
            max_name_bytes: 4 * 1024,
            max_path_bytes: 64 * 1024,
            max_symlink_target_bytes: 64 * 1024,
            // Two complete snapshots coexist while a pass is compared with
            // its predecessor. Keep retained attacker-controlled bytes well
            // below the outer 16-GiB Git address-space policy even at four
            // million entries.
            max_total_name_bytes: 512 * MIB,
            max_total_path_bytes: GIB,
            max_total_symlink_target_bytes: 256 * MIB,
            max_file_bytes: 64 * GIB,
            max_total_regular_bytes: 64 * GIB,
            max_duration: Duration::from_secs(30 * 60),
        }
    }
}

/// Normalize one exported Git tree and return its canonical SHA-256 digest.
///
/// The initial scan is deliberately separate from mutation: a hard link or
/// special inode anywhere in the tree rejects the complete export before a
/// mode or timestamp is changed. The final scan is the sole source of bytes
/// admitted to the digest.
pub(super) fn normalize_and_hash(root: &Path, source_date_epoch: i64) -> Result<String, Error> {
    normalize_and_hash_with_limits(root, source_date_epoch, MaterializationLimits::default())
}

/// Normalize using explicit finite limits. This is kept visible to the Git
/// parent module so policy wiring can tighten defaults without duplicating the
/// descriptor-rooted implementation.
pub(super) fn normalize_and_hash_with_limits(
    root: &Path,
    source_date_epoch: i64,
    limits: MaterializationLimits,
) -> Result<String, Error> {
    normalize_and_seal_with_limits(root, source_date_epoch, limits).map(SealedMaterialization::into_digest)
}

/// Normalize a checkout reached beneath a trusted `/proc/<pid>/fd/<fd>`
/// source-root path. The magic link is an intentional reference to a held
/// directory descriptor; the checkout itself is opened without following its
/// final component and all descendant work remains fd-anchored.
pub(super) fn normalize_and_hash_descriptor_path(root: &Path, source_date_epoch: i64) -> Result<String, Error> {
    normalize_and_hash_descriptor_path_with_limits(root, source_date_epoch, MaterializationLimits::default())
}

pub(super) fn normalize_and_hash_descriptor_path_with_limits(
    root: &Path,
    source_date_epoch: i64,
    limits: MaterializationLimits,
) -> Result<String, Error> {
    normalize_and_seal_descriptor_path_with_limits(root, source_date_epoch, limits)
        .map(SealedMaterialization::into_digest)
}

/// A normalized checkout whose directory inode remains pinned until the
/// caller has atomically installed and reverified it.
///
/// Keeping this capability across `renameat2(RENAME_NOREPLACE)` closes the gap
/// between hashing a staging path and trusting its final public name. The
/// post-install verification uses the same absolute deadline created before
/// the initial audit; repeated passes never reset the time budget.
pub(super) struct SealedMaterialization {
    root: RootHandle,
    digest: String,
    digest_bytes: [u8; 32],
    entries: Vec<Entry>,
    source_date_epoch: i64,
    limits: MaterializationLimits,
    deadline: Deadline,
}

impl SealedMaterialization {
    pub(super) fn digest(&self) -> &str {
        &self.digest
    }

    fn into_digest(self) -> String {
        self.digest
    }

    /// Verify that a normal final path names the exact normalized inode held
    /// by this proof and that its complete canonical digest is unchanged.
    #[cfg(test)]
    pub(super) fn verify_installed(mut self, installed: &Path) -> Result<(), Error> {
        self.verify_installed_with_access(installed, false)
    }

    /// Verify a final path while permitting an intentional held-fd magic link
    /// in an ancestor component of `installed`.
    pub(super) fn verify_installed_descriptor_path(mut self, installed: &Path) -> Result<(), Error> {
        self.verify_installed_with_access(installed, true)
    }

    fn verify_installed_with_access(&mut self, installed: &Path, descriptor_path: bool) -> Result<(), Error> {
        self.deadline.check(installed)?;
        self.root.retarget(installed, descriptor_path)?;
        self.deadline.check(&self.root.path)?;

        let first = scan_tree(&self.root, true, self.limits, &self.deadline, Some(&self.entries))?;
        // Only the first post-install scan needs the pre-rename symlink target
        // witnesses. Release that potentially large snapshot before retaining
        // another complete confirmation tree.
        drop(std::mem::take(&mut self.entries));
        verify_normalized_tree(&self.root, &first, self.source_date_epoch, &self.deadline)?;
        let first_digest = hash_tree(&self.root, &first, self.limits, &self.deadline)?;
        if first_digest != self.digest_bytes {
            return Err(Error::TreeChanged);
        }

        let confirmed = scan_tree(&self.root, true, self.limits, &self.deadline, Some(&first))?;
        require_stable_tree(&self.root, &first, &confirmed, &self.deadline)?;
        let confirmation = hash_tree(&self.root, &confirmed, self.limits, &self.deadline)?;
        if confirmation != first_digest {
            return Err(Error::TreeChanged);
        }
        drop(first);

        let final_tree = scan_tree(&self.root, true, self.limits, &self.deadline, Some(&confirmed))?;
        require_stable_tree(&self.root, &confirmed, &final_tree, &self.deadline)?;
        verify_normalized_tree(&self.root, &final_tree, self.source_date_epoch, &self.deadline)?;
        self.root.require_path_identity()?;
        self.deadline.check(&self.root.path)
    }
}

pub(super) fn normalize_and_seal_with_limits(
    root: &Path,
    source_date_epoch: i64,
    limits: MaterializationLimits,
) -> Result<SealedMaterialization, Error> {
    normalize_and_seal_open(root, source_date_epoch, limits, false)
}

pub(super) fn normalize_and_seal_descriptor_path_with_limits(
    root: &Path,
    source_date_epoch: i64,
    limits: MaterializationLimits,
) -> Result<SealedMaterialization, Error> {
    normalize_and_seal_open(root, source_date_epoch, limits, true)
}

fn normalize_and_seal_open(
    root: &Path,
    source_date_epoch: i64,
    limits: MaterializationLimits,
    descriptor_path: bool,
) -> Result<SealedMaterialization, Error> {
    let deadline = Deadline::new(limits.max_duration);
    deadline.check(root)?;
    let root = if descriptor_path {
        RootHandle::open_descriptor_path(root)?
    } else {
        RootHandle::open(root)?
    };
    deadline.check(&root.path)?;
    let (digest, entries) = normalize_and_hash_with(&root, source_date_epoch, limits, &deadline, |_| {})?;
    let mut digest_bytes = [0_u8; 32];
    hex::decode_to_slice(&digest, &mut digest_bytes).expect("normalization emits a valid SHA-256 digest");
    Ok(SealedMaterialization {
        root,
        digest,
        digest_bytes,
        entries,
        source_date_epoch,
        limits,
        deadline,
    })
}

/// Remove every Git administration entry beneath an exported checkout with a
/// bounded, descriptor-rooted, no-symlink traversal.
///
/// The root itself is never removed even when its authored clone directory is
/// named `.git`. This helper is intended to replace the parent's path-based
/// `WalkDir` implementation once the caller is wired to its structured error.
pub(super) fn remove_git_administration_bounded(root: &Path) -> Result<(), Error> {
    remove_git_administration_with_limits(root, MaterializationLimits::default(), false)
}

pub(super) fn remove_git_administration_descriptor_path_bounded(root: &Path) -> Result<(), Error> {
    remove_git_administration_with_limits(root, MaterializationLimits::default(), true)
}

pub(super) fn remove_git_administration_with_limits(
    root: &Path,
    limits: MaterializationLimits,
    descriptor_path: bool,
) -> Result<(), Error> {
    let deadline = Deadline::new(limits.max_duration);
    deadline.check(root)?;
    let root = if descriptor_path {
        RootHandle::open_descriptor_path(root)?
    } else {
        RootHandle::open(root)?
    };
    deadline.check(&root.path)?;
    let candidates = collect_git_administration(&root, limits, &deadline)?;
    remove_admin_candidates(&root, &candidates, &deadline)?;

    // A concurrent addition must not be silently admitted after the deletion
    // pass. Staging cleanup may discard the checkout after this fail-closed
    // error, but this helper never chases or removes the raced-in target.
    if !collect_git_administration(&root, limits, &deadline)?.is_empty() {
        return Err(Error::TreeChanged);
    }
    root.require_path_identity()?;
    deadline.check(&root.path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookPoint {
    AfterAudit,
    AfterCanonicalHash,
}

fn normalize_and_hash_with(
    root: &RootHandle,
    source_date_epoch: i64,
    limits: MaterializationLimits,
    deadline: &Deadline,
    mut hook: impl FnMut(HookPoint),
) -> Result<(String, Vec<Entry>), Error> {
    let audited = scan_tree(root, false, limits, deadline, None)?;
    hook(HookPoint::AfterAudit);
    deadline.check(&root.path)?;
    normalize_entries(root, &audited, source_date_epoch, limits, deadline)?;

    // This scan must read symlink targets to prove that normalization did not
    // change semantic bytes. Reset timestamps once more, then every remaining
    // scan can reuse an unchanged symlink inode's immutable target bytes.
    let normalized = scan_tree(root, true, limits, deadline, None)?;
    require_same_tree(root, &audited, &normalized, deadline)?;
    normalize_entries(root, &normalized, source_date_epoch, limits, deadline)?;
    drop(audited);

    let stable = scan_tree(root, true, limits, deadline, Some(&normalized))?;
    require_same_tree(root, &normalized, &stable, deadline)?;
    drop(normalized);
    let digest = hash_tree(root, &stable, limits, deadline)?;
    hook(HookPoint::AfterCanonicalHash);
    deadline.check(&root.path)?;

    // Re-enumerate the complete tree and hash it again. The scan catches an
    // added or removed path; the second digest catches same-length content
    // replacement even on a filesystem with coarse metadata timestamps.
    let confirmed = scan_tree(root, true, limits, deadline, Some(&stable))?;
    require_same_tree(root, &stable, &confirmed, deadline)?;
    let confirmation = hash_tree(root, &confirmed, limits, deadline)?;
    if confirmation != digest {
        return Err(Error::TreeChanged);
    }
    require_stable_tree(root, &stable, &confirmed, deadline)?;
    drop(stable);

    // Hashing and enumeration use O_NOATIME, so this final full rescan needs no
    // metadata repair afterward. Exact metadata equality detects a mutation
    // that raced the confirmation hash, and reopening the root proves that its
    // public path still names the directory held by our root descriptor.
    let final_tree = scan_tree(root, true, limits, deadline, Some(&confirmed))?;
    require_stable_tree(root, &confirmed, &final_tree, deadline)?;
    drop(confirmed);
    verify_normalized_tree(root, &final_tree, source_date_epoch, deadline)?;
    root.require_path_identity()?;
    deadline.check(&root.path)?;

    Ok((hex::encode(digest), final_tree))
}

include!("materialization/descriptor_tree.rs");
include!("materialization/tree_scanning.rs");
include!("materialization/normalization_hashing.rs");

#[derive(Debug, Error)]
pub enum Error {
    #[error("Git materialization root is not a directory: {0:?}")]
    RootNotDirectory(PathBuf),
    #[error("invalid Git materialization path {path:?}: {detail}")]
    InvalidPath { path: PathBuf, detail: &'static str },
    #[error("Git materialization entry {path:?} has unsupported type {kind}")]
    UnsupportedFileType { path: PathBuf, kind: &'static str },
    #[error("Git materialization non-directory {path:?} has link count {links}; expected exactly one")]
    UnexpectedLinkCount { path: PathBuf, links: u64 },
    #[error("Git materialization entry {0:?} changed during normalization or hashing")]
    EntryChanged(PathBuf),
    #[error("Git materialization tree changed during normalization or hashing")]
    TreeChanged,
    #[error("Git materialization tree contains duplicate path {0:?}")]
    DuplicatePath(PathBuf),
    #[error("Git materialization {resource} {actual} exceeds limit {limit} at {path:?}")]
    LimitExceeded {
        resource: &'static str,
        limit: u64,
        actual: u64,
        path: PathBuf,
    },
    #[error("Git materialization exceeded {limit:?} while processing {path:?}")]
    DurationExceeded { path: PathBuf, limit: Duration },
    #[error("Git materialization arithmetic overflow for {resource} at {path:?}")]
    ArithmeticOverflow { resource: &'static str, path: PathBuf },
    #[error("failed to reserve {requested} units for Git materialization {resource}: {source}")]
    Allocation {
        resource: &'static str,
        requested: usize,
        #[source]
        source: TryReserveError,
    },
    #[error("Git materialization {field} length for {path:?} cannot be represented as u64")]
    LengthNotRepresentable { field: &'static str, path: PathBuf },
    #[error("Git materialization entry {path:?} has mode {actual:#06o}; expected {expected:#06o}")]
    ModeNotNormalized { path: PathBuf, expected: u32, actual: u32 },
    #[error("Git materialization file {path:?} changed length while hashing (expected {expected}, found {actual})")]
    FileLengthChanged { path: PathBuf, expected: u64, actual: u64 },
    #[error("Git materialization file {0:?} changed while its bytes were being hashed")]
    FileChangedDuringHash(PathBuf),
    #[error(
        "Git materialization entry {path:?} did not retain timestamp {expected}.0 (atime={atime}.{atime_nsec}, mtime={mtime}.{mtime_nsec})"
    )]
    TimestampNotNormalized {
        path: PathBuf,
        expected: i64,
        atime: i64,
        atime_nsec: i64,
        mtime: i64,
        mtime_nsec: i64,
    },
    #[error("{operation} at {path:?}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests;
