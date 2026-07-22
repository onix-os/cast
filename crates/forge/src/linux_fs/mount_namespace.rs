//! Descriptor-retained authentication of the current task's mount context.
//!
//! This anchor retains two independent capabilities: the current thread's
//! mount-namespace descriptor and the absolute-path root exposed by that same
//! thread's authenticated procfs directory. The latter is the task root used
//! to interpret mountinfo mount-point bytes; it is **not** claimed to be a
//! global root of the mount namespace.
//!
//! Preparation and revalidation sandwich both names through complete passes.
//! An observed endpoint mismatch from `setns(2)`, `chroot(2)`, or another
//! root-changing operation is rejected; an ABA change which begins and ends
//! on the same identities between observations cannot be proven absent. The
//! value exposes no descriptor, performs no namespace switch or filesystem
//! mutation, and grants no path-resolution or publication authority.

use std::{
    io,
    marker::PhantomData,
    rc::Rc,
    time::{Duration, Instant},
};

mod attachment;
mod capture;
mod filesystem;
mod mountinfo_snapshot;

#[allow(unused_imports)] // named by the future owned mounted-topology aggregate
pub(crate) use attachment::{
    PreparedTaskRootedAttachment, RetainedBootFilePublicationError, RetainedBootFilePublicationLimits,
    RetainedBootFilePublicationOutcome, RetainedBootFilePublicationRequest, RevalidatedTaskRootedAttachment,
    AuthenticatedRetainedBootFileStaleCleanup, RetainedBootFileMutationFingerprint,
    RetainedBootFileAppliedSidecarCleanupState,
    RetainedBootFileRestoredSidecarCleanupState,
    RetainedBootFileReplacementError, RetainedBootFileStaleCleanupOutcome,
    RetainedBootFileStaleCleanupRequest, RetainedBootFileStaleCleanupState,
    RetainedBootFileReplacementRequest, RetainedBootFileSidecarCleanupOutcome,
    RetainedBootLeafAssessmentError, RetainedBootLeafAssessmentLimits,
    RetainedBootLeafAssessmentRequest, RetainedBootLeafAssessmentState,
    RetainedBootPublicationParent, RetainedBootPublicationParentError, TaskRootBootNamespaceAssessmentError,
    ValidatedRetainedBootFilePublication, ValidatedRetainedBootFileReplacement,
    ValidatedRetainedBootFileRestoration, ValidatedRetainedBootLeafAssessment,
    ValidatedTaskRootBootNamespaceAssessment,
};
#[allow(unused_imports)] // consumed by the authenticated mounted-topology aggregate
pub(crate) use mountinfo_snapshot::AuthenticatedMountInfoSnapshot;

#[cfg(test)]
#[allow(unused_imports)] // consumed by the synthetic attachment test slice
pub(crate) use attachment::FixtureTaskRootedAttachmentLimits;

#[cfg(test)]
pub(crate) use attachment::{
    arm_boot_file_exchange_error_after_applied, arm_boot_file_replacement_stop_before_exchange,
    arm_boot_file_sidecar_stop_after_unlink, arm_stale_boot_file_detach_error_after_applied,
    arm_stale_boot_file_stop_after_detach,
    FixtureRetainedBootLeafAssessmentHookGuard,
    FixtureRetainedBootFilePublicationFault, FixtureRetainedBootPublicationParentCheckpoint,
    FixtureRetainedBootPublicationParentFault,
    arm_retained_boot_leaf_assessment_terminal_rebind_hook,
    arm_retained_boot_file_private_name_substitution, arm_retained_boot_file_publication_fault,
    arm_retained_boot_publication_parent_checkpoint_hook, arm_retained_boot_publication_parent_fault,
    validate_fixture_boot_publication_parent_identity, validate_fixture_boot_publication_parent_policy,
};

#[cfg(test)]
pub(crate) use attachment::validate_fixture_attachment_st_dev;

#[cfg(test)]
#[allow(unused_imports)] // consumed by the dedicated mountinfo snapshot test slice
pub(crate) use mountinfo_snapshot::{
    FIXTURE_MOUNTINFO_PROCFS_MAGIC, FixtureMountInfoSnapshotLimits, FixtureMountInfoSnapshotUsage,
    validate_fixture_mountinfo_file_authentication,
};

use capture::{Capture, capture_twice, require_snapshot_matches};
use filesystem::{CaptureCheckpoint, Locator, MountNamespaceLimits, Operation};

const PRODUCTION_TIMEOUT: Duration = Duration::from_secs(30);
const PRODUCTION_MAX_WORK: usize = 16 * 1024 * 1024;
const PRODUCTION_MAX_DESCRIPTORS: usize = 1_024;

const PRODUCTION_LIMITS: MountNamespaceLimits = MountNamespaceLimits {
    max_work: PRODUCTION_MAX_WORK,
    max_descriptors: PRODUCTION_MAX_DESCRIPTORS,
};

// Preparation/revalidation each use two full captures, terminal rebinds, and
// retained-descriptor sandwiches. This overbounds five current-thread procfs
// acquisitions, twelve task-root mount-ID captures, and all direct component
// opens plus classifier/syscall work within one operation.
const REQUIRED_DESCRIPTOR_UNITS: usize = 5 * 5 + 12 * 24 + 64;
const REQUIRED_WORK_UNITS: usize = 12 * 64 * 1024 + 1024 * 1024;
const _: () = assert!(PRODUCTION_MAX_DESCRIPTORS >= REQUIRED_DESCRIPTOR_UNITS);
const _: () = assert!(PRODUCTION_MAX_WORK >= REQUIRED_WORK_UNITS);

/// Retained current-task mount context which requires fresh revalidation.
///
/// `PhantomData<Rc<()>>` makes this value `!Send` and `!Sync`: procfs
/// `thread-self`, mount IDs, and `setns` are thread-relative. A task root can
/// still be shared through `fs_struct` and changed by another thread, which is
/// why every use requires a fresh multi-pass observation.
pub(crate) struct PreparedMountNamespaceAnchor {
    locator: Locator,
    capture: Capture,
    _thread_bound: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for PreparedMountNamespaceAnchor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedMountNamespaceAnchor")
            .field("evidence", &"retained; revalidation required")
            .finish_non_exhaustive()
    }
}

impl PreparedMountNamespaceAnchor {
    /// Prepare from fixed, authenticated current-thread procfs entries.
    ///
    /// Production resolution is exactly `ns/mnt` and `root` below the
    /// authenticated current-thread directory. There is no root argument,
    /// environment override, path discovery, or fallback.
    pub(crate) fn prepare() -> io::Result<Self> {
        let deadline = deadline_after(PRODUCTION_TIMEOUT)?;
        Self::prepare_until(deadline)
    }

    /// Prepare without replacing the caller-owned absolute deadline.
    pub(crate) fn prepare_until(deadline: Instant) -> io::Result<Self> {
        let mut operation = Operation::production(PRODUCTION_LIMITS, deadline);
        Self::prepare_from_locator(Locator::production(), &mut operation)
    }

    fn prepare_from_locator(locator: Locator, operation: &mut Operation<'_>) -> io::Result<Self> {
        let capture = capture_twice(&locator, operation)?;
        locator.require_terminal_names(capture.snapshot(), operation)?;
        capture.require_retained(operation)?;
        operation.emit(CaptureCheckpoint::OperationComplete)?;
        operation.checkpoint()?;
        Ok(Self {
            locator,
            capture,
            _thread_bound: PhantomData,
        })
    }

    /// Revalidate both retained capabilities and both fixed procfs names.
    ///
    /// Semantic evidence can be extracted only after this revalidation through
    /// the returned borrowed view. Scalar facts copied from that view can
    /// still become stale; the borrow is not an ongoing-currentness guarantee.
    pub(crate) fn revalidate(&self) -> io::Result<RevalidatedMountNamespaceAnchor<'_>> {
        let deadline = deadline_after(PRODUCTION_TIMEOUT)?;
        self.revalidate_until(deadline)
    }

    /// Revalidate without replacing the caller-owned absolute deadline.
    pub(crate) fn revalidate_until(&self, deadline: Instant) -> io::Result<RevalidatedMountNamespaceAnchor<'_>> {
        #[cfg(test)]
        let mut operation = if self.locator.is_fixture() {
            Operation::fixture_without_hook(PRODUCTION_LIMITS, deadline)?
        } else {
            Operation::production(PRODUCTION_LIMITS, deadline)
        };
        #[cfg(not(test))]
        let mut operation = Operation::production(PRODUCTION_LIMITS, deadline);
        self.revalidate_with_operation(&mut operation)
    }

    fn revalidate_with_operation<'a>(
        &'a self,
        operation: &mut Operation<'_>,
    ) -> io::Result<RevalidatedMountNamespaceAnchor<'a>> {
        self.capture.require_retained(operation)?;
        let current = capture_twice(&self.locator, operation)?;
        require_snapshot_matches(self.capture.snapshot(), current.snapshot(), "prepared mount context")?;
        self.locator.require_terminal_names(current.snapshot(), operation)?;
        self.capture.require_retained(operation)?;
        current.require_retained(operation)?;
        operation.emit(CaptureCheckpoint::OperationComplete)?;
        operation.checkpoint()?;
        Ok(RevalidatedMountNamespaceAnchor {
            _prepared: self,
            current,
        })
    }

    #[cfg(test)]
    pub(crate) fn revalidate_with(
        &self,
        limits: FixtureMountNamespaceLimits,
        deadline: Instant,
        hook: &mut impl FnMut(FixtureMountNamespaceCheckpoint) -> io::Result<()>,
    ) -> io::Result<RevalidatedMountNamespaceAnchor<'_>> {
        let mut operation = Operation::fixture(limits.into(), deadline, hook)?;
        self.revalidate_with_operation(&mut operation)
    }

    #[cfg(test)]
    pub(crate) fn revalidate_with_clock(
        &self,
        limits: FixtureMountNamespaceLimits,
        deadline: Instant,
        hook: &mut impl FnMut(FixtureMountNamespaceCheckpoint) -> io::Result<()>,
        clock: &mut impl FnMut() -> Instant,
    ) -> io::Result<RevalidatedMountNamespaceAnchor<'_>> {
        let mut operation = Operation::fixture_with_clock(limits.into(), deadline, hook, clock)?;
        self.revalidate_with_operation(&mut operation)
    }
}

/// Fresh semantic evidence for one retained current-task mount context.
///
/// Mount-namespace identity and task-root mount identity are intentionally
/// separate domains. In particular, an nsfs descriptor's fdinfo mount ID is
/// neither captured nor compared with the task root's mount ID.
pub(crate) struct RevalidatedMountNamespaceAnchor<'a> {
    _prepared: &'a PreparedMountNamespaceAnchor,
    current: Capture,
}

impl std::fmt::Debug for RevalidatedMountNamespaceAnchor<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevalidatedMountNamespaceAnchor")
            .field("mount_namespace_device", &self.mount_namespace_device())
            .field("mount_namespace_inode", &self.mount_namespace_inode())
            .field("task_root_device", &self.task_root_device())
            .field("task_root_inode", &self.task_root_inode())
            .field("task_root_mount_id", &self.task_root_mount_id())
            .finish_non_exhaustive()
    }
}

impl RevalidatedMountNamespaceAnchor<'_> {
    pub(crate) const fn mount_namespace_device(&self) -> u64 {
        self.current.snapshot().namespace.device
    }

    pub(crate) const fn mount_namespace_inode(&self) -> u64 {
        self.current.snapshot().namespace.inode
    }

    /// Device identity for the current task's absolute-path root.
    pub(crate) const fn task_root_device(&self) -> u64 {
        self.current.snapshot().task_root.device
    }

    /// Inode identity for the current task's absolute-path root.
    pub(crate) const fn task_root_inode(&self) -> u64 {
        self.current.snapshot().task_root.inode
    }

    /// Mount ID for the current task's absolute-path root.
    ///
    /// This is not an nsfs mount ID and conveys no relationship to the
    /// namespace descriptor beyond their joint sandwich in this anchor.
    pub(crate) const fn task_root_mount_id(&self) -> u64 {
        self.current.snapshot().task_root.mount_id
    }
}

fn deadline_after(timeout: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "mount-context deadline overflowed"))
}

/// Shared nsfs plus namespace-type authentication used by all production
/// mount-namespace capture paths in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AuthenticatedMountNamespaceIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) kind: u32,
    pub(crate) namespace_type: nix::libc::c_int,
}

pub(crate) fn authenticate_mount_namespace_descriptor(
    namespace: &std::fs::File,
    deadline: Option<Instant>,
) -> io::Result<AuthenticatedMountNamespaceIdentity> {
    filesystem::authenticate_mount_namespace_descriptor(namespace, deadline)
}

#[cfg(test)]
pub(crate) const FIXTURE_NSFS_MAGIC: nix::libc::c_long = filesystem::NSFS_MAGIC;

#[cfg(test)]
pub(crate) const FIXTURE_MOUNT_NAMESPACE_TYPE: nix::libc::c_int = nix::libc::CLONE_NEWNS;

/// Pure test seam for the exact classifier used after production fstatfs and
/// NS_GET_NSTYPE calls. It opens no procfs or namespace descriptor.
#[cfg(test)]
pub(crate) fn validate_fixture_namespace_authentication(
    filesystem_magic: nix::libc::c_long,
    namespace_type: nix::libc::c_int,
) -> io::Result<()> {
    filesystem::validate_namespace_authentication(filesystem_magic, namespace_type)
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FixtureMountNamespaceLimits {
    pub(crate) max_work: usize,
    pub(crate) max_descriptors: usize,
}

#[cfg(test)]
impl Default for FixtureMountNamespaceLimits {
    fn default() -> Self {
        Self {
            max_work: PRODUCTION_MAX_WORK,
            max_descriptors: PRODUCTION_MAX_DESCRIPTORS,
        }
    }
}

#[cfg(test)]
impl From<FixtureMountNamespaceLimits> for MountNamespaceLimits {
    fn from(limits: FixtureMountNamespaceLimits) -> Self {
        Self {
            max_work: limits.max_work,
            max_descriptors: limits.max_descriptors,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixtureMountNamespaceCheckpoint {
    TreeRebind,
    NamespaceDirectoryPinned { pass: usize },
    NamespacePinned { pass: usize },
    TaskRootPinned { pass: usize },
    PassTaskRootRecheck { pass: usize },
    PassNamespaceRecheck { pass: usize },
    PassComplete { pass: usize },
    TerminalTreeRebind,
    TerminalNamespaceRebind,
    TerminalTaskRootRebind,
    TerminalTaskRootRecheck,
    TerminalNamespaceRecheck,
    AttachmentAnchorOpened,
    AttachmentComponentPinned { pass: usize, index: usize },
    AttachmentPassComplete { pass: usize },
    AttachmentTerminalFullChain { round: usize },
    AttachmentTerminalParent,
    AttachmentTerminalName,
    AttachmentBeforeClosingAnchor,
    AttachmentComplete,
    MountInfoSnapshotBeforeExactThread,
    MountInfoSnapshotThreadOpened,
    MountInfoSnapshotNamespacePinned,
    MountInfoSnapshotTaskRootPinned,
    MountInfoSnapshotFileOpened,
    MountInfoSnapshotBeforeRead,
    MountInfoSnapshotAfterRead,
    MountInfoSnapshotFileRebound,
    MountInfoSnapshotTaskRootRechecked,
    MountInfoSnapshotNamespaceRechecked,
    MountInfoSnapshotBeforeClosingAnchor,
    MountInfoSnapshotComplete,
    MountContextComplete,
}

/// Test-only admission of an ordinary, named synthetic task tree.
///
/// The tree layout is `ns/mnt` (an ordinary regular marker) plus `root` (an
/// ordinary directory). Admission rejects procfs, sysfs, and nsfs at every
/// retained level and never enters the production capture path.
#[cfg(test)]
pub(crate) struct FixtureMountNamespaceTree {
    locator: Locator,
    _thread_bound: PhantomData<Rc<()>>,
}

#[cfg(test)]
impl FixtureMountNamespaceTree {
    pub(crate) fn admit(parent: std::fs::File, tree_name: std::ffi::CString) -> io::Result<Self> {
        let deadline = deadline_after(PRODUCTION_TIMEOUT)?;
        let mut operation = Operation::fixture_without_hook(PRODUCTION_LIMITS, deadline)?;
        let locator = Locator::admit_fixture(parent, tree_name, &mut operation)?;
        capture::validate_fixture_tree(&locator, &mut operation)?;
        Ok(Self {
            locator,
            _thread_bound: PhantomData,
        })
    }

    pub(crate) fn prepare(&self) -> io::Result<PreparedMountNamespaceAnchor> {
        let deadline = deadline_after(PRODUCTION_TIMEOUT)?;
        let mut operation = Operation::fixture_without_hook(PRODUCTION_LIMITS, deadline)?;
        let locator = self.locator.reopen_owned(&mut operation)?;
        PreparedMountNamespaceAnchor::prepare_from_locator(locator, &mut operation)
    }

    pub(crate) fn prepare_with(
        &self,
        limits: FixtureMountNamespaceLimits,
        deadline: Instant,
        hook: &mut impl FnMut(FixtureMountNamespaceCheckpoint) -> io::Result<()>,
    ) -> io::Result<PreparedMountNamespaceAnchor> {
        let mut operation = Operation::fixture(limits.into(), deadline, hook)?;
        let locator = self.locator.reopen_owned(&mut operation)?;
        PreparedMountNamespaceAnchor::prepare_from_locator(locator, &mut operation)
    }

    pub(crate) fn prepare_with_clock(
        &self,
        limits: FixtureMountNamespaceLimits,
        deadline: Instant,
        hook: &mut impl FnMut(FixtureMountNamespaceCheckpoint) -> io::Result<()>,
        clock: &mut impl FnMut() -> Instant,
    ) -> io::Result<PreparedMountNamespaceAnchor> {
        let mut operation = Operation::fixture_with_clock(limits.into(), deadline, hook, clock)?;
        let locator = self.locator.reopen_owned(&mut operation)?;
        PreparedMountNamespaceAnchor::prepare_from_locator(locator, &mut operation)
    }
}
