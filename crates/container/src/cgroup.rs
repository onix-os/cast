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
        "delegated cgroup topology at {} expected {expected_descendants} visible descendants and {dying_requirement} dying descendants, found {descendants} visible and {dying_descendants} dying",
        path.display()
    ))]
    DelegationTopology {
        path: PathBuf,
        expected_descendants: u64,
        dying_requirement: &'static str,
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

/// Hard aggregate controls written to a newly created cgroup v2 leaf.
///
/// Values are emitted as canonical base-10 integers. This type intentionally
/// has no `max` variant: its purpose is to represent actual hard ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CgroupLimits {
    pids_max: u64,
    memory_max: u64,
    memory_swap_max: u64,
    cpu_quota_micros: u64,
    cpu_period_micros: u64,
}

impl CgroupLimits {
    pub fn new(
        pids_max: u64,
        memory_max: u64,
        memory_swap_max: u64,
        cpu_quota_micros: u64,
        cpu_period_micros: u64,
    ) -> Result<Self> {
        for (field, value) in [
            ("pids.max", pids_max),
            ("memory.max", memory_max),
            ("cpu.max quota", cpu_quota_micros),
            ("cpu.max period", cpu_period_micros),
        ] {
            if value == 0 {
                return Err(CgroupError::ZeroLimit { field });
            }
        }
        if pids_max > MAX_PIDS {
            return Err(CgroupError::InvalidPidsMax {
                value: pids_max,
                maximum: MAX_PIDS,
            });
        }
        if !(MIN_CPU_BANDWIDTH_MICROS..=MAX_CPU_QUOTA_MICROS).contains(&cpu_quota_micros) {
            return Err(CgroupError::InvalidCpuQuota {
                value: cpu_quota_micros,
                minimum: MIN_CPU_BANDWIDTH_MICROS,
                maximum: MAX_CPU_QUOTA_MICROS,
            });
        }
        if !(MIN_CPU_BANDWIDTH_MICROS..=MAX_CPU_PERIOD_MICROS).contains(&cpu_period_micros) {
            return Err(CgroupError::InvalidCpuPeriod {
                value: cpu_period_micros,
                minimum: MIN_CPU_BANDWIDTH_MICROS,
                maximum: MAX_CPU_PERIOD_MICROS,
            });
        }
        let page_size = system_page_size()?;
        for (field, value) in [("memory.max", memory_max), ("memory.swap.max", memory_swap_max)] {
            if value % page_size != 0 {
                return Err(CgroupError::UnalignedMemoryLimit {
                    field,
                    value,
                    page_size,
                });
            }
        }

        Ok(Self {
            pids_max,
            memory_max,
            memory_swap_max,
            cpu_quota_micros,
            cpu_period_micros,
        })
    }

    pub const fn pids_max(self) -> u64 {
        self.pids_max
    }

    pub const fn memory_max(self) -> u64 {
        self.memory_max
    }

    pub const fn memory_swap_max(self) -> u64 {
        self.memory_swap_max
    }

    pub const fn cpu_quota_micros(self) -> u64 {
        self.cpu_quota_micros
    }

    pub const fn cpu_period_micros(self) -> u64 {
        self.cpu_period_micros
    }
}

/// Finite policy used while waiting for a killed cgroup to become empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrainPolicy {
    timeout: Duration,
    poll_interval: Duration,
}

impl DrainPolicy {
    pub fn new(timeout: Duration, poll_interval: Duration) -> Result<Self> {
        if timeout.is_zero() || poll_interval.is_zero() {
            return Err(CgroupError::InvalidDrainPolicy);
        }
        Ok(Self { timeout, poll_interval })
    }

    pub const fn timeout(self) -> Duration {
        self.timeout
    }

    pub const fn poll_interval(self) -> Duration {
        self.poll_interval
    }
}

impl Default for DrainPolicy {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_DRAIN_TIMEOUT,
            poll_interval: DEFAULT_DRAIN_POLL_INTERVAL,
        }
    }
}

/// Parsed `cgroup.events` state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CgroupEvents {
    populated: bool,
    frozen: bool,
}

impl CgroupEvents {
    pub const fn populated(self) -> bool {
        self.populated
    }

    pub const fn frozen(self) -> bool {
        self.frozen
    }
}

#[derive(Debug)]
struct SupervisorAuthority {
    identity_witness: DescriptorIdentity,
    opener_tgid: u32,
}

#[derive(Debug)]
enum DelegationTopology {
    Systemd(SupervisorAuthority),
    #[cfg(test)]
    Simulated,
}

/// The single descriptor-pinned authority for one delegated systemd unit.
///
/// Moving this value between the root, provisional rollback, configured leaf,
/// and recovery types preserves both the advisory lock and the invariant that
/// there is never a second delegated-root descriptor hidden in the lifecycle.
#[derive(Debug)]
struct DelegationAuthority {
    directory: OwnedFd,
    label: PathBuf,
    topology: DelegationTopology,
}

impl DelegationAuthority {
    /// Authenticate the complete supervisor-only topology without requiring
    /// the delegated controllers to have been enabled yet.
    ///
    /// A systemd `Delegate=` + `DelegateSubgroup=` unit may leave every
    /// controller disabled in the delegated root. This probe is therefore
    /// used only during initial acquisition, before Cast performs its one
    /// idempotent mutation. It must remain otherwise identical to steady-state
    /// baseline probe so controller activation can never precede topology
    /// authentication.
    fn probe_pre_enable_baseline(&self) -> Result<BTreeSet<String>> {
        match &self.topology {
            DelegationTopology::Systemd(supervisor) => {
                let enabled = probe_root_authority_pre_enable(&self.directory, &self.label)?;
                require_descendant_topology(&self.directory, &self.label, 1, false)?;
                probe_supervisor(&self.directory, &self.label, supervisor)?;
                require_descendant_topology(&self.directory, &self.label, 1, false)?;
                Ok(enabled)
            }
            #[cfg(test)]
            DelegationTopology::Simulated => Ok(BTreeSet::new()),
        }
    }

    fn probe_baseline(&self) -> Result<()> {
        match &self.topology {
            DelegationTopology::Systemd(supervisor) => {
                probe_root_authority(&self.directory, &self.label)?;
                require_descendant_topology(&self.directory, &self.label, 1, false)?;
                probe_supervisor(&self.directory, &self.label, supervisor)?;
                require_descendant_topology(&self.directory, &self.label, 1, false)
            }
            #[cfg(test)]
            DelegationTopology::Simulated => Ok(()),
        }
    }

    fn probe_ready(&self, leaf: &CgroupLeaf) -> Result<()> {
        match &self.topology {
            DelegationTopology::Systemd(supervisor) => {
                probe_root_authority(&self.directory, &self.label)?;
                require_descendant_topology(&self.directory, &self.label, 2, false)?;
                probe_supervisor(&self.directory, &self.label, supervisor)?;
                probe_leaf(&self.directory, leaf)?;
                require_descendant_topology(&self.directory, &self.label, 2, false)
            }
            #[cfg(test)]
            DelegationTopology::Simulated => probe_leaf_witness(&self.directory, leaf),
        }
    }

    fn probe_activated(&self, leaf: &CgroupLeaf, expected_tgid: u32) -> Result<()> {
        match &self.topology {
            DelegationTopology::Systemd(supervisor) => {
                probe_root_authority(&self.directory, &self.label)?;
                require_descendant_topology(&self.directory, &self.label, 2, false)?;
                probe_supervisor(&self.directory, &self.label, supervisor)?;
                probe_activated_leaf(&self.directory, leaf, expected_tgid)?;
                require_descendant_topology(&self.directory, &self.label, 2, false)
            }
            #[cfg(test)]
            DelegationTopology::Simulated => {
                probe_leaf_witness(&self.directory, leaf)?;
                require_exact_leaf_membership(
                    &read_pid_list(&leaf.directory, c"cgroup.procs", &leaf.label)?,
                    expected_tgid,
                    &leaf.label.join("cgroup.procs"),
                )
            }
        }
    }

    /// A removed cgroup may retain dying controller state temporarily. Verify
    /// the visible tree is back to the supervisor-only baseline without
    /// converting normal asynchronous CSS release into a false cleanup error.
    fn probe_cleanup_baseline(&self) -> Result<()> {
        match &self.topology {
            DelegationTopology::Systemd(supervisor) => {
                probe_root_authority(&self.directory, &self.label)?;
                require_descendant_topology(&self.directory, &self.label, 1, true)?;
                probe_supervisor(&self.directory, &self.label, supervisor)?;
                require_descendant_topology(&self.directory, &self.label, 1, true)
            }
            #[cfg(test)]
            DelegationTopology::Simulated => Ok(()),
        }
    }
}

/// Authenticated, linear capability for a delegated cgroup v2 activation root.
///
/// The root itself is an internal domain with no direct processes. systemd
/// places this Cast process in the fixed `cast-supervisor` child through
/// `DelegateSubgroup=cast-supervisor`; the only other permitted child is the
/// one derivation leaf created by consuming this value.
pub struct DelegatedCgroupRoot {
    authority: DelegationAuthority,
}

impl DelegatedCgroupRoot {
    /// Open and exclusively lock one supervisor-only delegated subtree.
    ///
    /// `mount_point` is opened without following any symlink component. The
    /// expected cgroup mount transition is allowed for that first open; every
    /// subsequent lookup below its descriptor additionally rejects mount
    /// crossings with `RESOLVE_NO_XDEV`.
    ///
    /// The delegated directory must be owned by the effective UID and must not
    /// be group/other writable. A non-blocking advisory lock rejects a second
    /// cooperating supervisor. Linux offers no mandatory directory lock, so
    /// the caller must also ensure that no uncooperative same-UID process or
    /// container payload can reach this subtree for the guard's lifetime.
    pub fn open(mount_point: impl AsRef<Path>, delegated_relative: impl AsRef<Path>) -> Result<Self> {
        let mount_point = normalized_absolute(mount_point.as_ref())?;
        let delegated_relative = normalized_relative(delegated_relative.as_ref())?;
        let mount_name = path_cstring(&mount_point)
            .map_err(|source| descriptor_error("encode cgroup mount path", &mount_point, source))?;
        let mount = openat2(
            libc::AT_FDCWD,
            &mount_name,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
        )
        .map_err(|source| descriptor_error("open cgroup v2 mount", &mount_point, source))?;
        require_cgroup2(&mount, &mount_point)?;

        let relative_name = path_cstring(&delegated_relative)
            .map_err(|source| descriptor_error("encode delegated cgroup path", &delegated_relative, source))?;
        let label = mount_point.join(&delegated_relative);
        let directory = openat2(
            mount.as_raw_fd(),
            &relative_name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            ANCHORED_RESOLUTION,
        )
        .map_err(|source| descriptor_error("open delegated cgroup", &label, source))?;
        require_directory(&directory, &label)?;
        require_cgroup2(&directory, &label)?;

        // A correct systemd `Delegate=` + `DelegateSubgroup=` unit may start
        // with an empty `cgroup.subtree_control`. Authenticate the root and
        // exact supervisor topology before Cast enables anything in it.
        probe_root_authority_pre_enable(&directory, &label)?;
        require_descendant_topology(&directory, &label, 1, false)?;
        let supervisor = capture_supervisor(&directory, &label)?;
        require_descendant_topology(&directory, &label, 1, false)?;
        let authority = DelegationAuthority {
            directory,
            label,
            topology: DelegationTopology::Systemd(supervisor),
        };

        // Repeat the complete pre-mutation authentication through the stored
        // identity witness. Only the exact missing required controllers are
        // then enabled through the pinned root descriptor. A subsequent
        // steady-state probe verifies both effective controls and topology.
        let enabled = authority.probe_pre_enable_baseline()?;
        enable_required_controllers(&authority.directory, &authority.label, &enabled)?;
        let root = Self { authority };
        root.probe()?;
        Ok(root)
    }

    /// Diagnostic pathname retained only for errors and logs.
    pub fn label(&self) -> &Path {
        &self.authority.label
    }

    /// Revalidate the stable delegation contract without mutating it.
    pub fn probe(&self) -> Result<()> {
        self.authority.probe_baseline()
    }

    /// Consume this delegation to create its one per-derivation leaf.
    pub fn create_leaf(self, identity: &str, limits: CgroupLimits) -> Result<CgroupLeaf> {
        self.probe()?;
        configure_created_leaf(self.create_unconfigured_leaf(identity)?, limits)
    }

    fn create_unconfigured_leaf(self, identity: &str) -> Result<CgroupLeaf> {
        self.create_unconfigured_leaf_with(identity, &mut |_| Ok(()))
    }

    fn create_unconfigured_leaf_with(
        self,
        identity: &str,
        checkpoint: &mut dyn FnMut(CreationStage) -> io::Result<()>,
    ) -> Result<CgroupLeaf> {
        validate_leaf_identity(identity)?;
        let (name, label) = self.create_unique_leaf_directory(identity)?;
        let Self { authority } = self;

        // The sole locked root authority moves into rollback immediately after
        // mkdir. No descriptor allocation or duplication is needed here.
        let mut rollback = ProvisionalLeafRollback::new(authority, name, label);
        let setup = (|| {
            creation_checkpoint(checkpoint, CreationStage::Mkdir, &rollback.label)?;
            let directory = open_control_path(
                &rollback.authority.directory,
                &rollback.name,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
            .map_err(|source| descriptor_error("pin newly created cgroup leaf", &rollback.label, source))?;
            creation_checkpoint(checkpoint, CreationStage::Pinned, &rollback.label)?;
            require_directory(&directory, &rollback.label)?;
            let identity_witness = descriptor_identity(&directory, &rollback.label)?;
            rollback.authenticate(identity_witness);
            creation_checkpoint(checkpoint, CreationStage::Witnessed, &rollback.label)?;
            creation_checkpoint(checkpoint, CreationStage::AuthorityTransferred, &rollback.label)?;
            Ok((directory, identity_witness))
        })();

        match setup {
            Ok((directory, identity_witness)) => {
                let (authority, name, label) = rollback.disarm();
                Ok(CgroupLeaf {
                    authority: Some(authority),
                    directory,
                    name,
                    label,
                    identity: identity.to_owned(),
                    identity_witness,
                    active: true,
                    drop_cleanup_enabled: true,
                })
            }
            Err(failure) => Err(rollback.rollback_after(failure)),
        }
    }

    fn create_unique_leaf_directory(&self, identity: &str) -> Result<(CString, PathBuf)> {
        let mut last_collision = None;
        for _ in 0..LEAF_CREATE_ATTEMPTS {
            let name = random_leaf_name(identity)
                .map_err(|source| descriptor_error("generate unpredictable cgroup leaf name", self.label(), source))?;
            let label = self.label().join(os_str(&name));

            // SAFETY: directory and name remain live and mode is valid.
            if unsafe { libc::mkdirat(self.authority.directory.as_raw_fd(), name.as_ptr(), 0o700) } == -1 {
                let source = io::Error::last_os_error();
                if source.kind() == io::ErrorKind::AlreadyExists {
                    last_collision = Some(source);
                    continue;
                }
                return Err(descriptor_error(
                    "create cgroup leaf without replacement",
                    &label,
                    source,
                ));
            }
            return Ok((name, label));
        }

        Err(descriptor_error(
            "create unique unpredictable cgroup leaf",
            self.label(),
            last_collision.unwrap_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "leaf-name collision")),
        ))
    }

    #[cfg(test)]
    fn simulated(directory: &File, label: PathBuf) -> Self {
        Self {
            authority: DelegationAuthority {
                directory: duplicate_cloexec(directory).unwrap(),
                label,
                topology: DelegationTopology::Simulated,
            },
        }
    }
}

fn configure_created_leaf(mut leaf: CgroupLeaf, limits: CgroupLimits) -> Result<CgroupLeaf> {
    if let Err(failure) = leaf.configure(limits).and_then(|()| leaf.probe_ready_topology()) {
        leaf.drop_cleanup_enabled = false;
        return match leaf.remove_authenticated() {
            Ok(()) => match leaf.probe_cleanup_baseline() {
                Ok(()) => Err(failure),
                Err(cleanup) => Err(CgroupError::CleanupAfterFailure {
                    failure: Box::new(failure),
                    cleanup: Box::new(cleanup),
                }),
            },
            Err(cleanup) => match leaf.into_recovery() {
                Ok(recovery) => Err(CgroupError::CleanupRecovery {
                    failure: Box::new(failure),
                    cleanup: Box::new(cleanup),
                    recovery: Box::new(recovery),
                }),
                Err(authority) => Err(CgroupError::CleanupAfterFailure {
                    failure: Box::new(CgroupError::CleanupAfterFailure {
                        failure: Box::new(failure),
                        cleanup: Box::new(cleanup),
                    }),
                    cleanup: Box::new(authority),
                }),
            },
        };
    }
    Ok(leaf)
}

/// Authenticated authority to retry removal of one setup-time cgroup leaf.
///
/// This value is returned only when automatic setup rollback itself failed.
/// It owns the delegated-root lock and the unpredictable leaf name. Dropping
/// it performs no syscall: a supervisor must explicitly retry or quarantine
/// the delegation rather than receiving an unreported cleanup attempt.
#[derive(Debug)]
pub struct CgroupRecovery {
    authority: DelegationAuthority,
    name: CString,
    label: PathBuf,
    identity_witness: Option<DescriptorIdentity>,
    active: bool,
}

impl CgroupRecovery {
    fn new(
        authority: DelegationAuthority,
        name: CString,
        label: PathBuf,
        identity_witness: Option<DescriptorIdentity>,
    ) -> Self {
        Self {
            authority,
            name,
            label,
            identity_witness,
            active: true,
        }
    }

    pub fn label(&self) -> &Path {
        &self.label
    }

    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Reopen, authenticate, and remove the exact empty leaf.
    ///
    /// A missing initial witness is possible only for a failure immediately
    /// after the exclusive `mkdirat`. The delegation contract excludes any
    /// uncooperative same-UID actor from reaching that unpredictable name.
    pub fn retry_remove(&mut self) -> Result<()> {
        if !self.active {
            // Removal may already have succeeded while the asynchronous
            // topology verification failed. Retrying must revalidate the
            // supervisor-only baseline rather than silently treating that
            // earlier error as final success.
            return self.authority.probe_cleanup_baseline();
        }
        let identity_witness = match self.identity_witness {
            Some(identity_witness) => identity_witness,
            None => {
                let pinned = open_control_path(
                    &self.authority.directory,
                    &self.name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
                .map_err(|source| descriptor_error("pin provisional cgroup leaf for recovery", &self.label, source))?;
                require_directory(&pinned, &self.label)?;
                let identity_witness = descriptor_identity(&pinned, &self.label)?;
                self.identity_witness = Some(identity_witness);
                identity_witness
            }
        };
        remove_named_authenticated(
            &self.authority.directory,
            &self.name,
            &self.label,
            identity_witness,
            "retry authenticated cgroup leaf cleanup",
        )?;
        self.active = false;
        self.authority.probe_cleanup_baseline()
    }
}

/// Owned lifecycle guard for one configured per-derivation cgroup leaf.
#[derive(Debug)]
pub struct CgroupLeaf {
    authority: Option<DelegationAuthority>,
    directory: OwnedFd,
    name: CString,
    label: PathBuf,
    identity: String,
    identity_witness: DescriptorIdentity,
    active: bool,
    drop_cleanup_enabled: bool,
}

impl CgroupLeaf {
    fn authority(&self) -> Result<&DelegationAuthority> {
        self.authority.as_ref().ok_or(CgroupError::RemovalAuthorityUnavailable)
    }

    fn into_recovery(mut self) -> Result<CgroupRecovery> {
        self.drop_cleanup_enabled = false;
        self.active = false;
        let authority = self.authority.take().ok_or(CgroupError::RemovalAuthorityUnavailable)?;
        Ok(CgroupRecovery::new(
            authority,
            self.name.clone(),
            self.label.clone(),
            Some(self.identity_witness),
        ))
    }

    fn probe_ready_topology(&self) -> Result<()> {
        self.authority()?.probe_ready(self)
    }

    fn probe_cleanup_baseline(&self) -> Result<()> {
        self.authority()?.probe_cleanup_baseline()
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn label(&self) -> &Path {
        &self.label
    }

    /// Borrow the pinned cgroup directory for crate-owned atomic placement.
    ///
    /// This is intentionally not public API. Numeric writes to `cgroup.procs`
    /// cannot authenticate a process because Linux may recycle a PID between
    /// observation and write. The future `Container` integration must pass
    /// this descriptor to `clone3(CLONE_INTO_CGROUP | CLONE_PIDFD)` instead and
    /// close all inherited cgroup capabilities in the child before payload code
    /// can run.
    #[allow(dead_code)]
    pub(crate) fn placement(&self) -> Result<CgroupPlacement<'_>> {
        self.probe_ready_topology()?;
        let authority = self.authority()?;
        Ok(CgroupPlacement {
            root: authority.directory.as_fd(),
            target: self.directory.as_fd(),
        })
    }

    /// Revalidate atomic placement before releasing the clone child.
    ///
    /// At this point the child is still blocked on the setup pipe, so its
    /// unique TGID must be the leaf's complete membership. This closes the
    /// gap between a successful `CLONE_INTO_CGROUP` return and untrusted setup
    /// by rejecting a missing, duplicated-foreign, or pre-populated target.
    pub(crate) fn require_sole_member(&self, expected_tgid: u32) -> Result<()> {
        self.authority()?.probe_activated(self, expected_tgid)
    }

    /// Read and strictly parse the leaf's current core event state.
    pub fn events(&self) -> Result<CgroupEvents> {
        read_events(&self.directory, &self.label)
    }

    /// Ask the kernel to SIGKILL every process in this cgroup subtree.
    pub fn kill(&self) -> Result<()> {
        write_control(&self.directory, c"cgroup.kill", b"1", &self.label)
    }

    /// Boundedly wait until `cgroup.events` reports `populated 0`.
    pub fn wait_until_empty(&self, policy: DrainPolicy) -> Result<()> {
        let started = Instant::now();
        loop {
            if !self.events()?.populated() {
                return Ok(());
            }
            let elapsed = started.elapsed();
            if elapsed >= policy.timeout {
                return Err(CgroupError::DrainTimeout {
                    path: self.label.clone(),
                    timeout: policy.timeout,
                });
            }
            thread::sleep(policy.poll_interval.min(policy.timeout.saturating_sub(elapsed)));
        }
    }

    /// Kill, drain, and remove this exact leaf, returning cleanup failures.
    pub fn kill_and_remove(&mut self, policy: DrainPolicy) -> Result<()> {
        // This explicit operation is authoritative. If it fails, Drop must not
        // silently retry with a different timeout, but the caller retains this
        // authenticated capability for an explicit retry or quarantine.
        self.drop_cleanup_enabled = false;
        self.cleanup(policy)
    }

    /// Remove a configured leaf when no clone child was created.
    ///
    /// This path is used for parent-side preparation or `clone3` failures. It
    /// must not issue `cgroup.kill`: population at this stage is an invariant
    /// violation rather than a process tree that the caller knowingly owns.
    pub(crate) fn remove_unstarted(&mut self) -> Result<()> {
        self.drop_cleanup_enabled = false;
        if !self.active {
            return self.probe_cleanup_baseline();
        }
        self.require_empty_for_configuration()?;
        self.remove_authenticated()?;
        self.probe_cleanup_baseline()
    }

    fn configure(&self, limits: CgroupLimits) -> Result<()> {
        self.require_empty_for_configuration()?;
        write_control(
            &self.directory,
            c"pids.max",
            limits.pids_max.to_string().as_bytes(),
            &self.label,
        )?;
        write_control(
            &self.directory,
            c"memory.max",
            limits.memory_max.to_string().as_bytes(),
            &self.label,
        )?;
        write_control(
            &self.directory,
            c"memory.swap.max",
            limits.memory_swap_max.to_string().as_bytes(),
            &self.label,
        )?;
        write_control(&self.directory, c"memory.oom.group", b"1", &self.label)?;
        // A derivation is a terminal resource domain. Prevent payload-created
        // cgroup subtrees from consuming unbounded kernel metadata or keeping
        // authenticated leaf removal busy after every process has exited.
        write_control(&self.directory, c"cgroup.max.depth", b"0", &self.label)?;
        write_control(&self.directory, c"cgroup.max.descendants", b"0", &self.label)?;
        // Upstream Linux 5.14 exposes cpu.max.burst together with cpu.max.
        // Accept exact absence for custom or selectively backported kernels:
        // absence preserves the kernel's no-burst behavior, while every
        // present control is still authenticated, written, and read back.
        let cpu_max_burst_present = write_control_if_present(&self.directory, c"cpu.max.burst", b"0", &self.label)?;
        let cpu_max = format!("{} {}", limits.cpu_quota_micros, limits.cpu_period_micros);
        write_control(&self.directory, c"cpu.max", cpu_max.as_bytes(), &self.label)?;

        self.require_empty_for_configuration()?;
        self.verify_configured_controls(limits, cpu_max_burst_present)?;
        self.require_activation_controls()
    }

    fn require_empty_for_configuration(&self) -> Result<()> {
        let events = self.events()?;
        if events.populated() {
            Err(CgroupError::LeafPopulatedDuringConfiguration {
                path: self.label.join("cgroup.events"),
            })
        } else if events.frozen() {
            Err(CgroupError::LeafFrozenDuringConfiguration {
                path: self.label.join("cgroup.events"),
            })
        } else {
            Ok(())
        }
    }

    fn verify_configured_controls(&self, limits: CgroupLimits, cpu_max_burst_present: bool) -> Result<()> {
        verify_control(&self.directory, c"pids.max", &limits.pids_max.to_string(), &self.label)?;
        verify_control(
            &self.directory,
            c"memory.max",
            &limits.memory_max.to_string(),
            &self.label,
        )?;
        verify_control(
            &self.directory,
            c"memory.swap.max",
            &limits.memory_swap_max.to_string(),
            &self.label,
        )?;
        verify_control(&self.directory, c"memory.oom.group", "1", &self.label)?;
        verify_control(&self.directory, c"cgroup.max.depth", "0", &self.label)?;
        verify_control(&self.directory, c"cgroup.max.descendants", "0", &self.label)?;
        if cpu_max_burst_present {
            verify_control(&self.directory, c"cpu.max.burst", "0", &self.label)?;
        }
        verify_control(
            &self.directory,
            c"cpu.max",
            &format!("{} {}", limits.cpu_quota_micros, limits.cpu_period_micros),
            &self.label,
        )
    }

    fn require_activation_controls(&self) -> Result<()> {
        // Atomic CLONE_INTO_CGROUP placement still requires migration access
        // to this leaf, and every post-activation error path depends on the
        // race-safe subtree kill primitive. Prove both capabilities while the
        // leaf is empty, before lending its placement descriptor.
        drop(open_owned_writable_control(
            &self.directory,
            c"cgroup.procs",
            &self.label,
        )?);
        drop(open_owned_writable_control(
            &self.directory,
            c"cgroup.threads",
            &self.label,
        )?);
        drop(open_owned_writable_control(
            &self.directory,
            c"cgroup.kill",
            &self.label,
        )?);
        self.require_empty_for_configuration()
    }

    fn cleanup(&mut self, policy: DrainPolicy) -> Result<()> {
        if !self.active {
            return self.probe_cleanup_baseline();
        }

        let mut failure = self.kill().err();
        let drained = self.wait_until_empty(policy);
        if let Err(error) = drained {
            append_failure(&mut failure, error);
        } else {
            match self.remove_authenticated() {
                Err(error) => append_failure(&mut failure, error),
                Ok(()) => {
                    if let Err(error) = self.probe_cleanup_baseline() {
                        append_failure(&mut failure, error);
                    }
                }
            }
        }

        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Reopen and authenticate the witnessed leaf, then directly remove it.
    ///
    /// cgroup v2 rejects rename, so `unlinkat(AT_REMOVEDIR)` must address the
    /// original name. Linux has no conditional-rmdir syscall: the advisory
    /// delegated-root lock and caller's exclusive-ownership guarantee are what
    /// make the final precheck/remove sequence valid.
    fn remove_authenticated(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }

        self.probe_ready_topology()?;
        remove_named_authenticated(
            &self.authority()?.directory,
            &self.name,
            &self.label,
            self.identity_witness,
            "remove authenticated empty cgroup leaf",
        )?;
        self.active = false;
        Ok(())
    }
}

impl Drop for CgroupLeaf {
    fn drop(&mut self) {
        if self.active && self.drop_cleanup_enabled {
            // Drop never blocks for the default drain timeout. It issues the
            // kill, observes events once, and removes only an already-empty
            // authenticated leaf. A populated leaf is deliberately left for a
            // supervisor-owned reaper rather than hidden latency in Drop.
            let _ = self.kill();
            if matches!(self.events(), Ok(events) if !events.populated()) {
                let _ = self.remove_authenticated();
            }
        }
    }
}

/// Crate-private capability intended for `clone3(CLONE_INTO_CGROUP)`.
#[allow(dead_code)]
pub(crate) struct CgroupPlacement<'a> {
    root: BorrowedFd<'a>,
    target: BorrowedFd<'a>,
}

impl CgroupPlacement<'_> {
    /// The delegated root is retained only for authenticated cleanup. The
    /// clone child must close its copied descriptor before trusted setup runs.
    #[allow(dead_code)]
    pub(crate) fn root(&self) -> BorrowedFd<'_> {
        self.root
    }

    /// Directory descriptor passed to `clone3(CLONE_INTO_CGROUP)`.
    #[allow(dead_code)]
    pub(crate) fn target(&self) -> BorrowedFd<'_> {
        self.target
    }

    /// Both cgroup capabilities copied by clone, in deterministic close order.
    #[allow(dead_code)]
    pub(crate) fn inherited_raw_fds(&self) -> [RawFd; 2] {
        [self.root.as_raw_fd(), self.target.as_raw_fd()]
    }
}

impl AsFd for CgroupPlacement<'_> {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.target
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DescriptorIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreationStage {
    Mkdir,
    Pinned,
    Witnessed,
    AuthorityTransferred,
}

struct ProvisionalLeafRollback {
    authority: DelegationAuthority,
    name: CString,
    label: PathBuf,
    identity_witness: Option<DescriptorIdentity>,
}

impl ProvisionalLeafRollback {
    fn new(authority: DelegationAuthority, name: CString, label: PathBuf) -> Self {
        Self {
            authority,
            name,
            label,
            identity_witness: None,
        }
    }

    fn authenticate(&mut self, identity_witness: DescriptorIdentity) {
        self.identity_witness = Some(identity_witness);
    }

    fn disarm(self) -> (DelegationAuthority, CString, PathBuf) {
        (self.authority, self.name, self.label)
    }

    fn rollback_after(mut self, failure: CgroupError) -> CgroupError {
        match self.rollback() {
            Ok(()) => match self.authority.probe_cleanup_baseline() {
                Ok(()) => failure,
                Err(cleanup) => CgroupError::CleanupAfterFailure {
                    failure: Box::new(failure),
                    cleanup: Box::new(cleanup),
                },
            },
            Err(cleanup) => CgroupError::CleanupRecovery {
                failure: Box::new(failure),
                cleanup: Box::new(cleanup),
                recovery: Box::new(CgroupRecovery::new(
                    self.authority,
                    self.name,
                    self.label,
                    self.identity_witness,
                )),
            },
        }
    }

    fn rollback(&mut self) -> Result<()> {
        let identity_witness = match self.identity_witness {
            Some(identity_witness) => identity_witness,
            None => {
                // No fallible operation occurs between mkdir and arming this
                // guard. Under the locked-root ownership contract, pinning the
                // unpredictable name now witnesses the directory just created.
                let pinned = open_control_path(
                    &self.authority.directory,
                    &self.name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
                .map_err(|source| descriptor_error("pin provisional cgroup leaf for rollback", &self.label, source))?;
                require_directory(&pinned, &self.label)?;
                let identity_witness = descriptor_identity(&pinned, &self.label)?;
                self.identity_witness = Some(identity_witness);
                identity_witness
            }
        };
        remove_named_authenticated(
            &self.authority.directory,
            &self.name,
            &self.label,
            identity_witness,
            "roll back authenticated provisional cgroup leaf",
        )
    }
}

fn creation_checkpoint(
    checkpoint: &mut dyn FnMut(CreationStage) -> io::Result<()>,
    stage: CreationStage,
    label: &Path,
) -> Result<()> {
    checkpoint(stage).map_err(|source| descriptor_error("cgroup leaf creation checkpoint", label, source))
}

fn remove_named_authenticated(
    root: &OwnedFd,
    name: &CStr,
    label: &Path,
    expected: DescriptorIdentity,
    operation: &'static str,
) -> Result<()> {
    let pinned = open_control_path(
        root,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    )
    .map_err(|source| descriptor_error("reopen named cgroup leaf for removal", label, source))?;
    require_directory(&pinned, label)?;
    let found = descriptor_identity(&pinned, label)?;
    if found != expected {
        return Err(CgroupError::LeafReplaced {
            path: label.to_owned(),
            expected_device: expected.device,
            expected_inode: expected.inode,
            found_device: found.device,
            found_inode: found.inode,
        });
    }

    // SAFETY: root and the authenticated single-component name remain live.
    // The exclusive-root contract prevents a legitimate mutation between this
    // witness check and unlinkat; Linux has no atomic conditional-rmdir API.
    if unsafe { libc::unlinkat(root.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } == -1 {
        Err(descriptor_error(operation, label, io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn append_failure(failure: &mut Option<CgroupError>, next: CgroupError) {
    *failure = Some(match failure.take() {
        Some(previous) => CgroupError::CleanupAfterFailure {
            failure: Box::new(previous),
            cleanup: Box::new(next),
        },
        None => next,
    });
}

fn normalized_absolute(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(CgroupError::InvalidMountPath { path: path.to_owned() });
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(component) => normalized.push(component),
            Component::Prefix(_) | Component::CurDir | Component::ParentDir => {
                return Err(CgroupError::InvalidMountPath { path: path.to_owned() });
            }
        }
    }
    if normalized != path {
        return Err(CgroupError::InvalidMountPath { path: path.to_owned() });
    }
    Ok(normalized)
}

fn normalized_relative(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() || path.as_os_str().is_empty() {
        return Err(CgroupError::InvalidDelegatedPath { path: path.to_owned() });
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
                return Err(CgroupError::InvalidDelegatedPath { path: path.to_owned() });
            }
        }
    }
    if normalized.as_os_str().is_empty() || normalized != path {
        return Err(CgroupError::InvalidDelegatedPath { path: path.to_owned() });
    }
    Ok(normalized)
}

fn validate_leaf_identity(identity: &str) -> Result<()> {
    if identity.len() == 64
        && identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(CgroupError::InvalidLeafIdentity {
            identity: identity.to_owned(),
        })
    }
}

fn system_page_size() -> Result<u64> {
    // SAFETY: sysconf has no pointer arguments and `_SC_PAGESIZE` is valid.
    let found = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(found)
        .ok()
        .filter(|page_size| *page_size > 0)
        .ok_or(CgroupError::InvalidPageSize { found })
}

fn random_leaf_name(identity: &str) -> io::Result<CString> {
    let mut random = [0_u8; LEAF_RANDOM_BYTES];
    let mut filled = 0;
    let mut interrupted = 0;
    while filled < random.len() {
        // SAFETY: the remaining slice is writable for exactly the supplied
        // length; getrandom retains no pointer after returning.
        let result = unsafe { libc::getrandom(random[filled..].as_mut_ptr().cast(), random.len() - filled, 0) };
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted && interrupted < MAX_GETRANDOM_EINTR_RETRIES {
                interrupted += 1;
                continue;
            }
            return Err(source);
        }
        let read = usize::try_from(result).map_err(|_| io::Error::other("getrandom returned an invalid length"))?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "getrandom returned no cgroup leaf-name entropy",
            ));
        }
        filled += read;
    }

    let suffix = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    CString::new(format!("{LEAF_NAME_PREFIX}{identity}-{suffix}"))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "generated cgroup leaf name contains NUL"))
}

fn current_tgid() -> Result<u32> {
    // SAFETY: getpid has no arguments and cannot fail on Linux.
    let found = unsafe { libc::getpid() };
    u32::try_from(found)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or(CgroupError::InvalidSupervisorTgid { found })
}

fn capture_supervisor(root: &OwnedFd, root_label: &Path) -> Result<SupervisorAuthority> {
    let opener_tgid = current_tgid()?;
    let (supervisor, label) = open_supervisor(root, root_label)?;
    probe_supervisor_descriptor(&supervisor, &label, opener_tgid)?;
    Ok(SupervisorAuthority {
        identity_witness: descriptor_identity(&supervisor, &label)?,
        opener_tgid,
    })
}

fn probe_root_authority(directory: &OwnedFd, label: &Path) -> Result<()> {
    let enabled = probe_root_authority_pre_enable(directory, label)?;
    require_controllers(&enabled, &label.join("cgroup.subtree_control"))
}

/// Authenticate every delegated-root invariant except the initially-empty
/// enabled-controller set, returning that set to the one-time activation
/// path. No caller may mutate `cgroup.subtree_control` until the surrounding
/// supervisor topology has also been authenticated.
fn probe_root_authority_pre_enable(directory: &OwnedFd, label: &Path) -> Result<BTreeSet<String>> {
    require_directory(directory, label)?;
    require_cgroup2(directory, label)?;
    // Recheck owner/mode and reassert the same open-file-description lock on
    // every probe, not only at initial acquisition.
    acquire_exclusive_delegation(directory, label)?;
    require_domain(directory, label)?;

    let available = read_word_set(directory, c"cgroup.controllers", label)?;
    require_controllers(&available, &label.join("cgroup.controllers"))?;
    let enabled = read_word_set(directory, c"cgroup.subtree_control", label)?;

    let members = read_pid_list(directory, c"cgroup.procs", label)?;
    if let Some(pid) = members.first() {
        return Err(CgroupError::DelegationPopulated {
            path: label.join("cgroup.procs"),
            pid: *pid,
        });
    }

    // Authenticate both process and thread migration controls even though the
    // accepted topology is a domain hierarchy. This prevents a separately
    // delegated threaded-migration authority from being shared behind the
    // directory's otherwise-private mode bits.
    drop(open_owned_writable_control(directory, c"cgroup.procs", label)?);
    drop(open_owned_writable_control(directory, c"cgroup.threads", label)?);
    drop(open_owned_writable_control(
        directory,
        c"cgroup.subtree_control",
        label,
    )?);
    require_populated_unfrozen_delegation(read_events(directory, label)?, &label.join("cgroup.events"))?;
    Ok(enabled)
}

fn probe_supervisor(root: &OwnedFd, root_label: &Path, expected: &SupervisorAuthority) -> Result<()> {
    let found_tgid = current_tgid()?;
    if found_tgid != expected.opener_tgid {
        return Err(CgroupError::SupervisorProcessChanged {
            expected: expected.opener_tgid,
            found: found_tgid,
        });
    }

    let (supervisor, label) = open_supervisor(root, root_label)?;
    let found = descriptor_identity(&supervisor, &label)?;
    if found != expected.identity_witness {
        return Err(CgroupError::SupervisorReplaced {
            path: label,
            expected_device: expected.identity_witness.device,
            expected_inode: expected.identity_witness.inode,
            found_device: found.device,
            found_inode: found.inode,
        });
    }
    probe_supervisor_descriptor(&supervisor, &label, expected.opener_tgid)
}

fn open_supervisor(root: &OwnedFd, root_label: &Path) -> Result<(OwnedFd, PathBuf)> {
    let label = root_label.join(os_str(SUPERVISOR_NAME));
    let supervisor = open_control_path(
        root,
        SUPERVISOR_NAME,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    )
    .map_err(|source| descriptor_error("open fixed Cast supervisor subgroup", &label, source))?;
    Ok((supervisor, label))
}

fn probe_supervisor_descriptor(directory: &OwnedFd, label: &Path, expected_tgid: u32) -> Result<()> {
    require_directory(directory, label)?;
    require_cgroup2(directory, label)?;
    require_owned_private(directory, label)?;
    require_domain(directory, label)?;
    require_exact_supervisor_membership(
        &read_pid_list(directory, c"cgroup.procs", label)?,
        expected_tgid,
        &label.join("cgroup.procs"),
    )?;
    drop(open_owned_writable_control(directory, c"cgroup.procs", label)?);
    drop(open_owned_writable_control(directory, c"cgroup.threads", label)?);
    drop(open_owned_writable_control(
        directory,
        c"cgroup.subtree_control",
        label,
    )?);
    require_populated_unfrozen_delegation(read_events(directory, label)?, &label.join("cgroup.events"))?;
    require_descendant_topology(directory, label, 0, false)
}

fn probe_leaf(root: &OwnedFd, leaf: &CgroupLeaf) -> Result<()> {
    probe_leaf_witness(root, leaf)?;
    require_directory(&leaf.directory, &leaf.label)?;
    require_cgroup2(&leaf.directory, &leaf.label)?;
    require_owned_private(&leaf.directory, &leaf.label)?;
    require_domain(&leaf.directory, &leaf.label)?;
    let members = read_pid_list(&leaf.directory, c"cgroup.procs", &leaf.label)?;
    if !members.is_empty() {
        return Err(CgroupError::LeafPopulatedDuringConfiguration {
            path: leaf.label.join("cgroup.procs"),
        });
    }
    drop(open_owned_writable_control(
        &leaf.directory,
        c"cgroup.procs",
        &leaf.label,
    )?);
    drop(open_owned_writable_control(
        &leaf.directory,
        c"cgroup.threads",
        &leaf.label,
    )?);
    drop(open_owned_writable_control(
        &leaf.directory,
        c"cgroup.kill",
        &leaf.label,
    )?);
    require_empty_unfrozen_delegation(
        read_events(&leaf.directory, &leaf.label)?,
        &leaf.label.join("cgroup.events"),
    )?;
    require_descendant_topology(&leaf.directory, &leaf.label, 0, false)?;
    probe_leaf_witness(root, leaf)
}

fn probe_activated_leaf(root: &OwnedFd, leaf: &CgroupLeaf, expected_tgid: u32) -> Result<()> {
    probe_leaf_witness(root, leaf)?;
    require_directory(&leaf.directory, &leaf.label)?;
    require_cgroup2(&leaf.directory, &leaf.label)?;
    require_owned_private(&leaf.directory, &leaf.label)?;
    require_domain(&leaf.directory, &leaf.label)?;
    require_exact_leaf_membership(
        &read_pid_list(&leaf.directory, c"cgroup.procs", &leaf.label)?,
        expected_tgid,
        &leaf.label.join("cgroup.procs"),
    )?;
    drop(open_owned_writable_control(
        &leaf.directory,
        c"cgroup.procs",
        &leaf.label,
    )?);
    drop(open_owned_writable_control(
        &leaf.directory,
        c"cgroup.threads",
        &leaf.label,
    )?);
    drop(open_owned_writable_control(
        &leaf.directory,
        c"cgroup.kill",
        &leaf.label,
    )?);
    require_populated_unfrozen_delegation(
        read_events(&leaf.directory, &leaf.label)?,
        &leaf.label.join("cgroup.events"),
    )?;
    require_descendant_topology(&leaf.directory, &leaf.label, 0, false)?;
    probe_leaf_witness(root, leaf)
}

fn probe_leaf_witness(root: &OwnedFd, leaf: &CgroupLeaf) -> Result<()> {
    let pinned = open_control_path(
        root,
        &leaf.name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    )
    .map_err(|source| descriptor_error("reopen named cgroup leaf for topology probe", &leaf.label, source))?;
    require_directory(&pinned, &leaf.label)?;
    let found = descriptor_identity(&pinned, &leaf.label)?;
    if found == leaf.identity_witness {
        Ok(())
    } else {
        Err(CgroupError::LeafReplaced {
            path: leaf.label.clone(),
            expected_device: leaf.identity_witness.device,
            expected_inode: leaf.identity_witness.inode,
            found_device: found.device,
            found_inode: found.inode,
        })
    }
}

fn require_domain(directory: &OwnedFd, label: &Path) -> Result<()> {
    let group_type = read_single_value(directory, c"cgroup.type", label)?;
    if group_type == "domain" {
        Ok(())
    } else {
        Err(CgroupError::InvalidCgroupType {
            path: label.join("cgroup.type"),
            found: group_type,
        })
    }
}

fn require_exact_supervisor_membership(members: &[u32], expected: u32, path: &Path) -> Result<()> {
    // Kernel documentation permits duplicate entries while a process moves.
    // Membership is therefore exact when the unique set is exactly {self}.
    let unique = members.iter().copied().collect::<BTreeSet<_>>();
    let expected_present = unique.contains(&expected);
    let first_foreign = unique.iter().copied().find(|pid| *pid != expected);
    if expected_present && first_foreign.is_none() && unique.len() == 1 {
        Ok(())
    } else {
        Err(CgroupError::SupervisorMembership {
            path: path.to_owned(),
            expected,
            expected_present,
            first_foreign,
            unique_members: unique.len(),
        })
    }
}

fn require_exact_leaf_membership(members: &[u32], expected: u32, path: &Path) -> Result<()> {
    let unique = members.iter().copied().collect::<BTreeSet<_>>();
    let expected_present = unique.contains(&expected);
    let first_foreign = unique.iter().copied().find(|pid| *pid != expected);
    if expected > 0 && expected_present && first_foreign.is_none() && unique.len() == 1 {
        Ok(())
    } else {
        Err(CgroupError::LeafMembership {
            path: path.to_owned(),
            expected,
            expected_present,
            first_foreign,
            unique_members: unique.len(),
        })
    }
}

fn require_descendant_topology(
    directory: &OwnedFd,
    label: &Path,
    expected_descendants: u64,
    allow_dying: bool,
) -> Result<()> {
    let (descendants, dying_descendants) = read_descendant_counts(directory, label)?;
    validate_descendant_topology(
        descendants,
        dying_descendants,
        expected_descendants,
        allow_dying,
        &label.join("cgroup.stat"),
    )
}

fn validate_descendant_topology(
    descendants: u64,
    dying_descendants: u64,
    expected_descendants: u64,
    allow_dying: bool,
    path: &Path,
) -> Result<()> {
    if descendants == expected_descendants && (allow_dying || dying_descendants == 0) {
        Ok(())
    } else {
        Err(CgroupError::DelegationTopology {
            path: path.to_owned(),
            expected_descendants,
            dying_requirement: if allow_dying { "any number of" } else { "zero" },
            descendants,
            dying_descendants,
        })
    }
}

#[cfg(test)]
mod tests;
