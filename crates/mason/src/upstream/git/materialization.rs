// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

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
    pub(super) fn verify_installed(mut self, installed: &Path) -> Result<(), Error> {
        self.verify_installed_with_access(installed, false)
    }

    /// As [`Self::verify_installed`], but permit an intentional held-fd magic
    /// link in an ancestor component of `installed`.
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

struct RootHandle {
    path: PathBuf,
    file: fs::File,
    identity: Identity,
    descriptor_path: bool,
}

impl RootHandle {
    fn open(path: &Path) -> Result<Self, Error> {
        let path = std::path::absolute(path).map_err(|source| Error::Io {
            operation: "make Git materialization root absolute",
            path: path.to_owned(),
            source,
        })?;
        let file = openat2_file(
            libc::AT_FDCWD,
            path.as_os_str().as_bytes(),
            data_open_flags(true),
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
        )
        .map(|file| fs::File::from_parts(file, &path))
        .map_err(|source| Error::Io {
            operation: "open Git materialization root without symlinks",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| Error::Io {
            operation: "inspect opened Git materialization root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(Error::RootNotDirectory(path));
        }
        Ok(Self {
            path,
            identity: Identity::from_metadata(&metadata),
            file,
            descriptor_path: false,
        })
    }

    fn open_descriptor_path(path: &Path) -> Result<Self, Error> {
        let path = std::path::absolute(path).map_err(|source| Error::Io {
            operation: "make descriptor-rooted Git materialization path absolute",
            path: path.to_owned(),
            source,
        })?;
        let file = open_path_file(&path, data_open_flags(true))
            .map(|file| fs::File::from_parts(file, &path))
            .map_err(|source| Error::Io {
                operation: "open descriptor-rooted Git materialization root",
                path: path.clone(),
                source,
            })?;
        let metadata = file.metadata().map_err(|source| Error::Io {
            operation: "inspect descriptor-rooted Git materialization root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(Error::RootNotDirectory(path));
        }
        Ok(Self {
            path,
            identity: Identity::from_metadata(&metadata),
            file,
            descriptor_path: true,
        })
    }

    fn display_path(&self, relative: &[u8]) -> PathBuf {
        if relative.is_empty() {
            self.path.clone()
        } else {
            self.path.join(OsStr::from_bytes(relative))
        }
    }

    fn open_relative(&self, relative: &[u8], flags: i32, operation: &'static str) -> Result<fs::File, Error> {
        let path = self.display_path(relative);
        // A duplicated directory descriptor shares its directory-stream
        // offset with the original open file description. Open `.` afresh for
        // the root so every full-tree scan begins at offset zero.
        let relative = if relative.is_empty() { b".".as_slice() } else { relative };
        openat2_file(
            self.file.as_raw_fd(),
            relative,
            flags,
            libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV,
        )
        .map(|file| fs::File::from_parts(file, &path))
        .map_err(|source| Error::Io {
            operation,
            path,
            source,
        })
    }

    fn inspect_entry(
        &self,
        relative: Vec<u8>,
        context: &mut ScanContext<'_>,
        expected: Option<&[Entry]>,
    ) -> Result<Entry, Error> {
        context.check_time(&self.display_path(&relative))?;
        let path = self.display_path(&relative);
        let file = self.open_relative(
            &relative,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            "open Git materialization entry for inspection",
        )?;
        let metadata = file.metadata().map_err(|source| Error::Io {
            operation: "inspect opened Git materialization entry",
            path: path.clone(),
            source,
        })?;
        let identity = Identity::from_metadata(&metadata);
        let kind = classify_handle(&path, &file, &metadata, &relative, identity, expected, context)?;
        require_single_link(&path, &metadata, &kind)?;
        context.admit_kind(&kind, &path)?;
        Ok(Entry {
            relative,
            identity,
            kind,
            stamp: MetadataStamp::from_metadata(&metadata),
        })
    }

    fn open_data(&self, entry: &Entry) -> Result<fs::File, Error> {
        self.open_relative(
            &entry.relative,
            data_open_flags(entry.kind.is_directory()),
            "open Git materialization entry beneath root",
        )
    }

    fn open_inspection(&self, entry: &Entry) -> Result<fs::File, Error> {
        self.open_relative(
            &entry.relative,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            "reopen Git materialization entry for inspection",
        )
    }

    fn retarget(&mut self, path: &Path, descriptor_path: bool) -> Result<(), Error> {
        let path = std::path::absolute(path).map_err(|source| Error::Io {
            operation: "make installed Git materialization path absolute",
            path: path.to_owned(),
            source,
        })?;
        let reopened = if descriptor_path {
            open_path_file(&path, data_open_flags(true))
        } else {
            openat2_file(
                libc::AT_FDCWD,
                path.as_os_str().as_bytes(),
                data_open_flags(true),
                libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            )
        }
        .map_err(|source| Error::Io {
            operation: "open installed Git materialization root without symlinks",
            path: path.clone(),
            source,
        })?;
        let metadata = reopened.metadata().map_err(|source| Error::Io {
            operation: "inspect installed Git materialization root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() || Identity::from_metadata(&metadata) != self.identity {
            return Err(Error::EntryChanged(path));
        }
        self.path = path;
        self.descriptor_path = descriptor_path;
        Ok(())
    }

    fn require_path_identity(&self) -> Result<(), Error> {
        let reopened = if self.descriptor_path {
            open_path_file(&self.path, data_open_flags(true))
        } else {
            openat2_file(
                libc::AT_FDCWD,
                self.path.as_os_str().as_bytes(),
                data_open_flags(true),
                libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            )
        }
        .map_err(|source| Error::Io {
            operation: "reopen Git materialization root without symlinks",
            path: self.path.clone(),
            source,
        })?;
        let metadata = reopened.metadata().map_err(|source| Error::Io {
            operation: "verify Git materialization root path",
            path: self.path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() || Identity::from_metadata(&metadata) != self.identity {
            return Err(Error::EntryChanged(self.path.clone()));
        }
        Ok(())
    }
}

fn open_path_file(path: &Path, flags: i32) -> io::Result<std::fs::File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: `path` is NUL-terminated and a successful `open` returns a new
    // descriptor. O_NOFOLLOW applies to the checkout itself while permitting
    // the intentional held-fd magic link in an ancestor component.
    let result = unsafe { libc::open(path.as_ptr(), flags) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: successful open returned a fresh owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(result) }.into())
    }
}

fn data_open_flags(directory: bool) -> i32 {
    let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOATIME;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    flags
}

fn openat2_file(dirfd: RawFd, path: &[u8], flags: i32, resolve: u64) -> io::Result<std::fs::File> {
    let path = CString::new(path).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: every field of Linux `open_how` accepts zero, after which the
    // public fields used by this ABI version are initialized explicitly.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = 0;
    how.resolve = resolve;
    // SAFETY: `path` is NUL-terminated, `how` points to an initialized
    // `open_how`, and a successful syscall returns a new owned descriptor.
    let result = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            size_of::<libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat2 returns a fresh descriptor owned by us.
    let descriptor = unsafe { OwnedFd::from_raw_fd(result as RawFd) };
    Ok(descriptor.into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    relative: Vec<u8>,
    identity: Identity,
    kind: EntryKind,
    stamp: MetadataStamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MetadataStamp {
    mode: u32,
    links: u64,
    atime: i64,
    atime_nsec: i64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

impl MetadataStamp {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            mode: metadata.mode() & 0o7777,
            links: metadata.nlink(),
            atime: metadata.atime(),
            atime_nsec: metadata.atime_nsec(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryKind {
    Directory,
    Regular { executable: bool, length: u64 },
    Symlink { target: Vec<u8> },
}

impl EntryKind {
    fn tag(&self) -> u8 {
        match self {
            Self::Directory => DIRECTORY_TAG,
            Self::Regular { .. } => REGULAR_TAG,
            Self::Symlink { .. } => SYMLINK_TAG,
        }
    }

    fn normalized_mode(&self) -> u32 {
        match self {
            Self::Directory => DIRECTORY_MODE,
            Self::Regular { executable: true, .. } => EXECUTABLE_MODE,
            Self::Regular { executable: false, .. } => REGULAR_MODE,
            Self::Symlink { .. } => SYMLINK_MODE,
        }
    }

    fn is_directory(&self) -> bool {
        matches!(self, Self::Directory)
    }
}

#[derive(Debug, Default)]
struct ScanUsage {
    entries: u64,
    name_bytes: u64,
    path_bytes: u64,
    symlink_target_bytes: u64,
    regular_bytes: u64,
}

struct ScanContext<'a> {
    limits: MaterializationLimits,
    deadline: &'a Deadline,
    usage: ScanUsage,
}

impl<'a> ScanContext<'a> {
    fn new(limits: MaterializationLimits, deadline: &'a Deadline) -> Self {
        Self {
            limits,
            deadline,
            usage: ScanUsage::default(),
        }
    }

    fn check_time(&self, path: &Path) -> Result<(), Error> {
        self.deadline.check(path)
    }

    fn admit_entry_bytes(
        &mut self,
        name_bytes: usize,
        path_bytes: usize,
        depth: usize,
        path: &Path,
    ) -> Result<(), Error> {
        self.check_time(path)?;
        enforce_usize_limit("entry depth", self.limits.max_depth, depth, path)?;
        enforce_usize_limit("entry name bytes", self.limits.max_name_bytes, name_bytes, path)?;
        enforce_usize_limit("relative path bytes", self.limits.max_path_bytes, path_bytes, path)?;
        self.usage.entries = checked_add_limit("total entries", self.usage.entries, 1, self.limits.max_entries, path)?;
        self.usage.name_bytes = checked_add_limit(
            "total entry name bytes",
            self.usage.name_bytes,
            usize_to_u64(name_bytes, "total entry name bytes", path)?,
            self.limits.max_total_name_bytes,
            path,
        )?;
        self.usage.path_bytes = checked_add_limit(
            "total relative path bytes",
            self.usage.path_bytes,
            usize_to_u64(path_bytes, "total relative path bytes", path)?,
            self.limits.max_total_path_bytes,
            path,
        )?;
        Ok(())
    }

    fn admit_kind(&mut self, kind: &EntryKind, path: &Path) -> Result<(), Error> {
        self.check_time(path)?;
        match kind {
            EntryKind::Regular { length, .. } => {
                enforce_u64_limit("regular file bytes", self.limits.max_file_bytes, *length, path)?;
                self.usage.regular_bytes = checked_add_limit(
                    "total regular file bytes",
                    self.usage.regular_bytes,
                    *length,
                    self.limits.max_total_regular_bytes,
                    path,
                )?;
            }
            EntryKind::Symlink { target } => {
                enforce_usize_limit(
                    "symlink target bytes",
                    self.limits.max_symlink_target_bytes,
                    target.len(),
                    path,
                )?;
                self.usage.symlink_target_bytes = checked_add_limit(
                    "total symlink target bytes",
                    self.usage.symlink_target_bytes,
                    usize_to_u64(target.len(), "total symlink target bytes", path)?,
                    self.limits.max_total_symlink_target_bytes,
                    path,
                )?;
            }
            EntryKind::Directory => {}
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Deadline {
    started: Instant,
    limit: Duration,
}

impl Deadline {
    fn new(limit: Duration) -> Self {
        Self {
            started: Instant::now(),
            limit,
        }
    }

    fn check(&self, path: &Path) -> Result<(), Error> {
        if self.started.elapsed() >= self.limit {
            Err(Error::DurationExceeded {
                path: path.to_owned(),
                limit: self.limit,
            })
        } else {
            Ok(())
        }
    }
}

/// Enumerate without retaining directory descriptors. `directories` contains
/// only indices into `entries`; each directory stream is closed before any
/// child is opened, so traversal descriptor use is constant rather than
/// proportional to attacker-controlled depth.
fn scan_tree(
    root: &RootHandle,
    require_normalized_modes: bool,
    limits: MaterializationLimits,
    deadline: &Deadline,
    expected: Option<&[Entry]>,
) -> Result<Vec<Entry>, Error> {
    let mut context = ScanContext::new(limits, deadline);
    let root_entry = root.inspect_entry(Vec::new(), &mut context, expected)?;
    let mut entries = Vec::new();
    reserve(&mut entries, 1, "materialization entries")?;
    entries.push(root_entry);
    let mut directories = Vec::new();
    reserve(&mut directories, 1, "pending materialization directories")?;
    directories.push((0_usize, 0_usize));

    while let Some((directory_index, directory_depth)) = directories.pop() {
        let names = {
            let directory = &entries[directory_index];
            read_directory_names(root, directory, directory_depth, require_normalized_modes, &mut context)?
        };
        reserve(&mut entries, names.len(), "materialization entries")?;
        reserve(&mut directories, names.len(), "pending materialization directories")?;
        for name in names {
            let directory = &entries[directory_index];
            let path = root.display_path(&directory.relative).join(OsStr::from_bytes(&name));
            let relative = join_relative(&directory.relative, &name, &path)?;
            let entry = root.inspect_entry(relative, &mut context, expected)?;
            if require_normalized_modes {
                let entry_path = root.display_path(&entry.relative);
                let metadata = root.open_inspection(&entry)?.metadata().map_err(|source| Error::Io {
                    operation: "verify Git materialization entry mode",
                    path: entry_path.clone(),
                    source,
                })?;
                require_normalized_mode(&entry_path, &metadata, &entry.kind)?;
            }
            let is_directory = entry.kind.is_directory();
            let index = entries.len();
            entries.push(entry);
            if is_directory {
                let child_depth = directory_depth.checked_add(1).ok_or(Error::ArithmeticOverflow {
                    resource: "entry depth",
                    path,
                })?;
                directories.push((index, child_depth));
            }
        }
    }

    deadline.check(&root.path)?;
    entries.sort_unstable_by(|left, right| left.relative.cmp(&right.relative));
    deadline.check(&root.path)?;
    for adjacent in entries.windows(2) {
        deadline.check(&root.display_path(&adjacent[1].relative))?;
        if adjacent[0].relative == adjacent[1].relative {
            return Err(Error::DuplicatePath(root.display_path(&adjacent[0].relative)));
        }
    }
    Ok(entries)
}

#[derive(Debug)]
struct AdministrationEntry {
    relative: Vec<u8>,
    identity: Identity,
    stamp: MetadataStamp,
    directory: bool,
    remove: bool,
}

/// Discover administration entries without following symlinks or retaining a
/// descriptor stack. Each pending directory is represented by an index into a
/// bounded vector, so even a depth-256 tree keeps only the root plus one
/// transient directory stream live.
fn collect_git_administration(
    root: &RootHandle,
    limits: MaterializationLimits,
    deadline: &Deadline,
) -> Result<Vec<AdministrationEntry>, Error> {
    let root_metadata = root.file.metadata().map_err(|source| Error::Io {
        operation: "inspect Git administration removal root",
        path: root.path.clone(),
        source,
    })?;
    let mut nodes = Vec::new();
    reserve(&mut nodes, 1, "Git administration traversal entries")?;
    nodes.push(AdministrationEntry {
        relative: Vec::new(),
        identity: Identity::from_metadata(&root_metadata),
        stamp: MetadataStamp::from_metadata(&root_metadata),
        directory: true,
        remove: false,
    });
    let mut directories = Vec::new();
    reserve(&mut directories, 1, "pending Git administration directories")?;
    directories.push((0_usize, 0_usize));
    let mut context = ScanContext::new(limits, deadline);
    let mut candidate_count = 0_usize;

    while let Some((directory_index, directory_depth)) = directories.pop() {
        let names = {
            let directory = &nodes[directory_index];
            read_administration_directory_names(root, directory, directory_depth, &mut context)?
        };
        reserve(&mut nodes, names.len(), "Git administration traversal entries")?;
        reserve(&mut directories, names.len(), "pending Git administration directories")?;
        for name in names {
            let parent = &nodes[directory_index];
            let path = root.display_path(&parent.relative).join(OsStr::from_bytes(&name));
            let relative = join_relative(&parent.relative, &name, &path)?;
            let remove = parent.remove || name == b".git";
            let node = inspect_administration_entry(root, relative, remove, &mut context)?;
            let directory = node.directory;
            let index = nodes.len();
            nodes.push(node);
            if remove {
                candidate_count = candidate_count
                    .checked_add(1)
                    .ok_or_else(|| Error::ArithmeticOverflow {
                        resource: "Git administration entries",
                        path: path.clone(),
                    })?;
            }
            if directory {
                let child_depth = directory_depth.checked_add(1).ok_or(Error::ArithmeticOverflow {
                    resource: "entry depth",
                    path,
                })?;
                directories.push((index, child_depth));
            }
        }
    }

    let mut candidates = Vec::new();
    reserve(&mut candidates, candidate_count, "Git administration removal entries")?;
    for node in nodes {
        if node.remove {
            candidates.push(node);
        }
    }
    deadline.check(&root.path)?;
    Ok(candidates)
}

fn inspect_administration_entry(
    root: &RootHandle,
    relative: Vec<u8>,
    remove: bool,
    context: &mut ScanContext<'_>,
) -> Result<AdministrationEntry, Error> {
    let path = root.display_path(&relative);
    context.check_time(&path)?;
    let file = root.open_relative(
        &relative,
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        "open Git administration traversal entry",
    )?;
    let metadata = file.metadata().map_err(|source| Error::Io {
        operation: "inspect Git administration traversal entry",
        path: path.clone(),
        source,
    })?;
    let file_type = metadata.file_type();
    let directory = file_type.is_dir();
    if file_type.is_file() {
        context.admit_kind(
            &EntryKind::Regular {
                executable: metadata.mode() & 0o111 != 0,
                length: metadata.len(),
            },
            &path,
        )?;
    } else if file_type.is_symlink() {
        let target = read_symlink_handle(&file, &path, context.limits, context.deadline)?;
        context.admit_kind(&EntryKind::Symlink { target }, &path)?;
    }
    context.check_time(&path)?;
    Ok(AdministrationEntry {
        relative,
        identity: Identity::from_metadata(&metadata),
        stamp: MetadataStamp::from_metadata(&metadata),
        directory,
        remove,
    })
}

fn read_administration_directory_names(
    root: &RootHandle,
    directory: &AdministrationEntry,
    directory_depth: usize,
    context: &mut ScanContext<'_>,
) -> Result<Vec<Vec<u8>>, Error> {
    let path = root.display_path(&directory.relative);
    context.check_time(&path)?;
    let file = root.open_relative(
        &directory.relative,
        data_open_flags(true),
        "open Git administration traversal directory",
    )?;
    require_administration_directory(directory, &file, &path)?;
    let mut names =
        DirectoryStream::from_file(file, &path)?.names(root, &directory.relative, directory_depth, context)?;
    context.check_time(&path)?;
    names.sort_unstable();
    context.check_time(&path)?;
    let reopened = root.open_relative(
        &directory.relative,
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        "reopen Git administration traversal directory",
    )?;
    require_administration_directory(directory, &reopened, &path)?;
    Ok(names)
}

fn require_administration_directory(
    directory: &AdministrationEntry,
    file: &fs::File,
    path: &Path,
) -> Result<(), Error> {
    let metadata = file.metadata().map_err(|source| Error::Io {
        operation: "verify Git administration traversal directory",
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_dir()
        || Identity::from_metadata(&metadata) != directory.identity
        || MetadataStamp::from_metadata(&metadata) != directory.stamp
    {
        Err(Error::EntryChanged(path.to_owned()))
    } else {
        Ok(())
    }
}

fn remove_admin_candidates(
    root: &RootHandle,
    candidates: &[AdministrationEntry],
    deadline: &Deadline,
) -> Result<(), Error> {
    for candidate in candidates.iter().rev() {
        let path = root.display_path(&candidate.relative);
        deadline.check(&path)?;
        let (parent_relative, name) = split_relative(&candidate.relative).ok_or_else(|| Error::InvalidPath {
            path: path.clone(),
            detail: "administration candidate has no file name",
        })?;
        let parent = root.open_relative(
            parent_relative,
            data_open_flags(true),
            "open Git administration entry parent",
        )?;
        let current = openat2_file(
            parent.as_raw_fd(),
            name,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV,
        )
        .map_err(|source| Error::Io {
            operation: "reopen Git administration entry before removal",
            path: path.clone(),
            source,
        })?;
        let metadata = current.metadata().map_err(|source| Error::Io {
            operation: "inspect Git administration entry before removal",
            path: path.clone(),
            source,
        })?;
        if Identity::from_metadata(&metadata) != candidate.identity
            || metadata.file_type().is_dir() != candidate.directory
        {
            return Err(Error::EntryChanged(path));
        }
        let name = CString::new(name).map_err(|_| Error::InvalidPath {
            path: path.clone(),
            detail: "administration entry name contains NUL",
        })?;
        let flags = if candidate.directory { libc::AT_REMOVEDIR } else { 0 };
        // SAFETY: the parent descriptor and NUL-terminated single-component
        // name are live. `unlinkat` never follows a symlink in the final
        // component; directories require the explicit AT_REMOVEDIR flag.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) } == -1 {
            return Err(Error::Io {
                operation: "remove Git administration entry",
                path,
                source: io::Error::last_os_error(),
            });
        }
        deadline.check(&root.path)?;
    }
    Ok(())
}

fn split_relative(relative: &[u8]) -> Option<(&[u8], &[u8])> {
    if relative.is_empty() {
        return None;
    }
    match relative.iter().rposition(|byte| *byte == b'/') {
        Some(separator) => Some((&relative[..separator], &relative[separator + 1..])),
        None => Some((b"", relative)),
    }
}

struct DirectoryStream(std::ptr::NonNull<libc::DIR>);

impl DirectoryStream {
    fn from_file(file: fs::File, path: &Path) -> Result<Self, Error> {
        let descriptor = file.into_raw_fd();
        // SAFETY: `descriptor` is an owned directory descriptor. On success
        // fdopendir takes ownership; on failure we close it below.
        let stream = unsafe { libc::fdopendir(descriptor) };
        match std::ptr::NonNull::new(stream) {
            Some(stream) => Ok(Self(stream)),
            None => {
                let source = io::Error::last_os_error();
                // SAFETY: fdopendir failed and therefore did not consume the
                // descriptor.
                unsafe { libc::close(descriptor) };
                Err(Error::Io {
                    operation: "open Git materialization directory stream",
                    path: path.to_owned(),
                    source,
                })
            }
        }
    }

    fn names(
        &mut self,
        root: &RootHandle,
        directory_relative: &[u8],
        directory_depth: usize,
        context: &mut ScanContext<'_>,
    ) -> Result<Vec<Vec<u8>>, Error> {
        let directory_path = root.display_path(directory_relative);
        let mut names = Vec::new();
        loop {
            context.check_time(&directory_path)?;
            Errno::clear();
            // SAFETY: the DIR pointer is live and exclusively borrowed for
            // this iteration.
            let entry = unsafe { libc::readdir64(self.0.as_ptr()) };
            if entry.is_null() {
                let error = Errno::last();
                if error == Errno::UnknownErrno {
                    return Ok(names);
                }
                return Err(Error::Io {
                    operation: "read Git materialization directory",
                    path: directory_path,
                    source: io::Error::from_raw_os_error(error as i32),
                });
            }
            // SAFETY: readdir64 returned a live dirent whose d_name is
            // NUL-terminated for the duration of this loop iteration.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                let separator = usize::from(!directory_relative.is_empty());
                let relative_length = directory_relative
                    .len()
                    .checked_add(separator)
                    .and_then(|length| length.checked_add(name.len()))
                    .ok_or_else(|| Error::ArithmeticOverflow {
                        resource: "relative path bytes",
                        path: directory_path.clone(),
                    })?;
                let depth = directory_depth
                    .checked_add(1)
                    .ok_or_else(|| Error::ArithmeticOverflow {
                        resource: "entry depth",
                        path: directory_path.clone(),
                    })?;
                let path = directory_path.join(OsStr::from_bytes(name));
                context.admit_entry_bytes(name.len(), relative_length, depth, &path)?;
                reserve(&mut names, 1, "materialization directory names")?;
                names.push(copy_bytes(name, "materialization entry name")?);
            }
        }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the stream returned by fdopendir.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}

fn read_directory_names(
    root: &RootHandle,
    directory: &Entry,
    directory_depth: usize,
    require_normalized_modes: bool,
    context: &mut ScanContext<'_>,
) -> Result<Vec<Vec<u8>>, Error> {
    let path = root.display_path(&directory.relative);
    context.check_time(&path)?;
    let file = root.open_data(directory)?;
    let metadata = file.metadata().map_err(|source| Error::Io {
        operation: "inspect opened Git materialization directory",
        path: path.clone(),
        source,
    })?;
    require_handle_matches(directory, &metadata, require_normalized_modes, &path)?;
    require_stamp(directory, &metadata, &path)?;
    let mut names =
        DirectoryStream::from_file(file, &path)?.names(root, &directory.relative, directory_depth, context)?;
    context.check_time(&path)?;
    names.sort_unstable();
    context.check_time(&path)?;

    // Reopen through the pinned root after enumeration. The stamp comparison
    // catches additions/removals in the same directory inode, while identity
    // comparison rejects a path replacement.
    let reopened = root.open_inspection(directory)?;
    let metadata = reopened.metadata().map_err(|source| Error::Io {
        operation: "verify enumerated Git materialization directory",
        path: path.clone(),
        source,
    })?;
    require_handle_matches(directory, &metadata, require_normalized_modes, &path)?;
    require_stamp(directory, &metadata, &path)?;
    Ok(names)
}

fn classify_handle(
    path: &Path,
    file: &fs::File,
    metadata: &std::fs::Metadata,
    relative: &[u8],
    identity: Identity,
    expected: Option<&[Entry]>,
    context: &ScanContext<'_>,
) -> Result<EntryKind, Error> {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        Ok(EntryKind::Directory)
    } else if file_type.is_file() {
        Ok(EntryKind::Regular {
            executable: metadata.mode() & 0o111 != 0,
            length: metadata.len(),
        })
    } else if file_type.is_symlink() {
        let reused = expected
            .and_then(|entries| {
                entries
                    .binary_search_by(|entry| entry.relative.as_slice().cmp(relative))
                    .ok()
                    .map(|index| &entries[index])
            })
            .and_then(|entry| (entry.identity == identity).then_some(&entry.kind))
            .and_then(|kind| match kind {
                EntryKind::Symlink { target } => Some(target.as_slice()),
                _ => None,
            });
        let target = match reused {
            Some(target) => copy_bytes(target, "symlink target bytes")?,
            None => read_symlink_handle(file, path, context.limits, context.deadline)?,
        };
        Ok(EntryKind::Symlink { target })
    } else {
        Err(Error::UnsupportedFileType {
            path: path.to_owned(),
            kind: special_file_type(&file_type),
        })
    }
}

fn read_symlink_handle(
    file: &fs::File,
    path: &Path,
    limits: MaterializationLimits,
    deadline: &Deadline,
) -> Result<Vec<u8>, Error> {
    deadline.check(path)?;
    let capacity = limits
        .max_symlink_target_bytes
        .checked_add(1)
        .ok_or_else(|| Error::ArithmeticOverflow {
            resource: "symlink target bytes",
            path: path.to_owned(),
        })?;
    let mut target = Vec::new();
    target.try_reserve_exact(capacity).map_err(|source| Error::Allocation {
        resource: "symlink target bytes",
        requested: capacity,
        source,
    })?;
    target.resize(capacity, 0);
    // Linux readlinkat with an empty path reads the exact symlink pinned by
    // the O_PATH|O_NOFOLLOW descriptor. The extra byte distinguishes an exact
    // limit-sized target from a truncated over-limit target.
    // SAFETY: the descriptor is live and `target` exposes `capacity` writable
    // bytes for the duration of the syscall.
    let read = unsafe { libc::readlinkat(file.as_raw_fd(), c"".as_ptr(), target.as_mut_ptr().cast(), target.len()) };
    if read == -1 {
        return Err(Error::Io {
            operation: "read opened Git materialization symlink",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let read = usize::try_from(read).map_err(|_| Error::ArithmeticOverflow {
        resource: "symlink target bytes",
        path: path.to_owned(),
    })?;
    enforce_usize_limit("symlink target bytes", limits.max_symlink_target_bytes, read, path)?;
    target.truncate(read);
    deadline.check(path)?;
    Ok(target)
}

fn special_file_type(file_type: &std::fs::FileType) -> &'static str {
    if file_type.is_fifo() {
        "FIFO"
    } else if file_type.is_socket() {
        "socket"
    } else if file_type.is_block_device() {
        "block device"
    } else if file_type.is_char_device() {
        "character device"
    } else {
        "unknown special inode"
    }
}

fn require_single_link(path: &Path, metadata: &std::fs::Metadata, kind: &EntryKind) -> Result<(), Error> {
    if !kind.is_directory() && metadata.nlink() != 1 {
        Err(Error::UnexpectedLinkCount {
            path: path.to_owned(),
            links: metadata.nlink(),
        })
    } else {
        Ok(())
    }
}

fn require_normalized_mode(path: &Path, metadata: &std::fs::Metadata, kind: &EntryKind) -> Result<(), Error> {
    if matches!(kind, EntryKind::Symlink { .. }) {
        return Ok(());
    }
    let expected = kind.normalized_mode();
    let actual = metadata.mode() & 0o7777;
    if actual == expected {
        Ok(())
    } else {
        Err(Error::ModeNotNormalized {
            path: path.to_owned(),
            expected,
            actual,
        })
    }
}

fn normalize_entries(
    root: &RootHandle,
    entries: &[Entry],
    source_date_epoch: i64,
    limits: MaterializationLimits,
    deadline: &Deadline,
) -> Result<(), Error> {
    let timestamp = filetime::FileTime::from_unix_time(source_date_epoch, 0);

    // Children precede their directories, so directory timestamps are the
    // final metadata operation for each subtree.
    for entry in entries.iter().rev() {
        let path = root.display_path(&entry.relative);
        deadline.check(&path)?;
        match &entry.kind {
            EntryKind::Symlink { .. } => {
                let file = root.open_inspection(entry)?;
                require_symlink_handle_matches(entry, &file, true, &path, limits, deadline)?;
                set_symlink_handle_times(&file, &path, source_date_epoch)?;
                // Replacing a symlink requires a new inode, so fstat identity
                // and type are sufficient after the timestamp write.
                let metadata = file.metadata().map_err(|source| Error::Io {
                    operation: "verify timestamped Git materialization symlink",
                    path: path.clone(),
                    source,
                })?;
                if Identity::from_metadata(&metadata) != entry.identity || !metadata.file_type().is_symlink() {
                    return Err(Error::EntryChanged(path));
                }
                require_single_link(&path, &metadata, &entry.kind)?;
            }
            EntryKind::Directory | EntryKind::Regular { .. } => {
                let file = root.open_data(entry)?;
                require_handle_matches(
                    entry,
                    &file.metadata().map_err(|source| Error::Io {
                        operation: "inspect opened Git materialization entry",
                        path: path.clone(),
                        source,
                    })?,
                    false,
                    &path,
                )?;
                file.set_permissions(Permissions::from_mode(entry.kind.normalized_mode()))
                    .map_err(|source| Error::Io {
                        operation: "normalize Git materialization mode",
                        path: path.clone(),
                        source,
                    })?;
                filetime::set_file_handle_times(file.file(), Some(timestamp), Some(timestamp)).map_err(|source| {
                    Error::Io {
                        operation: "normalize Git materialization timestamp",
                        path: path.clone(),
                        source,
                    }
                })?;
                require_handle_matches(
                    entry,
                    &file.metadata().map_err(|source| Error::Io {
                        operation: "verify opened Git materialization entry",
                        path: path.clone(),
                        source,
                    })?,
                    true,
                    &path,
                )?;
            }
        }
        deadline.check(&path)?;
    }
    Ok(())
}

fn set_symlink_handle_times(file: &fs::File, path: &Path, source_date_epoch: i64) -> Result<(), Error> {
    let seconds = libc::time_t::try_from(source_date_epoch).map_err(|_| Error::Io {
        operation: "represent Git materialization symlink timestamp",
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "timestamp is outside time_t"),
    })?;
    let times = [
        libc::timespec {
            tv_sec: seconds,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: seconds,
            tv_nsec: 0,
        },
    ];
    // SAFETY: the descriptor is live, the empty path is NUL-terminated, and
    // `times` contains the two initialized timespec values required by Linux.
    let result = unsafe {
        libc::utimensat(
            file.as_raw_fd(),
            c"".as_ptr(),
            times.as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == -1 {
        Err(Error::Io {
            operation: "normalize opened Git materialization symlink timestamp",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

fn require_symlink_handle_matches(
    entry: &Entry,
    file: &fs::File,
    verify_target: bool,
    path: &Path,
    limits: MaterializationLimits,
    deadline: &Deadline,
) -> Result<(), Error> {
    deadline.check(path)?;
    let metadata = file.metadata().map_err(|source| Error::Io {
        operation: "inspect opened Git materialization symlink",
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_symlink() || Identity::from_metadata(&metadata) != entry.identity {
        return Err(Error::EntryChanged(path.to_owned()));
    }
    require_single_link(path, &metadata, &entry.kind)?;
    if verify_target {
        let EntryKind::Symlink { target } = &entry.kind else {
            return Err(Error::EntryChanged(path.to_owned()));
        };
        if read_symlink_handle(file, path, limits, deadline)? != *target {
            return Err(Error::EntryChanged(path.to_owned()));
        }
    }
    deadline.check(path)?;
    Ok(())
}

fn require_handle_matches(
    entry: &Entry,
    metadata: &std::fs::Metadata,
    require_normalized: bool,
    path: &Path,
) -> Result<(), Error> {
    let kind = if metadata.file_type().is_dir() {
        EntryKind::Directory
    } else if metadata.file_type().is_file() {
        EntryKind::Regular {
            executable: metadata.mode() & 0o111 != 0,
            length: metadata.len(),
        }
    } else {
        return Err(Error::EntryChanged(path.to_owned()));
    };
    require_single_link(path, metadata, &kind)?;
    if require_normalized {
        require_normalized_mode(path, metadata, &kind)?;
    }
    if Identity::from_metadata(metadata) != entry.identity || kind != entry.kind {
        return Err(Error::EntryChanged(path.to_owned()));
    }
    Ok(())
}

fn require_stamp(entry: &Entry, metadata: &std::fs::Metadata, path: &Path) -> Result<(), Error> {
    if MetadataStamp::from_metadata(metadata) == entry.stamp {
        Ok(())
    } else {
        Err(Error::EntryChanged(path.to_owned()))
    }
}

fn require_same_tree(
    root: &RootHandle,
    audited: &[Entry],
    normalized: &[Entry],
    deadline: &Deadline,
) -> Result<(), Error> {
    if audited.len() != normalized.len() {
        return Err(Error::TreeChanged);
    }
    for (expected, actual) in audited.iter().zip(normalized) {
        let path = root.display_path(&actual.relative);
        deadline.check(&path)?;
        if expected.relative != actual.relative || expected.identity != actual.identity || expected.kind != actual.kind
        {
            return Err(Error::EntryChanged(path));
        }
    }
    Ok(())
}

fn require_stable_tree(
    root: &RootHandle,
    expected: &[Entry],
    actual: &[Entry],
    deadline: &Deadline,
) -> Result<(), Error> {
    require_same_tree(root, expected, actual, deadline)?;
    for (expected, actual) in expected.iter().zip(actual) {
        let path = root.display_path(&actual.relative);
        deadline.check(&path)?;
        if expected.stamp != actual.stamp {
            return Err(Error::EntryChanged(path));
        }
    }
    Ok(())
}

fn hash_tree(
    root: &RootHandle,
    entries: &[Entry],
    limits: MaterializationLimits,
    deadline: &Deadline,
) -> Result<[u8; 32], Error> {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hash_length(&mut hasher, entries.len(), "entry count", &root.path)?;

    for entry in entries {
        let path = root.display_path(&entry.relative);
        deadline.check(&path)?;
        match &entry.kind {
            EntryKind::Directory => {
                let file = root.open_data(entry)?;
                let metadata = file.metadata().map_err(|source| Error::Io {
                    operation: "inspect Git materialization directory while hashing",
                    path: path.clone(),
                    source,
                })?;
                require_handle_matches(entry, &metadata, true, &path)?;
            }
            EntryKind::Regular { .. } => {}
            EntryKind::Symlink { .. } => {
                let file = root.open_inspection(entry)?;
                require_symlink_handle_matches(entry, &file, false, &path, limits, deadline)?;
            }
        }
        hasher.update([entry.kind.tag()]);
        hash_length(&mut hasher, entry.relative.len(), "relative path", &path)?;
        hasher.update(&entry.relative);
        hasher.update(entry.kind.normalized_mode().to_le_bytes());

        match &entry.kind {
            EntryKind::Directory => {}
            EntryKind::Regular { length, .. } => {
                hash_regular(root, entry, *length, &mut hasher, &path, deadline)?;
            }
            EntryKind::Symlink { target } => hash_symlink(target, &mut hasher, &path)?,
        }
    }

    Ok(hasher.finalize().into())
}

fn hash_length(hasher: &mut Sha256, length: usize, field: &'static str, path: &Path) -> Result<(), Error> {
    let length = u64::try_from(length).map_err(|_| Error::LengthNotRepresentable {
        field,
        path: path.to_owned(),
    })?;
    hasher.update(length.to_le_bytes());
    Ok(())
}

fn hash_regular(
    root: &RootHandle,
    entry: &Entry,
    expected_length: u64,
    hasher: &mut Sha256,
    path: &Path,
    deadline: &Deadline,
) -> Result<(), Error> {
    deadline.check(path)?;
    let mut file = root.open_data(entry)?;
    let before = file.metadata().map_err(|source| Error::Io {
        operation: "inspect Git materialization file before hashing",
        path: path.to_owned(),
        source,
    })?;
    require_handle_matches(entry, &before, true, path)?;
    if before.len() != expected_length {
        return Err(Error::FileLengthChanged {
            path: path.to_owned(),
            expected: expected_length,
            actual: before.len(),
        });
    }

    hasher.update(expected_length.to_le_bytes());
    let mut read_length = 0_u64;
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    while read_length < expected_length {
        deadline.check(path)?;
        let remaining = expected_length - read_length;
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).expect("buffer length fits usize");
        let read = match file.read(&mut buffer[..limit]) {
            Ok(0) => {
                return Err(Error::FileLengthChanged {
                    path: path.to_owned(),
                    expected: expected_length,
                    actual: read_length,
                });
            }
            Ok(read) => read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(Error::Io {
                    operation: "read Git materialization file",
                    path: path.to_owned(),
                    source,
                });
            }
        };
        hasher.update(&buffer[..read]);
        read_length += u64::try_from(read).expect("buffer read length fits u64");
    }

    let mut extra = [0_u8; 1];
    let extra_read = loop {
        deadline.check(path)?;
        match file.read(&mut extra) {
            Ok(read) => break read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                return Err(Error::Io {
                    operation: "verify Git materialization file length",
                    path: path.to_owned(),
                    source,
                });
            }
        }
    };
    let after = file.metadata().map_err(|source| Error::Io {
        operation: "inspect Git materialization file after hashing",
        path: path.to_owned(),
        source,
    })?;
    if extra_read != 0 || after.len() != expected_length {
        let observed_extra = if extra_read == 0 { 0 } else { 1 };
        return Err(Error::FileLengthChanged {
            path: path.to_owned(),
            expected: expected_length,
            actual: after.len().max(expected_length + observed_extra),
        });
    }
    if content_stamp(&before) != content_stamp(&after) {
        return Err(Error::FileChangedDuringHash(path.to_owned()));
    }
    require_handle_matches(entry, &after, true, path)?;
    deadline.check(path)?;
    Ok(())
}

fn content_stamp(metadata: &std::fs::Metadata) -> (u64, i64, i64, i64, i64) {
    (
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn hash_symlink(expected_target: &[u8], hasher: &mut Sha256, path: &Path) -> Result<(), Error> {
    hash_length(hasher, expected_target.len(), "symlink target", path)?;
    hasher.update(expected_target);
    Ok(())
}

fn verify_normalized_tree(
    root: &RootHandle,
    entries: &[Entry],
    source_date_epoch: i64,
    deadline: &Deadline,
) -> Result<(), Error> {
    for entry in entries {
        let path = root.display_path(&entry.relative);
        deadline.check(&path)?;
        if entry.stamp.atime != source_date_epoch
            || entry.stamp.atime_nsec != 0
            || entry.stamp.mtime != source_date_epoch
            || entry.stamp.mtime_nsec != 0
        {
            return Err(Error::TimestampNotNormalized {
                path,
                expected: source_date_epoch,
                atime: entry.stamp.atime,
                atime_nsec: entry.stamp.atime_nsec,
                mtime: entry.stamp.mtime,
                mtime_nsec: entry.stamp.mtime_nsec,
            });
        }
    }
    Ok(())
}

fn join_relative(parent: &[u8], name: &[u8], path: &Path) -> Result<Vec<u8>, Error> {
    let separator = usize::from(!parent.is_empty());
    let capacity = parent
        .len()
        .checked_add(separator)
        .and_then(|length| length.checked_add(name.len()))
        .ok_or_else(|| Error::ArithmeticOverflow {
            resource: "relative path bytes",
            path: path.to_owned(),
        })?;
    let mut relative = Vec::new();
    relative
        .try_reserve_exact(capacity)
        .map_err(|source| Error::Allocation {
            resource: "relative path bytes",
            requested: capacity,
            source,
        })?;
    relative.extend_from_slice(parent);
    if separator != 0 {
        relative.push(b'/');
    }
    relative.extend_from_slice(name);
    Ok(relative)
}

fn copy_bytes(bytes: &[u8], resource: &'static str) -> Result<Vec<u8>, Error> {
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(bytes.len())
        .map_err(|source| Error::Allocation {
            resource,
            requested: bytes.len(),
            source,
        })?;
    copied.extend_from_slice(bytes);
    Ok(copied)
}

fn reserve<T>(items: &mut Vec<T>, additional: usize, resource: &'static str) -> Result<(), Error> {
    items.try_reserve(additional).map_err(|source| Error::Allocation {
        resource,
        requested: additional,
        source,
    })
}

fn enforce_usize_limit(resource: &'static str, limit: usize, actual: usize, path: &Path) -> Result<(), Error> {
    if actual > limit {
        Err(Error::LimitExceeded {
            resource,
            limit: limit as u64,
            actual: actual as u64,
            path: path.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn enforce_u64_limit(resource: &'static str, limit: u64, actual: u64, path: &Path) -> Result<(), Error> {
    if actual > limit {
        Err(Error::LimitExceeded {
            resource,
            limit,
            actual,
            path: path.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn checked_add_limit(
    resource: &'static str,
    current: u64,
    additional: u64,
    limit: u64,
    path: &Path,
) -> Result<u64, Error> {
    let actual = current
        .checked_add(additional)
        .ok_or_else(|| Error::ArithmeticOverflow {
            resource,
            path: path.to_owned(),
        })?;
    enforce_u64_limit(resource, limit, actual, path)?;
    Ok(actual)
}

fn usize_to_u64(value: usize, resource: &'static str, path: &Path) -> Result<u64, Error> {
    u64::try_from(value).map_err(|_| Error::ArithmeticOverflow {
        resource,
        path: path.to_owned(),
    })
}

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
mod tests {
    use std::{
        ffi::OsString,
        os::unix::{
            ffi::OsStringExt,
            fs::{MetadataExt, PermissionsExt, symlink},
            net::UnixListener,
        },
    };

    use nix::{sys::stat::Mode, unistd::mkfifo};

    use super::*;

    const EPOCH: i64 = 1_700_000_000;

    #[test]
    fn empty_tree_has_a_stable_golden_digest() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            normalize_and_hash(root.path(), EPOCH).unwrap(),
            "badf0db4c0a8fd2cde62d7893df1313fcdaca41f8b9cab21c2e58f53c033c908"
        );
    }

    #[test]
    fn trusted_descriptor_root_path_is_accepted_without_weakening_descendant_traversal() {
        let temporary = tempfile::tempdir().unwrap();
        let held_root = temporary.path().join("held");
        let checkout = held_root.join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join("source"), b"descriptor rooted").unwrap();
        fs::create_dir(checkout.join(".git")).unwrap();
        fs::write(checkout.join(".git/config"), b"admin").unwrap();
        let held = fs::File::open(&held_root).unwrap();
        let descriptor_checkout =
            PathBuf::from(format!("/proc/{}/fd/{}/checkout", std::process::id(), held.as_raw_fd()));

        remove_git_administration_descriptor_path_bounded(&descriptor_checkout).unwrap();
        let digest = normalize_and_hash_descriptor_path(&descriptor_checkout, EPOCH).unwrap();

        assert_eq!(digest.len(), 64);
        assert_eq!(fs::read(checkout.join("source")).unwrap(), b"descriptor rooted");
        assert!(!checkout.join(".git").exists());

        let staged = held_root.join("staged");
        let installed = held_root.join("installed");
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("source"), b"sealed descriptor root").unwrap();
        let descriptor_staged = PathBuf::from(format!("/proc/{}/fd/{}/staged", std::process::id(), held.as_raw_fd()));
        let proof =
            normalize_and_seal_descriptor_path_with_limits(&descriptor_staged, EPOCH, MaterializationLimits::default())
                .unwrap();
        fs::rename(&staged, &installed).unwrap();
        let descriptor_installed = PathBuf::from(format!(
            "/proc/{}/fd/{}/installed",
            std::process::id(),
            held.as_raw_fd()
        ));
        proof.verify_installed_descriptor_path(&descriptor_installed).unwrap();
    }

    #[test]
    fn order_non_utf8_permissions_and_timestamps_normalize_identically() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let raw_name = OsString::from_vec(b"non-utf8-\xff".to_vec());
        create_equivalent_tree(first.path(), &raw_name, false, 111);
        create_equivalent_tree(second.path(), &raw_name, true, 222);

        let first_hash = normalize_and_hash(first.path(), EPOCH).unwrap();
        let second_hash = normalize_and_hash(second.path(), EPOCH).unwrap();
        assert_eq!(first_hash, second_hash);

        for root in [first.path(), second.path()] {
            assert_mode(root, DIRECTORY_MODE);
            assert_mode(&root.join("nested"), DIRECTORY_MODE);
            assert_mode(&root.join(&raw_name), REGULAR_MODE);
            assert_mode(&root.join("executable"), EXECUTABLE_MODE);
            for path in [
                root.to_owned(),
                root.join("nested"),
                root.join(&raw_name),
                root.join("executable"),
                root.join("link"),
            ] {
                assert_timestamp(&path, EPOCH);
            }
        }
    }

    #[test]
    fn content_path_mode_type_and_symlink_target_are_semantic() {
        let baseline = digest_with(|_| {});
        let mutations = [
            digest_with(|root| fs::write(root.join("regular"), b"bravo").unwrap()),
            digest_with(|root| fs::write(root.join("regular"), b"longer").unwrap()),
            digest_with(|root| fs::rename(root.join("regular"), root.join("renamed")).unwrap()),
            digest_with(|root| fs::set_permissions(root.join("regular"), Permissions::from_mode(0o755)).unwrap()),
            digest_with(|root| {
                fs::remove_file(root.join("link")).unwrap();
                symlink("executable", root.join("link")).unwrap();
            }),
            digest_with(|root| {
                fs::remove_file(root.join("kind")).unwrap();
                fs::create_dir(root.join("kind")).unwrap();
            }),
        ];
        for mutation in mutations {
            assert_ne!(mutation, baseline);
        }
    }

    #[test]
    fn entry_added_after_the_canonical_hash_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("original"), b"original").unwrap();

        let result = normalize_with_hook(root.path(), |point| {
            if point == HookPoint::AfterCanonicalHash {
                let added = root.path().join("added");
                fs::write(&added, b"added").unwrap();
                fs::set_permissions(added, Permissions::from_mode(REGULAR_MODE)).unwrap();
            }
        });

        assert!(matches!(result, Err(Error::TreeChanged)));
        assert!(root.path().join("added").exists());
    }

    #[test]
    fn same_length_content_change_after_the_canonical_hash_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, b"alpha").unwrap();

        let result = normalize_with_hook(root.path(), |point| {
            if point == HookPoint::AfterCanonicalHash {
                fs::write(&source, b"bravo").unwrap();
            }
        });

        assert!(matches!(result, Err(Error::TreeChanged)));
        assert_eq!(fs::read(source).unwrap(), b"bravo");
    }

    #[test]
    fn ancestor_symlink_swap_cannot_escape_the_audited_root() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("tree");
        let moved = temporary.path().join("moved-outside");
        let nested = root.join("nested");
        let source = nested.join("source");
        fs::create_dir_all(&nested).unwrap();
        fs::write(&source, b"outside sentinel").unwrap();
        fs::set_permissions(&source, Permissions::from_mode(0o600)).unwrap();
        let old = filetime::FileTime::from_unix_time(123, 0);
        filetime::set_file_times(&source, old, old).unwrap();

        let result = normalize_with_hook(&root, |point| {
            if point == HookPoint::AfterAudit {
                fs::rename(&nested, &moved).unwrap();
                symlink(&moved, &nested).unwrap();
            }
        });

        assert!(result.is_err());
        let escaped = moved.join("source");
        assert_mode(&escaped, 0o600);
        assert_eq!(fs::metadata(escaped).unwrap().mtime(), 123);
    }

    #[test]
    fn hard_links_reject_the_whole_tree_before_mutation() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("a-original");
        fs::write(&original, b"shared").unwrap();
        fs::set_permissions(&original, Permissions::from_mode(0o600)).unwrap();
        let old = filetime::FileTime::from_unix_time(123, 0);
        filetime::set_file_times(&original, old, old).unwrap();
        fs::hard_link(&original, root.path().join("b-link")).unwrap();

        assert!(matches!(
            normalize_and_hash(root.path(), EPOCH),
            Err(Error::UnexpectedLinkCount { links: 2, .. })
        ));
        assert_mode(&original, 0o600);
        assert_eq!(fs::metadata(&original).unwrap().mtime(), 123);
    }

    #[test]
    fn fifos_and_sockets_are_rejected_before_mutation() {
        let fifo_root = tempfile::tempdir().unwrap();
        let fifo_sentinel = fifo_root.path().join("a-sentinel");
        fs::write(&fifo_sentinel, b"sentinel").unwrap();
        fs::set_permissions(&fifo_sentinel, Permissions::from_mode(0o600)).unwrap();
        mkfifo(&fifo_root.path().join("z-fifo"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        assert!(matches!(
            normalize_and_hash(fifo_root.path(), EPOCH),
            Err(Error::UnsupportedFileType { kind: "FIFO", .. })
        ));
        assert_mode(&fifo_sentinel, 0o600);

        let socket_root = tempfile::tempdir().unwrap();
        let socket_sentinel = socket_root.path().join("a-sentinel");
        fs::write(&socket_sentinel, b"sentinel").unwrap();
        fs::set_permissions(&socket_sentinel, Permissions::from_mode(0o600)).unwrap();
        let _listener = match UnixListener::bind(socket_root.path().join("z-socket")) {
            Ok(listener) => listener,
            // Some test sandboxes prohibit AF_UNIX creation. FIFO coverage
            // above still proves special inodes are rejected without opening
            // them; exercise the socket branch whenever the host permits it.
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to create Unix socket fixture: {error}"),
        };
        assert!(matches!(
            normalize_and_hash(socket_root.path(), EPOCH),
            Err(Error::UnsupportedFileType { kind: "socket", .. })
        ));
        assert_mode(&socket_sentinel, 0o600);
    }

    #[test]
    fn symlinks_are_hashed_and_timestamped_without_following_targets() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("tree");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::write(&outside, b"outside").unwrap();
        fs::set_permissions(&outside, Permissions::from_mode(0o600)).unwrap();
        let old = filetime::FileTime::from_unix_time(123, 0);
        filetime::set_file_times(&outside, old, old).unwrap();
        symlink("../outside", root.join("link")).unwrap();

        normalize_and_hash(&root, EPOCH).unwrap();

        assert!(
            fs::symlink_metadata(root.join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_timestamp(&root.join("link"), EPOCH);
        assert_mode(&outside, 0o600);
        assert_eq!(fs::metadata(&outside).unwrap().mtime(), 123);
    }

    #[test]
    fn entry_limit_accepts_exact_n_and_rejects_n_plus_one_before_mutation() {
        let exact = tempfile::tempdir().unwrap();
        fs::write(exact.path().join("a"), b"a").unwrap();
        fs::write(exact.path().join("b"), b"b").unwrap();
        let mut limits = MaterializationLimits::default();
        limits.max_entries = 2;
        normalize_and_hash_with_limits(exact.path(), EPOCH, limits).unwrap();

        let over = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            let path = over.path().join(name);
            fs::write(&path, name).unwrap();
            fs::set_permissions(&path, Permissions::from_mode(0o600)).unwrap();
        }
        assert_limit(
            normalize_and_hash_with_limits(over.path(), EPOCH, limits),
            "total entries",
            2,
            3,
        );
        assert_mode(&over.path().join("a"), 0o600);
    }

    #[test]
    fn depth_name_and_path_limits_have_exact_boundaries() {
        let exact_depth = tempfile::tempdir().unwrap();
        fs::create_dir_all(exact_depth.path().join("a/b")).unwrap();
        let mut limits = MaterializationLimits::default();
        limits.max_depth = 2;
        normalize_and_hash_with_limits(exact_depth.path(), EPOCH, limits).unwrap();

        let over_depth = tempfile::tempdir().unwrap();
        fs::create_dir_all(over_depth.path().join("a/b/c")).unwrap();
        assert_limit(
            normalize_and_hash_with_limits(over_depth.path(), EPOCH, limits),
            "entry depth",
            2,
            3,
        );

        let exact_name = tempfile::tempdir().unwrap();
        fs::write(exact_name.path().join("abc"), b"").unwrap();
        limits = MaterializationLimits::default();
        limits.max_name_bytes = 3;
        normalize_and_hash_with_limits(exact_name.path(), EPOCH, limits).unwrap();

        let over_name = tempfile::tempdir().unwrap();
        fs::write(over_name.path().join("abcd"), b"").unwrap();
        assert_limit(
            normalize_and_hash_with_limits(over_name.path(), EPOCH, limits),
            "entry name bytes",
            3,
            4,
        );

        let exact_path = tempfile::tempdir().unwrap();
        fs::create_dir(exact_path.path().join("a")).unwrap();
        fs::write(exact_path.path().join("a/b"), b"").unwrap();
        limits = MaterializationLimits::default();
        limits.max_path_bytes = 3;
        normalize_and_hash_with_limits(exact_path.path(), EPOCH, limits).unwrap();

        let over_path = tempfile::tempdir().unwrap();
        fs::create_dir(over_path.path().join("a")).unwrap();
        fs::write(over_path.path().join("a/bb"), b"").unwrap();
        assert_limit(
            normalize_and_hash_with_limits(over_path.path(), EPOCH, limits),
            "relative path bytes",
            3,
            4,
        );
    }

    #[test]
    fn file_and_regular_aggregate_limits_have_exact_boundaries() {
        let exact_file = tempfile::tempdir().unwrap();
        fs::write(exact_file.path().join("source"), b"abc").unwrap();
        let mut limits = MaterializationLimits::default();
        limits.max_file_bytes = 3;
        normalize_and_hash_with_limits(exact_file.path(), EPOCH, limits).unwrap();

        let over_file = tempfile::tempdir().unwrap();
        fs::write(over_file.path().join("source"), b"abcd").unwrap();
        assert_limit(
            normalize_and_hash_with_limits(over_file.path(), EPOCH, limits),
            "regular file bytes",
            3,
            4,
        );

        let exact_total = tempfile::tempdir().unwrap();
        fs::write(exact_total.path().join("a"), b"a").unwrap();
        fs::write(exact_total.path().join("b"), b"bc").unwrap();
        limits = MaterializationLimits::default();
        limits.max_total_regular_bytes = 3;
        normalize_and_hash_with_limits(exact_total.path(), EPOCH, limits).unwrap();

        let over_total = tempfile::tempdir().unwrap();
        fs::write(over_total.path().join("a"), b"a").unwrap();
        fs::write(over_total.path().join("b"), b"bcd").unwrap();
        assert_limit(
            normalize_and_hash_with_limits(over_total.path(), EPOCH, limits),
            "total regular file bytes",
            3,
            4,
        );
    }

    #[test]
    fn symlink_and_all_allocation_aggregates_have_exact_boundaries() {
        let exact_target = tempfile::tempdir().unwrap();
        symlink("abc", exact_target.path().join("link")).unwrap();
        let mut limits = MaterializationLimits::default();
        limits.max_symlink_target_bytes = 3;
        normalize_and_hash_with_limits(exact_target.path(), EPOCH, limits).unwrap();

        let over_target = tempfile::tempdir().unwrap();
        symlink("abcd", over_target.path().join("link")).unwrap();
        assert_limit(
            normalize_and_hash_with_limits(over_target.path(), EPOCH, limits),
            "symlink target bytes",
            3,
            4,
        );

        let exact_names = tempfile::tempdir().unwrap();
        fs::write(exact_names.path().join("a"), b"").unwrap();
        fs::write(exact_names.path().join("bb"), b"").unwrap();
        limits = MaterializationLimits::default();
        limits.max_total_name_bytes = 3;
        normalize_and_hash_with_limits(exact_names.path(), EPOCH, limits).unwrap();

        let over_names = tempfile::tempdir().unwrap();
        fs::write(over_names.path().join("a"), b"").unwrap();
        fs::write(over_names.path().join("bbb"), b"").unwrap();
        assert_limit(
            normalize_and_hash_with_limits(over_names.path(), EPOCH, limits),
            "total entry name bytes",
            3,
            4,
        );

        let exact_paths = tempfile::tempdir().unwrap();
        fs::write(exact_paths.path().join("a"), b"").unwrap();
        fs::write(exact_paths.path().join("bb"), b"").unwrap();
        limits = MaterializationLimits::default();
        limits.max_total_path_bytes = 3;
        normalize_and_hash_with_limits(exact_paths.path(), EPOCH, limits).unwrap();

        let over_paths = tempfile::tempdir().unwrap();
        fs::write(over_paths.path().join("a"), b"").unwrap();
        fs::write(over_paths.path().join("bbb"), b"").unwrap();
        assert_limit(
            normalize_and_hash_with_limits(over_paths.path(), EPOCH, limits),
            "total relative path bytes",
            3,
            4,
        );

        let exact_links = tempfile::tempdir().unwrap();
        symlink("a", exact_links.path().join("one")).unwrap();
        symlink("bb", exact_links.path().join("two")).unwrap();
        limits = MaterializationLimits::default();
        limits.max_total_symlink_target_bytes = 3;
        normalize_and_hash_with_limits(exact_links.path(), EPOCH, limits).unwrap();

        let over_links = tempfile::tempdir().unwrap();
        symlink("a", over_links.path().join("one")).unwrap();
        symlink("bbb", over_links.path().join("two")).unwrap();
        assert_limit(
            normalize_and_hash_with_limits(over_links.path(), EPOCH, limits),
            "total symlink target bytes",
            3,
            4,
        );
    }

    #[test]
    fn zero_deadline_and_impossible_symlink_capacity_fail_structurally() {
        let empty = tempfile::tempdir().unwrap();
        let mut limits = MaterializationLimits::default();
        limits.max_duration = Duration::ZERO;
        assert!(matches!(
            normalize_and_hash_with_limits(empty.path(), EPOCH, limits),
            Err(Error::DurationExceeded { .. })
        ));

        let linked = tempfile::tempdir().unwrap();
        symlink("a", linked.path().join("link")).unwrap();
        limits = MaterializationLimits::default();
        limits.max_symlink_target_bytes = usize::MAX;
        assert!(matches!(
            normalize_and_hash_with_limits(linked.path(), EPOCH, limits),
            Err(Error::ArithmeticOverflow {
                resource: "symlink target bytes",
                ..
            })
        ));

        limits.max_symlink_target_bytes = usize::MAX - 1;
        assert!(matches!(
            normalize_and_hash_with_limits(linked.path(), EPOCH, limits),
            Err(Error::Allocation {
                resource: "symlink target bytes",
                requested: usize::MAX,
                ..
            })
        ));
    }

    #[test]
    fn deep_and_wide_trees_use_iterative_bounded_traversal() {
        let deep = tempfile::tempdir().unwrap();
        let mut cursor = deep.path().to_owned();
        for _ in 0..200 {
            cursor.push("d");
            fs::create_dir(&cursor).unwrap();
        }
        fs::write(cursor.join("leaf"), b"deep").unwrap();
        normalize_and_hash(deep.path(), EPOCH).unwrap();

        let wide = tempfile::tempdir().unwrap();
        for index in 0..512 {
            fs::write(wide.path().join(format!("entry-{index:04}")), b"wide").unwrap();
        }
        let before = open_descriptor_count();
        normalize_and_hash(wide.path(), EPOCH).unwrap();
        let after = open_descriptor_count();
        assert!(after <= before + 1, "descriptor leak: before={before}, after={after}");
    }

    #[test]
    fn sealed_materialization_survives_rename_and_rejects_path_replacement_or_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let installed = temporary.path().join("installed");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("payload"), b"sealed").unwrap();
        let proof = normalize_and_seal_with_limits(&source, EPOCH, MaterializationLimits::default()).unwrap();
        assert_eq!(proof.digest().len(), 64);
        fs::rename(&source, &installed).unwrap();
        proof.verify_installed(&installed).unwrap();

        let source = temporary.path().join("source-two");
        let installed = temporary.path().join("installed-two");
        let held = temporary.path().join("held-two");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("payload"), b"sealed").unwrap();
        let proof = normalize_and_seal_with_limits(&source, EPOCH, MaterializationLimits::default()).unwrap();
        fs::rename(&source, &installed).unwrap();
        fs::rename(&installed, &held).unwrap();
        fs::create_dir(&installed).unwrap();
        fs::write(installed.join("payload"), b"sealed").unwrap();
        assert!(matches!(
            proof.verify_installed(&installed),
            Err(Error::EntryChanged(_))
        ));

        let source = temporary.path().join("source-three");
        let installed = temporary.path().join("installed-three");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("payload"), b"sealed").unwrap();
        let proof = normalize_and_seal_with_limits(&source, EPOCH, MaterializationLimits::default()).unwrap();
        fs::rename(&source, &installed).unwrap();
        fs::write(installed.join("payload"), b"mutate").unwrap();
        assert!(proof.verify_installed(&installed).is_err());
    }

    #[test]
    fn sealed_post_install_verification_reuses_the_original_deadline() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let installed = temporary.path().join("installed");
        fs::create_dir(&source).unwrap();
        let mut proof = normalize_and_seal_with_limits(&source, EPOCH, MaterializationLimits::default()).unwrap();
        fs::rename(&source, &installed).unwrap();
        proof.deadline.limit = Duration::ZERO;
        assert!(matches!(
            proof.verify_installed(&installed),
            Err(Error::DurationExceeded { .. })
        ));
    }

    #[test]
    fn bounded_administration_removal_preserves_authored_neighbors_and_never_follows_links() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join(".git");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"outside").unwrap();
        fs::create_dir_all(root.join("nested/.git/objects")).unwrap();
        fs::write(root.join("nested/.git/config"), b"admin").unwrap();
        fs::write(root.join(".git"), b"gitdir: elsewhere").unwrap();
        fs::write(root.join(".git-marker"), b"authored").unwrap();
        symlink(&outside, root.join("linked")).unwrap();
        symlink(&outside, root.join("linked-admin.git")).unwrap();
        fs::create_dir(root.join("other")).unwrap();
        symlink(&outside, root.join("other/.git")).unwrap();

        remove_git_administration_bounded(&root).unwrap();

        assert!(root.is_dir(), "an export root named .git must survive");
        assert!(!root.join(".git").exists());
        assert!(!root.join("nested/.git").exists());
        assert!(!root.join("other/.git").exists());
        assert_eq!(fs::read(root.join(".git-marker")).unwrap(), b"authored");
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside");
        assert!(
            fs::symlink_metadata(root.join("linked"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::symlink_metadata(root.join("linked-admin.git"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn administration_removal_is_bounded_iterative_and_handles_special_git_entries() {
        let exact = tempfile::tempdir().unwrap();
        fs::create_dir(exact.path().join(".git")).unwrap();
        fs::write(exact.path().join(".git/config"), b"config").unwrap();
        let mut limits = MaterializationLimits::default();
        limits.max_entries = 2;
        remove_git_administration_with_limits(exact.path(), limits, false).unwrap();
        assert!(!exact.path().join(".git").exists());

        let over = tempfile::tempdir().unwrap();
        fs::create_dir(over.path().join(".git")).unwrap();
        fs::write(over.path().join(".git/config"), b"config").unwrap();
        fs::write(over.path().join("ordinary"), b"ordinary").unwrap();
        assert_limit(
            remove_git_administration_with_limits(over.path(), limits, false).map(|()| String::new()),
            "total entries",
            2,
            3,
        );
        assert!(over.path().join(".git").exists(), "preflight failure must not mutate");

        let deep = tempfile::tempdir().unwrap();
        let mut cursor = deep.path().join(".git");
        fs::create_dir(&cursor).unwrap();
        for _ in 0..200 {
            cursor.push("d");
            fs::create_dir(&cursor).unwrap();
        }
        fs::write(cursor.join("leaf"), b"leaf").unwrap();
        remove_git_administration_bounded(deep.path()).unwrap();
        assert!(!deep.path().join(".git").exists());

        let special = tempfile::tempdir().unwrap();
        mkfifo(&special.path().join(".git"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        remove_git_administration_bounded(special.path()).unwrap();
        assert!(!special.path().join(".git").exists());
    }

    fn create_equivalent_tree(root: &Path, raw_name: &OsString, reverse: bool, timestamp: i64) {
        fs::create_dir(root.join("nested")).unwrap();
        let files = if reverse {
            vec![
                (PathBuf::from("executable"), b"execute".as_slice()),
                (PathBuf::from(raw_name), b"raw".as_slice()),
            ]
        } else {
            vec![
                (PathBuf::from(raw_name), b"raw".as_slice()),
                (PathBuf::from("executable"), b"execute".as_slice()),
            ]
        };
        for (path, bytes) in files {
            fs::write(root.join(path), bytes).unwrap();
        }
        symlink(raw_name, root.join("link")).unwrap();

        fs::set_permissions(root, Permissions::from_mode(if reverse { 0o777 } else { 0o700 })).unwrap();
        fs::set_permissions(
            root.join("nested"),
            Permissions::from_mode(if reverse { 0o775 } else { 0o700 }),
        )
        .unwrap();
        fs::set_permissions(
            root.join(raw_name),
            Permissions::from_mode(if reverse { 0o664 } else { 0o600 }),
        )
        .unwrap();
        fs::set_permissions(
            root.join("executable"),
            Permissions::from_mode(if reverse { 0o777 } else { 0o711 }),
        )
        .unwrap();

        let old = filetime::FileTime::from_unix_time(timestamp, 0);
        for path in [
            root.to_owned(),
            root.join("nested"),
            root.join(raw_name),
            root.join("executable"),
        ] {
            filetime::set_file_times(path, old, old).unwrap();
        }
        filetime::set_symlink_file_times(root.join("link"), old, old).unwrap();
    }

    fn digest_with(mutate: impl FnOnce(&Path)) -> String {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("regular"), b"alpha").unwrap();
        fs::write(root.path().join("executable"), b"execute").unwrap();
        fs::set_permissions(root.path().join("executable"), Permissions::from_mode(0o755)).unwrap();
        fs::write(root.path().join("kind"), b"kind").unwrap();
        symlink("regular", root.path().join("link")).unwrap();
        mutate(root.path());
        normalize_and_hash(root.path(), EPOCH).unwrap()
    }

    fn normalize_with_hook(root: &Path, hook: impl FnMut(HookPoint)) -> Result<String, Error> {
        let limits = MaterializationLimits::default();
        let deadline = Deadline::new(limits.max_duration);
        let root = RootHandle::open(root)?;
        normalize_and_hash_with(&root, EPOCH, limits, &deadline, hook).map(|(digest, _)| digest)
    }

    fn assert_limit<T>(result: Result<T, Error>, resource: &'static str, limit: u64, actual: u64) {
        assert!(
            matches!(
                result,
                Err(Error::LimitExceeded {
                    resource: found_resource,
                    limit: found_limit,
                    actual: found_actual,
                    ..
                }) if found_resource == resource && found_limit == limit && found_actual == actual
            ),
            "expected {resource} limit {limit} with actual {actual}"
        );
    }

    fn open_descriptor_count() -> usize {
        fs::read_dir("/proc/self/fd").unwrap().count()
    }

    fn assert_mode(path: &Path, expected: u32) {
        assert_eq!(fs::symlink_metadata(path).unwrap().mode() & 0o7777, expected);
    }

    fn assert_timestamp(path: &Path, expected: i64) {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert_eq!(metadata.atime(), expected);
        assert_eq!(metadata.atime_nsec(), 0);
        assert_eq!(metadata.mtime(), expected);
        assert_eq!(metadata.mtime_nsec(), 0);
    }
}
