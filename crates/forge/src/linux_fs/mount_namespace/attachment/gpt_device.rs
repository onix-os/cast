//! Closed composition of exact task-root `/dev` and owned GPT authentication.
//!
//! This adapter fixes the order and wiring between two independent authorities:
//! exact authored attachment plus same-mount devtmpfs authentication completes
//! first, then owned GPT parent authentication receives the resulting mount ID,
//! the same sysfs expectation, the same role, and the same absolute deadline.
//! The retained destination file remains inside the same retained attachment
//! capture and never crosses the attachment's public API boundary.

use std::{io, time::Instant};

use thiserror::Error;

#[cfg(test)]
use super::device::bind_task_root_devtmpfs_attachment_until;
use super::{
    RevalidatedTaskRootedAttachment, TaskRootDevtmpfsAttachmentAuthenticationError,
    ValidatedTaskRootDevtmpfsAttachmentEvidence,
};
#[cfg(test)]
use crate::linux_fs::descriptor_devtmpfs_filesystem::{
    DevtmpfsDescriptorAuthenticationError, ValidatedDevtmpfsSameMountDescriptorEvidence,
};
use crate::linux_fs::{
    descriptor_devtmpfs_filesystem::DevtmpfsDescriptorMagicFamily,
    gpt_partition_device::LiveAuthenticatedGptPartitionDeviceEvidence,
    gpt_partition_role::GptPartitionRole,
    mountinfo_devtmpfs_policy::{DevtmpfsAccessMode, DevtmpfsFilesystemKind, ValidatedDevtmpfsMountInfoPolicy},
    sysfs_identity::SysfsGptDeviceExpectation,
};

impl RevalidatedTaskRootedAttachment<'_> {
    /// Authenticate a GPT parent below this exact retained task-root `/dev`.
    ///
    /// The authored `/dev` selection and its same-mount devtmpfs binding are
    /// authenticated first. Only then may the GPT layer resolve the exact
    /// parent named by `expected` below the same private retained destination
    /// descriptor and the same captured mount ID. The same expectation and
    /// role are passed unchanged into the owned GPT authentication schedule.
    ///
    /// Success is closed scalar evidence for this bounded schedule. It owns no
    /// descriptor, path, callback, observer, image, or reopen authority, and
    /// does not prove ongoing currentness of `/dev`, sysfs, or the block node.
    /// Thread affinity prevents moving the retained view across threads; it
    /// does not prevent `setns(2)` on the same thread between calls. An outer
    /// authority must therefore bracket dependent use with fresh mount-context
    /// and attachment revalidation.
    pub(in crate::linux_fs) fn authenticate_devtmpfs_gpt_partition_device_until(
        &self,
        policy: ValidatedDevtmpfsMountInfoPolicy,
        expected: &SysfsGptDeviceExpectation<'_>,
        expected_role: GptPartitionRole,
        deadline: Instant,
    ) -> Result<
        ValidatedTaskRootDevtmpfsGptPartitionDeviceEvidence,
        TaskRootDevtmpfsGptPartitionDeviceAuthenticationError,
    > {
        let devtmpfs_attachment = self
            .authenticate_devtmpfs_attachment_until(policy, deadline)
            .map_err(|source| TaskRootDevtmpfsGptPartitionDeviceAuthenticationError::DevtmpfsAttachment { source })?;
        require_deadline(deadline)?;

        let gpt_partition_device = self
            .current
            .authenticate_gpt_parent_until(devtmpfs_attachment.mount_id(), expected, expected_role, deadline)
            .map_err(|source| TaskRootDevtmpfsGptPartitionDeviceAuthenticationError::GptAuthentication { source })?;
        bind_authenticated_evidence_until(devtmpfs_attachment, gpt_partition_device, deadline)
    }
}

/// Closed scalar evidence for one bounded `/dev` plus GPT authentication.
///
/// This value owns both the exact task-root devtmpfs attachment evidence and
/// the owned-parent GPT read evidence. It contains no descriptor, file, path,
/// observer, callback, image, or reopen authority. The two layers ran in one
/// fixed order over the same retained root descriptor and captured mount ID,
/// but the result does not claim ongoing currentness of any public name or
/// kernel object after its final observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::linux_fs) struct ValidatedTaskRootDevtmpfsGptPartitionDeviceEvidence {
    devtmpfs_attachment: ValidatedTaskRootDevtmpfsAttachmentEvidence,
    gpt_partition_device: LiveAuthenticatedGptPartitionDeviceEvidence,
}

impl ValidatedTaskRootDevtmpfsGptPartitionDeviceEvidence {
    pub(in crate::linux_fs) const fn selector(&self) -> &'static str {
        self.devtmpfs_attachment.selector()
    }

    pub(in crate::linux_fs) const fn devtmpfs_directory_device(&self) -> u64 {
        self.devtmpfs_attachment.directory_device()
    }

    pub(in crate::linux_fs) const fn devtmpfs_directory_inode(&self) -> u64 {
        self.devtmpfs_attachment.directory_inode()
    }

    pub(in crate::linux_fs) const fn devtmpfs_mount_id(&self) -> u64 {
        self.devtmpfs_attachment.mount_id()
    }

    pub(in crate::linux_fs) const fn devtmpfs_filesystem(&self) -> DevtmpfsFilesystemKind {
        self.devtmpfs_attachment.filesystem()
    }

    pub(in crate::linux_fs) const fn devtmpfs_access_mode(&self) -> DevtmpfsAccessMode {
        self.devtmpfs_attachment.access_mode()
    }

    pub(in crate::linux_fs) const fn devtmpfs_magic_family(&self) -> DevtmpfsDescriptorMagicFamily {
        self.devtmpfs_attachment.magic_family()
    }

    pub(in crate::linux_fs) const fn gpt_containing_device(&self) -> u64 {
        self.gpt_partition_device.containing_device()
    }

    pub(in crate::linux_fs) const fn gpt_inode(&self) -> u64 {
        self.gpt_partition_device.inode()
    }

    pub(in crate::linux_fs) const fn gpt_mount_id(&self) -> u64 {
        self.gpt_partition_device.mount_id()
    }

    pub(in crate::linux_fs) const fn parent_major(&self) -> u32 {
        self.gpt_partition_device.parent_major()
    }

    pub(in crate::linux_fs) const fn parent_minor(&self) -> u32 {
        self.gpt_partition_device.parent_minor()
    }

    pub(in crate::linux_fs) const fn logical_block_size(&self) -> u32 {
        self.gpt_partition_device.logical_block_size()
    }

    pub(in crate::linux_fs) const fn device_byte_length(&self) -> u64 {
        self.gpt_partition_device.device_byte_length()
    }

    pub(in crate::linux_fs) const fn partition_number(&self) -> u32 {
        self.gpt_partition_device.partition_number()
    }

    pub(in crate::linux_fs) fn partition_uuid(&self) -> &str {
        self.gpt_partition_device.partition_uuid()
    }

    pub(in crate::linux_fs) const fn partition_start_bytes(&self) -> u64 {
        self.gpt_partition_device.partition_start_bytes()
    }

    pub(in crate::linux_fs) const fn partition_size_bytes(&self) -> u64 {
        self.gpt_partition_device.partition_size_bytes()
    }

    pub(in crate::linux_fs) const fn role(&self) -> GptPartitionRole {
        self.gpt_partition_device.role()
    }

    pub(in crate::linux_fs) const fn table_sha256(&self) -> &[u8; 32] {
        self.gpt_partition_device.table_sha256()
    }
}

#[derive(Debug, Error)]
pub(in crate::linux_fs) enum TaskRootDevtmpfsGptPartitionDeviceAuthenticationError {
    #[error("task-root devtmpfs attachment authentication failed before GPT authentication")]
    DevtmpfsAttachment {
        #[source]
        source: TaskRootDevtmpfsAttachmentAuthenticationError,
    },
    #[error("task-root devtmpfs and GPT composition exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("owned GPT parent authentication failed below the retained task-root /dev")]
    GptAuthentication {
        #[source]
        source: io::Error,
    },
    #[error(
        "GPT evidence mount ID {gpt_mount_id} does not retain the authenticated task-root devtmpfs mount ID {devtmpfs_mount_id}"
    )]
    GptMountMismatch { devtmpfs_mount_id: u64, gpt_mount_id: u64 },
}

/// Seal the two concrete evidence types after checking their own mount IDs.
///
/// Neither evidence type is caller-constructible. In particular, there is no
/// generic callback, tuple, claimed mount ID, or arbitrary payload seam in the
/// production composition path.
fn bind_authenticated_evidence_until(
    devtmpfs_attachment: ValidatedTaskRootDevtmpfsAttachmentEvidence,
    gpt_partition_device: LiveAuthenticatedGptPartitionDeviceEvidence,
    deadline: Instant,
) -> Result<ValidatedTaskRootDevtmpfsGptPartitionDeviceEvidence, TaskRootDevtmpfsGptPartitionDeviceAuthenticationError>
{
    require_deadline(deadline)?;
    let devtmpfs_mount_id = devtmpfs_attachment.mount_id();
    let gpt_mount_id = gpt_partition_device.mount_id();
    if gpt_mount_id != devtmpfs_mount_id {
        return Err(
            TaskRootDevtmpfsGptPartitionDeviceAuthenticationError::GptMountMismatch {
                devtmpfs_mount_id,
                gpt_mount_id,
            },
        );
    }
    require_deadline(deadline)?;
    Ok(ValidatedTaskRootDevtmpfsGptPartitionDeviceEvidence {
        devtmpfs_attachment,
        gpt_partition_device,
    })
}

fn require_deadline(deadline: Instant) -> Result<(), TaskRootDevtmpfsGptPartitionDeviceAuthenticationError> {
    if Instant::now() > deadline {
        Err(TaskRootDevtmpfsGptPartitionDeviceAuthenticationError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FixtureGptPartitionDeviceEvidence<Payload> {
    mount_id: u64,
    payload: Payload,
}

#[cfg(test)]
impl<Payload> FixtureGptPartitionDeviceEvidence<Payload> {
    pub(super) const fn new(mount_id: u64, payload: Payload) -> Self {
        Self { mount_id, payload }
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FixtureValidatedTaskRootDevtmpfsGptPartitionDeviceEvidence<Payload> {
    devtmpfs_attachment: ValidatedTaskRootDevtmpfsAttachmentEvidence,
    gpt_partition_device: FixtureGptPartitionDeviceEvidence<Payload>,
}

#[cfg(test)]
impl<Payload> FixtureValidatedTaskRootDevtmpfsGptPartitionDeviceEvidence<Payload> {
    pub(crate) const fn devtmpfs_attachment(&self) -> ValidatedTaskRootDevtmpfsAttachmentEvidence {
        self.devtmpfs_attachment
    }

    pub(crate) const fn gpt_partition_device(&self) -> &Payload {
        &self.gpt_partition_device.payload
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn authenticate_fixture_until<Expectation, Payload>(
    selector: &str,
    expected_device: u64,
    expected_inode: u64,
    expected_mount_id: u64,
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
    ) -> io::Result<FixtureGptPartitionDeviceEvidence<Payload>>,
    clock: &mut impl FnMut() -> Instant,
) -> Result<
    FixtureValidatedTaskRootDevtmpfsGptPartitionDeviceEvidence<Payload>,
    TaskRootDevtmpfsGptPartitionDeviceAuthenticationError,
> {
    let devtmpfs_attachment = bind_task_root_devtmpfs_attachment_until(
        selector,
        expected_device,
        expected_inode,
        expected_mount_id,
        policy,
        deadline,
        authenticate_devtmpfs,
    )
    .map_err(|source| TaskRootDevtmpfsGptPartitionDeviceAuthenticationError::DevtmpfsAttachment { source })?;
    require_deadline_with_clock(deadline, clock)?;

    let devtmpfs_mount_id = devtmpfs_attachment.mount_id();
    let gpt_partition_device = authenticate_gpt(devtmpfs_mount_id, expected, expected_role, deadline)
        .map_err(|source| TaskRootDevtmpfsGptPartitionDeviceAuthenticationError::GptAuthentication { source })?;
    require_deadline_with_clock(deadline, clock)?;
    if gpt_partition_device.mount_id != devtmpfs_mount_id {
        return Err(
            TaskRootDevtmpfsGptPartitionDeviceAuthenticationError::GptMountMismatch {
                devtmpfs_mount_id,
                gpt_mount_id: gpt_partition_device.mount_id,
            },
        );
    }
    require_deadline_with_clock(deadline, clock)?;

    Ok(FixtureValidatedTaskRootDevtmpfsGptPartitionDeviceEvidence {
        devtmpfs_attachment,
        gpt_partition_device,
    })
}

#[cfg(test)]
fn require_deadline_with_clock(
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> Result<(), TaskRootDevtmpfsGptPartitionDeviceAuthenticationError> {
    if clock() > deadline {
        Err(TaskRootDevtmpfsGptPartitionDeviceAuthenticationError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}
