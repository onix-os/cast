//! Authenticated, exact-current-thread mountinfo snapshots.
//!
//! The generic mountinfo parser authenticates no reader. This layer binds one
//! bounded read to the prepared current-thread mount namespace and task root,
//! retains only owned bytes plus parsed values, and exposes no descriptor or
//! path authority. The result is an endpoint-sandwiched observation, not a
//! continuously current view; an unobserved ABA change cannot be disproven.

use std::{io, marker::PhantomData, rc::Rc, time::Instant};

use super::{
    PRODUCTION_MAX_DESCRIPTORS, PRODUCTION_MAX_WORK, PreparedMountNamespaceAnchor,
    filesystem::{MountNamespaceLimits, Operation},
};
use crate::linux_fs::mountinfo::{MOUNTINFO_LIMITS, MountInfo};

mod capture;
mod filesystem;

#[cfg(test)]
pub(crate) use filesystem::validate_fixture_mountinfo_file_authentication;

const MOUNTINFO_SENTINEL_BYTES: usize = MOUNTINFO_LIMITS.max_bytes + 1;
const MOUNTINFO_READ_PARSE_WORK_BOUND: usize = MOUNTINFO_SENTINEL_BYTES + MOUNTINFO_LIMITS.max_work;

const OPENING_AND_CLOSING_ANCHOR_ALLOWANCES: usize = 2;
const EXACT_THREAD_CONTEXT_ALLOWANCES: usize = 1;
const CONTEXT_ALLOWANCES: usize = OPENING_AND_CLOSING_ANCHOR_ALLOWANCES + EXACT_THREAD_CONTEXT_ALLOWANCES;

// The operation grants two full prepared-anchor revalidation allowances plus
// one full mount-context allowance for the exact-thread ns/root sandwich.
// Fixed headroom covers the two mountinfo file authentication/rebind steps.
// Both admissions charge real ns/root operations; only file work is reserved
// synthetically by the Cursor fixture.
const EXACT_THREAD_DESCRIPTOR_HEADROOM: usize = 96;
const EXACT_THREAD_WORK_HEADROOM: usize = 512 * 1024;

const SNAPSHOT_MAX_WORK: usize =
    PRODUCTION_MAX_WORK * CONTEXT_ALLOWANCES + MOUNTINFO_READ_PARSE_WORK_BOUND + EXACT_THREAD_WORK_HEADROOM;
const SNAPSHOT_MAX_DESCRIPTORS: usize =
    PRODUCTION_MAX_DESCRIPTORS * CONTEXT_ALLOWANCES + EXACT_THREAD_DESCRIPTOR_HEADROOM;
const SNAPSHOT_LIMITS: MountNamespaceLimits = MountNamespaceLimits {
    max_work: SNAPSHOT_MAX_WORK,
    max_descriptors: SNAPSHOT_MAX_DESCRIPTORS,
};

const _: () = assert!(MOUNTINFO_SENTINEL_BYTES > MOUNTINFO_LIMITS.max_bytes);
const _: () = assert!(CONTEXT_ALLOWANCES == 3);
const _: () = assert!(SNAPSHOT_MAX_WORK > MOUNTINFO_READ_PARSE_WORK_BOUND);
const _: () = assert!(SNAPSHOT_MAX_DESCRIPTORS > EXACT_THREAD_DESCRIPTOR_HEADROOM);

#[cfg(test)]
pub(crate) const FIXTURE_MOUNTINFO_PROCFS_MAGIC: nix::libc::c_long = crate::linux_fs::PROC_SUPER_MAGIC;

/// One authenticated current-thread mountinfo observation.
///
/// This value deliberately owns no `File`, fd, pathname, or reopen closure.
/// It remains `!Send` and `!Sync` because its bytes are meaningful only in the
/// acquiring thread's task-root and mount-namespace context. A stable change
/// after that domain's last observation can escape detection even if it occurs
/// before this function returns; consumers must not treat the value as a live
/// currentness lease.
pub(crate) struct AuthenticatedMountInfoSnapshot {
    bytes: Vec<u8>,
    parsed: MountInfo,
    _thread_bound: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for AuthenticatedMountInfoSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedMountInfoSnapshot")
            .field("byte_len", &self.bytes.len())
            .field("entry_count", &self.parsed.entries().len())
            .finish_non_exhaustive()
    }
}

impl AuthenticatedMountInfoSnapshot {
    /// Exact bytes from the authenticated production descriptor, or from the
    /// explicitly Cursor-only cfg(test) fixture seam.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Parsed values derived from exactly [`Self::bytes`].
    pub(crate) fn mountinfo(&self) -> &MountInfo {
        &self.parsed
    }

    fn new(bytes: Vec<u8>, parsed: MountInfo) -> Self {
        Self {
            bytes,
            parsed,
            _thread_bound: PhantomData,
        }
    }
}

impl PreparedMountNamespaceAnchor {
    /// Read this exact current thread's fixed procfs `mountinfo` entry.
    ///
    /// The caller's absolute deadline is shared by the opening anchor check,
    /// retained-thread ns/root sandwich, bounded read and parse, fixed-name
    /// rebind, closing anchor check, and terminal checkpoint.
    pub(crate) fn read_current_thread_mountinfo_until(
        &self,
        deadline: Instant,
    ) -> io::Result<AuthenticatedMountInfoSnapshot> {
        #[cfg(test)]
        if self.locator.is_fixture() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "production mountinfo reader rejects a fixture mount-context anchor",
            ));
        }

        let mut operation = Operation::production(SNAPSHOT_LIMITS, deadline);
        let opening = self.revalidate_with_operation(&mut operation)?;
        operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::BeforeExactThread)?;
        reserve_mountinfo_read_parse(&mut operation)?;
        let (bytes, parsed) = capture::read_current_thread_mountinfo(self, &mut operation)?;
        operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::BeforeClosingAnchor)?;
        let closing = self.revalidate_with_operation(&mut operation)?;
        require_same_revalidated_context(&opening, &closing)?;
        operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::Complete)?;
        operation.checkpoint()?;
        Ok(AuthenticatedMountInfoSnapshot::new(bytes, parsed))
    }

    /// Cursor-only test seam for an already admitted ordinary fixture anchor.
    ///
    /// It never opens procfs, sysfs, nsfs, a device, or a special file. The
    /// synthetic namespace/root are still checked before and after the bounded
    /// parser, while production-equivalent reader work and descriptors are
    /// reserved from the same operation budget.
    #[cfg(test)]
    pub(crate) fn read_fixture_mountinfo_bytes_with(
        &self,
        bytes: &[u8],
        limits: FixtureMountInfoSnapshotLimits,
        deadline: Instant,
        hook: &mut impl FnMut(super::FixtureMountNamespaceCheckpoint) -> io::Result<()>,
    ) -> io::Result<AuthenticatedMountInfoSnapshot> {
        if !self.locator.is_fixture() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mountinfo byte fixture requires a fixture mount-context anchor",
            ));
        }
        let mut operation = Operation::fixture(limits.into(), deadline, hook)?;
        self.read_fixture_mountinfo_bytes_with_operation(bytes, &mut operation)
    }

    #[cfg(test)]
    pub(crate) fn read_fixture_mountinfo_bytes_with_clock(
        &self,
        bytes: &[u8],
        limits: FixtureMountInfoSnapshotLimits,
        deadline: Instant,
        hook: &mut impl FnMut(super::FixtureMountNamespaceCheckpoint) -> io::Result<()>,
        clock: &mut impl FnMut() -> Instant,
    ) -> io::Result<AuthenticatedMountInfoSnapshot> {
        if !self.locator.is_fixture() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mountinfo byte fixture requires a fixture mount-context anchor",
            ));
        }
        let mut operation = Operation::fixture_with_clock(limits.into(), deadline, hook, clock)?;
        self.read_fixture_mountinfo_bytes_with_operation(bytes, &mut operation)
    }

    #[cfg(test)]
    pub(crate) fn measure_fixture_mountinfo_bytes_with(
        &self,
        bytes: &[u8],
        limits: FixtureMountInfoSnapshotLimits,
        deadline: Instant,
    ) -> io::Result<(AuthenticatedMountInfoSnapshot, FixtureMountInfoSnapshotUsage)> {
        if !self.locator.is_fixture() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mountinfo byte fixture requires a fixture mount-context anchor",
            ));
        }
        let mut hook = |_| Ok(());
        let mut operation = Operation::fixture(limits.into(), deadline, &mut hook)?;
        let snapshot = self.read_fixture_mountinfo_bytes_with_operation(bytes, &mut operation)?;
        let usage = FixtureMountInfoSnapshotUsage {
            work: operation.consumed_work(),
            descriptors: operation.consumed_descriptors(),
        };
        Ok((snapshot, usage))
    }

    #[cfg(test)]
    fn read_fixture_mountinfo_bytes_with_operation(
        &self,
        bytes: &[u8],
        operation: &mut Operation<'_>,
    ) -> io::Result<AuthenticatedMountInfoSnapshot> {
        let opening = self.revalidate_with_operation(operation)?;
        operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::BeforeExactThread)?;
        reserve_mountinfo_read_parse(operation)?;
        let (bytes, parsed) = capture::read_fixture_mountinfo(self, bytes, operation)?;
        operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::BeforeClosingAnchor)?;
        let closing = self.revalidate_with_operation(operation)?;
        require_same_revalidated_context(&opening, &closing)?;
        operation.emit_mountinfo_snapshot(MountInfoSnapshotCheckpoint::Complete)?;
        operation.checkpoint()?;
        Ok(AuthenticatedMountInfoSnapshot::new(bytes, parsed))
    }
}

fn reserve_mountinfo_read_parse(operation: &mut Operation<'_>) -> io::Result<()> {
    operation.charge(
        MOUNTINFO_READ_PARSE_WORK_BOUND,
        "reserving bounded mountinfo read and parser work",
    )
}

fn require_same_revalidated_context(
    opening: &super::RevalidatedMountNamespaceAnchor<'_>,
    closing: &super::RevalidatedMountNamespaceAnchor<'_>,
) -> io::Result<()> {
    super::capture::require_snapshot_matches(
        opening.current.snapshot(),
        closing.current.snapshot(),
        "mountinfo opening and closing anchor snapshots",
    )
}

#[derive(Debug, Clone, Copy)]
pub(super) enum MountInfoSnapshotCheckpoint {
    BeforeExactThread,
    ThreadOpened,
    NamespacePinned,
    TaskRootPinned,
    FileOpened,
    BeforeRead,
    AfterRead,
    FileRebound,
    TaskRootRechecked,
    NamespaceRechecked,
    BeforeClosingAnchor,
    Complete,
}

#[cfg(test)]
impl From<MountInfoSnapshotCheckpoint> for super::FixtureMountNamespaceCheckpoint {
    fn from(checkpoint: MountInfoSnapshotCheckpoint) -> Self {
        match checkpoint {
            MountInfoSnapshotCheckpoint::BeforeExactThread => Self::MountInfoSnapshotBeforeExactThread,
            MountInfoSnapshotCheckpoint::ThreadOpened => Self::MountInfoSnapshotThreadOpened,
            MountInfoSnapshotCheckpoint::NamespacePinned => Self::MountInfoSnapshotNamespacePinned,
            MountInfoSnapshotCheckpoint::TaskRootPinned => Self::MountInfoSnapshotTaskRootPinned,
            MountInfoSnapshotCheckpoint::FileOpened => Self::MountInfoSnapshotFileOpened,
            MountInfoSnapshotCheckpoint::BeforeRead => Self::MountInfoSnapshotBeforeRead,
            MountInfoSnapshotCheckpoint::AfterRead => Self::MountInfoSnapshotAfterRead,
            MountInfoSnapshotCheckpoint::FileRebound => Self::MountInfoSnapshotFileRebound,
            MountInfoSnapshotCheckpoint::TaskRootRechecked => Self::MountInfoSnapshotTaskRootRechecked,
            MountInfoSnapshotCheckpoint::NamespaceRechecked => Self::MountInfoSnapshotNamespaceRechecked,
            MountInfoSnapshotCheckpoint::BeforeClosingAnchor => Self::MountInfoSnapshotBeforeClosingAnchor,
            MountInfoSnapshotCheckpoint::Complete => Self::MountInfoSnapshotComplete,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FixtureMountInfoSnapshotLimits {
    pub(crate) max_work: usize,
    pub(crate) max_descriptors: usize,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FixtureMountInfoSnapshotUsage {
    pub(crate) work: usize,
    pub(crate) descriptors: usize,
}

#[cfg(test)]
impl Default for FixtureMountInfoSnapshotLimits {
    fn default() -> Self {
        Self {
            max_work: SNAPSHOT_MAX_WORK,
            max_descriptors: SNAPSHOT_MAX_DESCRIPTORS,
        }
    }
}

#[cfg(test)]
impl From<FixtureMountInfoSnapshotLimits> for MountNamespaceLimits {
    fn from(limits: FixtureMountInfoSnapshotLimits) -> Self {
        Self {
            max_work: limits.max_work,
            max_descriptors: limits.max_descriptors,
        }
    }
}
