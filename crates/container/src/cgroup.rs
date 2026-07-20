//! Descriptor-pinned cgroup v2 lifecycle primitives.
//!
//! This module deliberately stops below container activation. It expects a
//! systemd delegation created with `Delegate=yes` and
//! `DelegateSubgroup=cast-supervisor`: the delegated root has no direct
//! processes, while its fixed `cast-supervisor` child contains exactly this
//! supervisor's TGID. It authenticates that baseline, creates and configures
//! one sibling payload leaf, lends pinned root and leaf descriptors to the
//! crate's `clone3(CLONE_INTO_CGROUP)` integration, and tears the leaf down
//! without following symlinks. The caller must arrange the delegation; the
//! container crate places the child atomically before releasing it.
//! systemd may initially leave the delegated root's
//! `cgroup.subtree_control` empty. Cast authenticates the complete
//! supervisor-only topology first, enables only its missing `cpu`, `memory`,
//! and `pids` controllers through the pinned descriptor, and requires an exact
//! effective-set readback before the root capability can escape `open`.
//!
//! Root authority is linear. [`DelegatedCgroupRoot::create_leaf`] consumes the
//! authenticated root and moves its sole descriptor into either the live leaf
//! or a recovery value. This prevents an accidental second owner from mutating
//! or removing the delegated topology while payload creation is in flight.
//!
//! Linux cgroup v2 deliberately forbids `rename(2)`, and it offers no
//! unlink-by-descriptor operation for cgroup directories. Removal therefore
//! reopens the unpredictable leaf name below the locked delegated-root
//! descriptor, compares its device/inode witness, and calls descriptor-relative
//! `unlinkat(AT_REMOVEDIR)`. The final check/remove pair is safe only while this
//! process exclusively owns the delegated root. [`DelegatedCgroupRoot::open`]
//! enforces owner/mode checks and a non-blocking advisory lock against other
//! cooperating supervisors. The caller must additionally keep the delegated
//! subtree inaccessible to payloads and other same-UID processes; Linux cannot
//! enforce that part with a conditional-rmdir syscall because none exists.
//!
//! The unit tests use ordinary temporary directories only to exercise parsers,
//! exact writes, descriptor lookup, and replacement detection. Ordinary files
//! do not implement cgroup kernel semantics, so enforcement remains a separate
//! live integration-test responsibility under a genuinely delegated cgroup v2
//! subtree.

// Cgroup controls are opened and authenticated descriptor-relative. A
// path-context wrapper would report the diagnostic label as though it were the
// authority, so raw std file ownership is intentional here.
#![allow(clippy::disallowed_types)]

use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
#[cfg(test)]
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use nix::libc;
use snafu::Snafu;

use self::control_files::{
    ANCHORED_RESOLUTION, acquire_exclusive_delegation, descriptor_error, descriptor_identity,
    enable_required_controllers, open_control_path, open_owned_writable_control, openat2, os_str, path_cstring,
    read_descendant_counts, read_events, read_pid_list, read_single_value, read_word_set, require_cgroup2,
    require_controllers, require_directory, require_empty_unfrozen_delegation, require_owned_private,
    require_populated_unfrozen_delegation, verify_control, write_control, write_control_if_present,
};
#[cfg(test)]
use self::control_files::{
    CONTROL_READ_LIMIT_BYTES, controller_enable_request, duplicate_cloexec, enable_required_controllers_with,
    parse_events, parse_pid_list, read_control, write_exact_control_value,
};

mod control_files;

const MAX_GETRANDOM_EINTR_RETRIES: usize = 8;
const LEAF_RANDOM_BYTES: usize = 16;
const LEAF_CREATE_ATTEMPTS: usize = 8;
const LEAF_NAME_PREFIX: &str = "cast-";
const SUPERVISOR_NAME: &CStr = c"cast-supervisor";
// Linux PID_MAX_LIMIT on the supported 64-bit target. `pids.max` rejects the
// next value because PID_MAX_LIMIT + 1 is the controller's internal `max`
// sentinel rather than a finite ceiling.
const MAX_PIDS: u64 = 4 * 1024 * 1024;
const MIN_CPU_BANDWIDTH_MICROS: u64 = 1_000;
const MAX_CPU_PERIOD_MICROS: u64 = 1_000_000;
// Linux BW_SHIFT is 20, leaving 44 bits for the finite runtime value.
const MAX_CPU_QUOTA_MICROS: u64 = (1_u64 << 44) - 1;
// The kernel may retain a removed cgroup as a dying descendant for an
// unbounded asynchronous reclamation interval. Each terminal payload leaf can
// add at most one such object, so cap that deleted controller state instead of
// confusing it with a visible, reusable leaf. Admission reserves one final
// slot for the leaf owned by the next execution; cleanup may consume that slot
// after authenticated removal.
const MAX_RETIRED_CGROUPS: u64 = 64;
const MAX_RETIRED_CGROUPS_AT_ADMISSION: u64 = MAX_RETIRED_CGROUPS - 1;
const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Errors emitted by descriptor-pinned cgroup v2 setup and lifecycle work.
#[derive(Debug, Snafu)]
pub enum CgroupError {
    #[snafu(display("{operation} at {}: {source}", path.display()))]
    DescriptorOperation {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },

    #[snafu(display("cgroup mount path must be normalized and absolute: {}", path.display()))]
    InvalidMountPath { path: PathBuf },

    #[snafu(display("delegated cgroup path must be a non-empty normalized relative path: {}", path.display()))]
    InvalidDelegatedPath { path: PathBuf },

    #[snafu(display("{} is not a cgroup v2 filesystem (magic 0x{found:x})", path.display()))]
    NotCgroupV2 { path: PathBuf, found: libc::c_long },

    #[snafu(display("cgroup object at {} is not a directory", path.display()))]
    NotDirectory { path: PathBuf },

    #[snafu(display("cgroup control at {} is not a regular file", path.display()))]
    NotControlFile { path: PathBuf },

    #[snafu(display("cgroup control {} exceeds the {limit}-byte read ceiling", path.display()))]
    ControlTooLarge { path: PathBuf, limit: usize },

    #[snafu(display("malformed cgroup control {}: {reason}", path.display()))]
    MalformedControl { path: PathBuf, reason: String },

    #[snafu(display("delegated cgroup {} has type {found:?}, expected \"domain\"", path.display()))]
    InvalidCgroupType { path: PathBuf, found: String },

    #[snafu(display("delegated cgroup {} is missing enabled controllers: {missing}", path.display()))]
    MissingControllers { path: PathBuf, missing: String },

    #[snafu(display("delegated cgroup {} contains process {pid}; the delegated parent must be empty", path.display()))]
    DelegationPopulated { path: PathBuf, pid: u32 },

    #[snafu(display(
        "delegated cgroup subtree {} is populated even though its direct process list is empty",
        path.display()
    ))]
    DelegationSubtreePopulated { path: PathBuf },

    #[snafu(display(
        "delegated cgroup topology at {} expected {expected_descendants} visible descendants and at most {maximum_dying_descendants} dying descendants, found {descendants} visible and {dying_descendants} dying",
        path.display()
    ))]
    DelegationTopology {
        path: PathBuf,
        expected_descendants: u64,
        maximum_dying_descendants: u64,
        descendants: u64,
        dying_descendants: u64,
    },

    #[snafu(display("delegated cgroup subtree {} is unexpectedly empty; the Cast supervisor must be populated", path.display()))]
    DelegationSubtreeUnpopulated { path: PathBuf },

    #[snafu(display(
        "Cast supervisor process changed after delegation authentication: expected TGID {expected}, found {found}"
    ))]
    SupervisorProcessChanged { expected: u32, found: u32 },

    #[snafu(display("kernel returned invalid current TGID {found}"))]
    InvalidSupervisorTgid { found: libc::pid_t },

    #[snafu(display(
        "Cast supervisor membership at {} must contain only TGID {expected}; expected_present={expected_present}, first_foreign={first_foreign:?}, unique_members={unique_members}",
        path.display()
    ))]
    SupervisorMembership {
        path: PathBuf,
        expected: u32,
        expected_present: bool,
        first_foreign: Option<u32>,
        unique_members: usize,
    },

    #[snafu(display(
        "derivation cgroup membership at {} must contain only child TGID {expected}; expected_present={expected_present}, first_foreign={first_foreign:?}, unique_members={unique_members}",
        path.display()
    ))]
    LeafMembership {
        path: PathBuf,
        expected: u32,
        expected_present: bool,
        first_foreign: Option<u32>,
        unique_members: usize,
    },

    #[snafu(display(
        "Cast supervisor {} was replaced (expected dev={expected_device} ino={expected_inode}, found dev={found_device} ino={found_inode})",
        path.display()
    ))]
    SupervisorReplaced {
        path: PathBuf,
        expected_device: u64,
        expected_inode: u64,
        found_device: u64,
        found_inode: u64,
    },

    #[snafu(display("delegated cgroup {} is frozen; an activation root must be unfrozen", path.display()))]
    DelegationFrozen { path: PathBuf },

    #[snafu(display(
        "delegated cgroup {} is owned by uid {found_uid}, expected effective uid {expected_uid}",
        path.display()
    ))]
    DelegationOwnerMismatch {
        path: PathBuf,
        expected_uid: libc::uid_t,
        found_uid: libc::uid_t,
    },

    #[snafu(display(
        "delegated cgroup {} has shared write mode {mode:#o}; group/other write access is forbidden",
        path.display()
    ))]
    DelegationSharedWritable { path: PathBuf, mode: libc::mode_t },

    #[snafu(display("delegated cgroup {} is already locked by another supervisor", path.display()))]
    DelegationAlreadyOwned { path: PathBuf },

    #[snafu(display("cgroup leaf identity must be exactly 64 lowercase hexadecimal bytes: {identity:?}"))]
    InvalidLeafIdentity { identity: String },

    #[snafu(display("cgroup limit {field} must be non-zero"))]
    ZeroLimit { field: &'static str },

    #[snafu(display("cgroup pids.max {value} exceeds the supported finite maximum {maximum}"))]
    InvalidPidsMax { value: u64, maximum: u64 },

    #[snafu(display("cgroup cpu.max quota {value} is outside the supported {minimum}..={maximum} microsecond range"))]
    InvalidCpuQuota { value: u64, minimum: u64, maximum: u64 },

    #[snafu(display("cgroup cpu.max period {value} is outside the supported {minimum}..={maximum} microsecond range"))]
    InvalidCpuPeriod { value: u64, minimum: u64, maximum: u64 },

    #[snafu(display("cgroup memory limit {field}={value} is not aligned to the host page size {page_size}"))]
    UnalignedMemoryLimit {
        field: &'static str,
        value: u64,
        page_size: u64,
    },

    #[snafu(display("could not determine a positive host page size (sysconf returned {found})"))]
    InvalidPageSize { found: libc::c_long },

    #[snafu(display("cgroup drain timeout and poll interval must both be non-zero"))]
    InvalidDrainPolicy,

    #[snafu(display(
        "short write to cgroup control {}: wrote {written} of {expected} bytes",
        path.display()
    ))]
    ShortControlWrite {
        path: PathBuf,
        expected: usize,
        written: usize,
    },

    #[snafu(display("cgroup leaf {} became populated before configuration completed", path.display()))]
    LeafPopulatedDuringConfiguration { path: PathBuf },

    #[snafu(display("cgroup leaf {} became frozen before configuration completed", path.display()))]
    LeafFrozenDuringConfiguration { path: PathBuf },

    #[snafu(display("configured cgroup leaf lost its delegated-root removal authority"))]
    RemovalAuthorityUnavailable,

    #[snafu(display(
        "cgroup control verification failed at {}: expected {expected:?}, found {found:?}",
        path.display()
    ))]
    ControlVerification {
        path: PathBuf,
        expected: String,
        found: String,
    },

    #[snafu(display("cgroup {} remained populated after {timeout:?}", path.display()))]
    DrainTimeout { path: PathBuf, timeout: Duration },

    #[snafu(display(
        "cgroup leaf {} was replaced (expected dev={expected_device} ino={expected_inode}, found dev={found_device} ino={found_inode})",
        path.display()
    ))]
    LeafReplaced {
        path: PathBuf,
        expected_device: u64,
        expected_inode: u64,
        found_device: u64,
        found_inode: u64,
    },

    #[snafu(display("cgroup cleanup failed after an earlier failure: {failure}; cleanup: {cleanup}"))]
    CleanupAfterFailure {
        failure: Box<CgroupError>,
        cleanup: Box<CgroupError>,
    },

    #[snafu(display(
        "cgroup cleanup failed after an earlier failure: {failure}; cleanup: {cleanup}; authenticated recovery capability retained"
    ))]
    CleanupRecovery {
        failure: Box<CgroupError>,
        cleanup: Box<CgroupError>,
        recovery: Box<CgroupRecovery>,
    },
}

impl CgroupError {
    /// Borrow the authenticated cleanup capability retained after a failed
    /// setup rollback, if this error owns one.
    pub fn recovery_mut(&mut self) -> Option<&mut CgroupRecovery> {
        match self {
            Self::CleanupRecovery { recovery, .. } => Some(recovery),
            _ => None,
        }
    }

    /// Consume a failed setup error and recover its exact cleanup capability.
    pub fn into_recovery(self) -> Option<CgroupRecovery> {
        match self {
            Self::CleanupRecovery { recovery, .. } => Some(*recovery),
            _ => None,
        }
    }
}

/// Result alias for cgroup operations.
pub type Result<T> = std::result::Result<T, CgroupError>;

include!("cgroup/policy.rs");
include!("cgroup/delegation.rs");
include!("cgroup/lifecycle.rs");
include!("cgroup/topology.rs");

#[cfg(test)]
mod tests;
