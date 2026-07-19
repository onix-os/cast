//! Retained resolution of one authored selector below an authenticated task root.
//!
//! Every component is opened descriptor-relatively without following symlinks
//! or magic links. Mount crossings remain allowed because the destination can
//! itself be an already-mounted ESP or XBOOTLDR. Two complete rebind passes and
//! terminal parent/name/full-chain checks are sandwiched by fresh checks of a
//! snapshot-equivalent authenticated mount context.
//!
//! Success is evidence only for this bounded observation. It does not prove
//! call-to-return state, ongoing currentness, or simultaneous residency: a
//! stable change after either domain's last observation, as well as an ABA
//! replacement entirely between observations, can escape until the next
//! aggregate revalidation brackets dependent use. This is only attachment
//! evidence. It does not prove that the destination is
//! a mount point, match mountinfo, identify a PARTUUID, validate GPT or a
//! filesystem, authorize publication, or establish durability. A future
//! aggregate must sandwich this result together with those independent
//! authorities.

use std::{
    io,
    marker::PhantomData,
    rc::Rc,
    time::{Duration, Instant},
};

mod boot_namespace;
mod capture;
mod device;
mod filesystem;
mod gpt_device;
mod selector;

use capture::{AttachmentCapture, capture_twice, require_capture_matches};
pub(crate) use boot_namespace::{
    TaskRootBootNamespaceAssessmentError, ValidatedTaskRootBootNamespaceAssessment,
};
pub(crate) use device::{TaskRootDevtmpfsAttachmentAuthenticationError, ValidatedTaskRootDevtmpfsAttachmentEvidence};
use filesystem::{AttachmentLimits, directory_witness, duplicate_directory, require_same_directory};
#[allow(unused_imports)] // named by the future owned mounted-topology aggregate
pub(in crate::linux_fs) use gpt_device::{
    TaskRootDevtmpfsGptPartitionDeviceAuthenticationError, ValidatedTaskRootDevtmpfsGptPartitionDeviceEvidence,
};
use selector::{AttachmentSelector, MAX_SELECTOR_COMPONENTS};

use super::{
    PreparedMountNamespaceAnchor, RevalidatedMountNamespaceAnchor,
    capture::{Snapshot, require_snapshot_matches},
    filesystem::{DESCRIPTOR_MOUNT_ID_DESCRIPTOR_BOUND, DESCRIPTOR_MOUNT_ID_WORK_BOUND, Operation},
};
#[cfg(test)]
use crate::linux_fs::descriptor_devtmpfs_filesystem::{
    DevtmpfsDescriptorAuthenticationError, ValidatedDevtmpfsSameMountDescriptorEvidence,
};
use crate::linux_fs::{
    descriptor_boot_filesystem::{BootFilesystemAuthenticationError, ValidatedBootFilesystemDescriptorEvidence},
    gpt_partition_role::GptPartitionRole,
    mountinfo_devtmpfs_policy::ValidatedDevtmpfsMountInfoPolicy,
    sysfs_block::SysfsDeviceNumber,
};

const PRODUCTION_TIMEOUT: Duration = Duration::from_secs(30);
const PRODUCTION_MAX_WORK: usize = 512 * 1024 * 1024;
const PRODUCTION_MAX_DESCRIPTORS: usize = 131_072;
const PRODUCTION_LIMITS: AttachmentLimits = AttachmentLimits {
    max_work: PRODUCTION_MAX_WORK,
    max_descriptors: PRODUCTION_MAX_DESCRIPTORS,
};

// Thirty-two mount-ID witnesses per possible directory conservatively
// overbound both fresh anchor sandwiches, two full captures, retained-chain
// checks, terminal parent/name checks, and both terminal full-chain rebinds.
// Each witness reserves the encapsulated authenticated fdinfo descriptor and
// parser ceilings even though fixture witnesses remain entirely proc-free.
const MAX_CHAIN_DIRECTORIES: usize = MAX_SELECTOR_COMPONENTS + 1;
const WORST_CASE_WITNESSES: usize = 32 * MAX_CHAIN_DIRECTORIES;
const REQUIRED_DESCRIPTOR_UNITS: usize =
    WORST_CASE_WITNESSES * DESCRIPTOR_MOUNT_ID_DESCRIPTOR_BOUND + 16 * MAX_CHAIN_DIRECTORIES + 2_048;
const REQUIRED_WORK_UNITS: usize = WORST_CASE_WITNESSES * DESCRIPTOR_MOUNT_ID_WORK_BOUND + 64 * 1024 * 1024;
const _: () = assert!(PRODUCTION_MAX_DESCRIPTORS >= REQUIRED_DESCRIPTOR_UNITS);
const _: () = assert!(PRODUCTION_MAX_WORK >= REQUIRED_WORK_UNITS);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttachmentCheckpoint {
    AnchorOpened,
    ComponentPinned { pass: usize, index: usize },
    PassComplete { pass: usize },
    TerminalFullChain { round: usize },
    TerminalParent,
    TerminalName,
    BeforeClosingAnchor,
    Complete,
}

#[cfg(test)]
impl From<AttachmentCheckpoint> for super::FixtureMountNamespaceCheckpoint {
    fn from(checkpoint: AttachmentCheckpoint) -> Self {
        match checkpoint {
            AttachmentCheckpoint::AnchorOpened => Self::AttachmentAnchorOpened,
            AttachmentCheckpoint::ComponentPinned { pass, index } => Self::AttachmentComponentPinned { pass, index },
            AttachmentCheckpoint::PassComplete { pass } => Self::AttachmentPassComplete { pass },
            AttachmentCheckpoint::TerminalFullChain { round } => Self::AttachmentTerminalFullChain { round },
            AttachmentCheckpoint::TerminalParent => Self::AttachmentTerminalParent,
            AttachmentCheckpoint::TerminalName => Self::AttachmentTerminalName,
            AttachmentCheckpoint::BeforeClosingAnchor => Self::AttachmentBeforeClosingAnchor,
            AttachmentCheckpoint::Complete => Self::AttachmentComplete,
        }
    }
}

/// Owned, thread-bound attachment evidence requiring an explicit anchor on use.
///
/// The anchor is intentionally not borrowed or stored: an aggregate can own
/// both values without becoming self-referential. Revalidation accepts an
/// anchor and rejects it unless its authenticated namespace and task-root snapshot
/// matches the one recorded here.
pub(crate) struct PreparedTaskRootedAttachment {
    root: std::fs::File,
    selector: AttachmentSelector,
    capture: AttachmentCapture,
    anchor_snapshot: Snapshot,
    _thread_bound: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for PreparedTaskRootedAttachment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedTaskRootedAttachment")
            .field("selector", &self.selector.authored())
            .field("evidence", &"retained; anchor revalidation required")
            .finish_non_exhaustive()
    }
}

impl RevalidatedMountNamespaceAnchor<'_> {
    pub(crate) fn prepare_task_rooted_attachment(&self, selector: &str) -> io::Result<PreparedTaskRootedAttachment> {
        let deadline = deadline_after(PRODUCTION_TIMEOUT)?;
        self.prepare_task_rooted_attachment_until(selector, deadline)
    }

    /// Prepare without replacing the caller-owned absolute deadline.
    pub(crate) fn prepare_task_rooted_attachment_until(
        &self,
        selector: &str,
        deadline: Instant,
    ) -> io::Result<PreparedTaskRootedAttachment> {
        #[cfg(test)]
        let mut operation = if self._prepared.locator.is_fixture() {
            Operation::fixture_without_hook(PRODUCTION_LIMITS, deadline)?
        } else {
            Operation::production(PRODUCTION_LIMITS, deadline)
        };
        #[cfg(not(test))]
        let mut operation = Operation::production(PRODUCTION_LIMITS, deadline);
        self.prepare_task_rooted_attachment_with_operation(selector, &mut operation)
    }

    fn prepare_task_rooted_attachment_with_operation(
        &self,
        selector: &str,
        operation: &mut Operation<'_>,
    ) -> io::Result<PreparedTaskRootedAttachment> {
        let selector = AttachmentSelector::parse(selector, operation)?;
        let anchor_snapshot = self.current.snapshot();

        self.current.require_retained(operation)?;
        let opening = self._prepared.revalidate_with_operation(operation)?;
        require_snapshot_matches(anchor_snapshot, opening.current.snapshot(), "attachment opening anchor")?;
        opening.current.require_retained(operation)?;
        operation.emit_attachment(AttachmentCheckpoint::AnchorOpened)?;

        let root = duplicate_directory(
            self.current.task_root_file(),
            operation,
            "duplicating exact revalidated task root for attachment",
        )?;
        let root_witness = directory_witness(&root, operation, "duplicated attachment task root")?;
        require_same_directory(
            anchor_snapshot.task_root,
            root_witness,
            "duplicated attachment task root against anchor snapshot",
        )?;
        self.current.require_retained(operation)?;

        let capture = capture_twice(&root, root_witness, &selector, operation)?;
        capture.require_terminal_names(&root, operation)?;

        operation.emit_attachment(AttachmentCheckpoint::BeforeClosingAnchor)?;
        let closing = self._prepared.revalidate_with_operation(operation)?;
        require_snapshot_matches(anchor_snapshot, closing.current.snapshot(), "attachment closing anchor")?;
        closing.current.require_retained(operation)?;
        self.current.require_retained(operation)?;
        capture.require_retained(&root, operation)?;
        operation.emit_attachment(AttachmentCheckpoint::Complete)?;
        operation.checkpoint()?;

        Ok(PreparedTaskRootedAttachment {
            root,
            selector,
            capture,
            anchor_snapshot,
            _thread_bound: PhantomData,
        })
    }

    #[cfg(test)]
    pub(crate) fn prepare_task_rooted_attachment_with(
        &self,
        selector: &str,
        limits: FixtureTaskRootedAttachmentLimits,
        deadline: Instant,
        hook: &mut impl FnMut(super::FixtureMountNamespaceCheckpoint) -> io::Result<()>,
    ) -> io::Result<PreparedTaskRootedAttachment> {
        if !self._prepared.locator.is_fixture() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "attachment fixture operation requires a fixture-backed namespace anchor",
            ));
        }
        let mut operation = Operation::fixture(limits.into(), deadline, hook)?;
        self.prepare_task_rooted_attachment_with_operation(selector, &mut operation)
    }

    #[cfg(test)]
    pub(crate) fn prepare_task_rooted_attachment_with_clock(
        &self,
        selector: &str,
        limits: FixtureTaskRootedAttachmentLimits,
        deadline: Instant,
        hook: &mut impl FnMut(super::FixtureMountNamespaceCheckpoint) -> io::Result<()>,
        clock: &mut impl FnMut() -> Instant,
    ) -> io::Result<PreparedTaskRootedAttachment> {
        if !self._prepared.locator.is_fixture() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "attachment fixture operation requires a fixture-backed namespace anchor",
            ));
        }
        let mut operation = Operation::fixture_with_clock(limits.into(), deadline, hook, clock)?;
        self.prepare_task_rooted_attachment_with_operation(selector, &mut operation)
    }
}

impl PreparedTaskRootedAttachment {
    /// Revalidate against a snapshot-equivalent authenticated mount context.
    pub(crate) fn revalidate_against(
        &self,
        anchor: &PreparedMountNamespaceAnchor,
    ) -> io::Result<RevalidatedTaskRootedAttachment<'_>> {
        let deadline = deadline_after(PRODUCTION_TIMEOUT)?;
        self.revalidate_against_until(anchor, deadline)
    }

    /// Revalidate without replacing the caller-owned absolute deadline.
    pub(crate) fn revalidate_against_until(
        &self,
        anchor: &PreparedMountNamespaceAnchor,
        deadline: Instant,
    ) -> io::Result<RevalidatedTaskRootedAttachment<'_>> {
        #[cfg(test)]
        let mut operation = if anchor.locator.is_fixture() {
            Operation::fixture_without_hook(PRODUCTION_LIMITS, deadline)?
        } else {
            Operation::production(PRODUCTION_LIMITS, deadline)
        };
        #[cfg(not(test))]
        let mut operation = Operation::production(PRODUCTION_LIMITS, deadline);
        self.revalidate_against_with_operation(anchor, &mut operation)
    }

    fn revalidate_against_with_operation<'a>(
        &'a self,
        anchor: &PreparedMountNamespaceAnchor,
        operation: &mut Operation<'_>,
    ) -> io::Result<RevalidatedTaskRootedAttachment<'a>> {
        let opening = anchor.revalidate_with_operation(operation)?;
        require_snapshot_matches(
            self.anchor_snapshot,
            opening.current.snapshot(),
            "attachment revalidation opening anchor",
        )?;
        opening.current.require_retained(operation)?;
        operation.emit_attachment(AttachmentCheckpoint::AnchorOpened)?;

        require_same_directory(
            self.anchor_snapshot.task_root,
            directory_witness(&self.root, operation, "retained attachment task root")?,
            "retained attachment task root against anchor snapshot",
        )?;
        self.capture.require_retained(&self.root, operation)?;
        let current = capture_twice(&self.root, self.anchor_snapshot.task_root, &self.selector, operation)?;
        require_capture_matches(&self.capture, &current, "prepared attachment chain")?;
        current.require_terminal_names(&self.root, operation)?;

        operation.emit_attachment(AttachmentCheckpoint::BeforeClosingAnchor)?;
        let closing = anchor.revalidate_with_operation(operation)?;
        require_snapshot_matches(
            self.anchor_snapshot,
            closing.current.snapshot(),
            "attachment revalidation closing anchor",
        )?;
        closing.current.require_retained(operation)?;
        self.capture.require_retained(&self.root, operation)?;
        current.require_retained(&self.root, operation)?;
        operation.emit_attachment(AttachmentCheckpoint::Complete)?;
        operation.checkpoint()?;

        Ok(RevalidatedTaskRootedAttachment {
            _prepared: self,
            current,
        })
    }

    #[cfg(test)]
    pub(crate) fn revalidate_against_with(
        &self,
        anchor: &PreparedMountNamespaceAnchor,
        limits: FixtureTaskRootedAttachmentLimits,
        deadline: Instant,
        hook: &mut impl FnMut(super::FixtureMountNamespaceCheckpoint) -> io::Result<()>,
    ) -> io::Result<RevalidatedTaskRootedAttachment<'_>> {
        if !anchor.locator.is_fixture() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "attachment fixture operation requires a fixture-backed namespace anchor",
            ));
        }
        let mut operation = Operation::fixture(limits.into(), deadline, hook)?;
        self.revalidate_against_with_operation(anchor, &mut operation)
    }

    #[cfg(test)]
    pub(crate) fn revalidate_against_with_clock(
        &self,
        anchor: &PreparedMountNamespaceAnchor,
        limits: FixtureTaskRootedAttachmentLimits,
        deadline: Instant,
        hook: &mut impl FnMut(super::FixtureMountNamespaceCheckpoint) -> io::Result<()>,
        clock: &mut impl FnMut() -> Instant,
    ) -> io::Result<RevalidatedTaskRootedAttachment<'_>> {
        if !anchor.locator.is_fixture() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "attachment fixture operation requires a fixture-backed namespace anchor",
            ));
        }
        let mut operation = Operation::fixture_with_clock(limits.into(), deadline, hook, clock)?;
        self.revalidate_against_with_operation(anchor, &mut operation)
    }
}

/// Fresh scalar-only view of one completed attachment observation.
///
/// The borrow prevents the prepared descriptors from being dropped while the
/// view exists, but it is not an ongoing-currentness guarantee. Exposed scalar
/// facts can become stale immediately after their domain's last observation,
/// including before the call returns. They do not prove ongoing currentness or
/// simultaneous residency with separately observed evidence. Stable later
/// changes and ABA replacements can escape until the next aggregate
/// revalidation brackets dependent use.
pub(crate) struct RevalidatedTaskRootedAttachment<'a> {
    _prepared: &'a PreparedTaskRootedAttachment,
    current: AttachmentCapture,
}

impl RevalidatedTaskRootedAttachment<'_> {
    pub(crate) fn selector(&self) -> &str {
        self._prepared.selector.authored()
    }

    pub(crate) fn component_count(&self) -> usize {
        self.current.component_count()
    }

    pub(crate) const fn destination_device(&self) -> u64 {
        self.current.destination_witness().device
    }

    pub(crate) const fn destination_inode(&self) -> u64 {
        self.current.destination_witness().inode
    }

    pub(crate) const fn destination_mount_id(&self) -> u64 {
        self.current.destination_witness().mount_id
    }

    /// Authenticate the retained final destination without exposing it.
    ///
    /// The exact `st_dev` and `st_ino` come from this same revalidated capture.
    /// The returned closed evidence contains no descriptor or path authority.
    pub(crate) fn authenticate_boot_filesystem_until(
        &self,
        deadline: Instant,
    ) -> Result<ValidatedBootFilesystemDescriptorEvidence, BootFilesystemAuthenticationError> {
        self.current.authenticate_boot_filesystem_until(deadline)
    }

    /// Bind the retained exact task-root `/dev` destination to independently
    /// authenticated devtmpfs mountinfo and same-mount descriptor evidence.
    ///
    /// The destination descriptor remains private to the retained capture.
    /// Success contains scalars only and proves neither whole-root bind
    /// provenance nor ongoing currentness.
    pub(crate) fn authenticate_devtmpfs_attachment_until(
        &self,
        policy: ValidatedDevtmpfsMountInfoPolicy,
        deadline: Instant,
    ) -> Result<ValidatedTaskRootDevtmpfsAttachmentEvidence, TaskRootDevtmpfsAttachmentAuthenticationError> {
        device::bind_task_root_devtmpfs_attachment_until(
            self.selector(),
            self.destination_device(),
            self.destination_inode(),
            self.destination_mount_id(),
            policy,
            deadline,
            |_device, _inode, _mount_id, policy, deadline| {
                self.current.authenticate_devtmpfs_same_mount_until(policy, deadline)
            },
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn validate_fixture_devtmpfs_attachment_with(
        &self,
        policy: ValidatedDevtmpfsMountInfoPolicy,
        deadline: Instant,
        authenticate: impl FnOnce(
            u64,
            u64,
            u64,
            ValidatedDevtmpfsMountInfoPolicy,
            Instant,
        ) -> Result<
            ValidatedDevtmpfsSameMountDescriptorEvidence,
            DevtmpfsDescriptorAuthenticationError,
        >,
    ) -> Result<ValidatedTaskRootDevtmpfsAttachmentEvidence, TaskRootDevtmpfsAttachmentAuthenticationError> {
        device::bind_task_root_devtmpfs_attachment_until(
            self.selector(),
            self.destination_device(),
            self.destination_inode(),
            self.destination_mount_id(),
            policy,
            deadline,
            authenticate,
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(in crate::linux_fs) fn validate_fixture_devtmpfs_gpt_partition_device_with<Expectation, GptEvidence>(
        &self,
        policy: ValidatedDevtmpfsMountInfoPolicy,
        expected: &Expectation,
        expected_role: GptPartitionRole,
        deadline: Instant,
        authenticate_devtmpfs: impl FnOnce(
            u64,
            u64,
            u64,
            ValidatedDevtmpfsMountInfoPolicy,
            Instant,
        ) -> Result<
            ValidatedDevtmpfsSameMountDescriptorEvidence,
            DevtmpfsDescriptorAuthenticationError,
        >,
        authenticate_gpt: impl FnOnce(
            u64,
            &Expectation,
            GptPartitionRole,
            Instant,
        ) -> io::Result<gpt_device::FixtureGptPartitionDeviceEvidence<GptEvidence>>,
        clock: &mut impl FnMut() -> Instant,
    ) -> Result<
        gpt_device::FixtureValidatedTaskRootDevtmpfsGptPartitionDeviceEvidence<GptEvidence>,
        TaskRootDevtmpfsGptPartitionDeviceAuthenticationError,
    > {
        gpt_device::authenticate_fixture_until(
            self.selector(),
            self.destination_device(),
            self.destination_inode(),
            self.destination_mount_id(),
            policy,
            expected,
            expected_role,
            deadline,
            authenticate_devtmpfs,
            authenticate_gpt,
            clock,
        )
    }

    #[cfg(test)]
    pub(in crate::linux_fs) fn fixture_gpt_partition_device_evidence<GptEvidence>(
        mount_id: u64,
        evidence: GptEvidence,
    ) -> gpt_device::FixtureGptPartitionDeviceEvidence<GptEvidence> {
        gpt_device::FixtureGptPartitionDeviceEvidence::new(mount_id, evidence)
    }

    #[cfg(test)]
    pub(in crate::linux_fs) fn validate_fixture_gpt_root_mount_id(
        &self,
        authenticated_root_mount_id: u64,
    ) -> io::Result<()> {
        self.current
            .validate_fixture_gpt_root_mount_id(authenticated_root_mount_id)
    }

    /// Convert the destination `st_dev` into one exact sysfs device number.
    pub(crate) fn destination_sysfs_device_number(&self) -> io::Result<SysfsDeviceNumber> {
        sysfs_device_number_from_raw(u128::from(self.destination_device()))
    }

    #[cfg(test)]
    pub(crate) fn fixture_component_witness(&self, index: usize) -> Option<(u64, u64, u32, u64)> {
        self.current
            .component_witness(index)
            .map(|witness| (witness.device, witness.inode, witness.kind, witness.mount_id))
    }
}

fn sysfs_device_number_from_raw(device: u128) -> io::Result<SysfsDeviceNumber> {
    let raw_device: nix::libc::dev_t = device.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "attachment st_dev cannot be represented as Linux dev_t",
        )
    })?;
    let major: u32 = nix::libc::major(raw_device);
    let minor: u32 = nix::libc::minor(raw_device);
    let rebuilt = nix::libc::makedev(major, minor);
    let rebuilt_u128 = u128::from(rebuilt);
    if rebuilt != raw_device || rebuilt_u128 != device {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "attachment st_dev is not a canonical Linux major/minor encoding",
        ));
    }
    Ok(SysfsDeviceNumber::from_major_minor(major, minor))
}

#[cfg(test)]
pub(crate) fn validate_fixture_attachment_st_dev(device: u128) -> io::Result<SysfsDeviceNumber> {
    sysfs_device_number_from_raw(device)
}

impl std::fmt::Debug for RevalidatedTaskRootedAttachment<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevalidatedTaskRootedAttachment")
            .field("selector", &self.selector())
            .field("component_count", &self.component_count())
            .field("destination_device", &self.destination_device())
            .field("destination_inode", &self.destination_inode())
            .field("destination_mount_id", &self.destination_mount_id())
            .finish_non_exhaustive()
    }
}

fn deadline_after(timeout: Duration) -> io::Result<Instant> {
    Instant::now().checked_add(timeout).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "task-rooted attachment deadline overflowed",
        )
    })
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FixtureTaskRootedAttachmentLimits {
    pub(crate) max_work: usize,
    pub(crate) max_descriptors: usize,
}

#[cfg(test)]
impl Default for FixtureTaskRootedAttachmentLimits {
    fn default() -> Self {
        Self {
            max_work: PRODUCTION_MAX_WORK,
            max_descriptors: PRODUCTION_MAX_DESCRIPTORS,
        }
    }
}

#[cfg(test)]
impl From<FixtureTaskRootedAttachmentLimits> for AttachmentLimits {
    fn from(limits: FixtureTaskRootedAttachmentLimits) -> Self {
        Self {
            max_work: limits.max_work,
            max_descriptors: limits.max_descriptors,
        }
    }
}
