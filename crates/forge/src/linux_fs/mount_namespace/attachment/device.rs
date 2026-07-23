//! Closed composition of one exact task-root `/dev` attachment with
//! descriptor-bound devtmpfs evidence.
//!
//! This layer deliberately receives only the already-authored selector and
//! scalar attachment identity. The production caller captures the retained
//! destination descriptor in a private closure, so no descriptor, path, or
//! reopen authority crosses this boundary.

use std::time::Instant;

use thiserror::Error;

use crate::linux_fs::{
    descriptor_devtmpfs_filesystem::{
        DevtmpfsDescriptorAuthenticationError, DevtmpfsDescriptorMagicFamily,
        ValidatedDevtmpfsSameMountDescriptorEvidence,
    },
    mountinfo_devtmpfs_policy::{
        DevtmpfsAccessMode, DevtmpfsFilesystemKind as DevtmpfsKind, ValidatedDevtmpfsMountInfoPolicy,
    },
};

const EXACT_TASK_ROOT_DEVICE_SELECTOR: &str = "/dev";

/// Closed evidence that the exact authored task-root selector `/dev` agreed
/// with same-mount descriptor authentication for the retained destination.
///
/// This value contains scalar evidence only. It owns no descriptor or path,
/// grants no reopen or mutation authority, and proves neither whole-root bind
/// provenance nor ongoing currentness. An aggregate must freshly bracket it
/// with the task-root attachment and mountinfo authorities before dependent
/// use.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ValidatedTaskRootDevtmpfsAttachmentEvidence {
    descriptor: ValidatedDevtmpfsSameMountDescriptorEvidence,
}

impl ValidatedTaskRootDevtmpfsAttachmentEvidence {
    pub(crate) const fn selector(self) -> &'static str {
        EXACT_TASK_ROOT_DEVICE_SELECTOR
    }

    pub(crate) const fn directory_device(self) -> u64 {
        self.descriptor.directory_device()
    }

    pub(crate) const fn directory_inode(self) -> u64 {
        self.descriptor.directory_inode()
    }

    pub(crate) const fn mount_id(self) -> u64 {
        self.descriptor.mount_id()
    }

    pub(crate) const fn filesystem(self) -> DevtmpfsKind {
        self.descriptor.filesystem()
    }

    pub(crate) const fn access_mode(self) -> DevtmpfsAccessMode {
        self.descriptor.access_mode()
    }

    pub(crate) const fn magic_family(self) -> DevtmpfsDescriptorMagicFamily {
        self.descriptor.magic_family()
    }
}

#[derive(Debug, Error)]
pub(crate) enum TaskRootDevtmpfsAttachmentAuthenticationError {
    #[error("task-root devtmpfs attachment selector is not exactly /dev")]
    UnexpectedSelector,
    #[error("task-root devtmpfs attachment exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("retained task-root devtmpfs destination authentication failed")]
    DescriptorAuthentication {
        #[source]
        source: DevtmpfsDescriptorAuthenticationError,
    },
    #[error("descriptor evidence does not retain the exact task-root attachment identity")]
    DescriptorIdentityMismatch,
    #[error("descriptor evidence does not retain the supplied devtmpfs mount policy")]
    DescriptorPolicyMismatch,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn bind_task_root_devtmpfs_attachment_until(
    selector: &str,
    expected_device: u64,
    expected_inode: u64,
    expected_mount_id: u64,
    policy: ValidatedDevtmpfsMountInfoPolicy,
    deadline: Instant,
    authenticate: impl FnOnce(
        u64,
        u64,
        u64,
        ValidatedDevtmpfsMountInfoPolicy,
        Instant,
    )
        -> Result<ValidatedDevtmpfsSameMountDescriptorEvidence, DevtmpfsDescriptorAuthenticationError>,
) -> Result<ValidatedTaskRootDevtmpfsAttachmentEvidence, TaskRootDevtmpfsAttachmentAuthenticationError> {
    // This exact authored-selector guard deliberately precedes the private
    // authenticator, and therefore every descriptor and procfs observation.
    if selector != EXACT_TASK_ROOT_DEVICE_SELECTOR {
        return Err(TaskRootDevtmpfsAttachmentAuthenticationError::UnexpectedSelector);
    }
    require_deadline(deadline)?;

    let descriptor = authenticate(expected_device, expected_inode, expected_mount_id, policy, deadline)
        .map_err(|source| TaskRootDevtmpfsAttachmentAuthenticationError::DescriptorAuthentication { source })?;
    require_deadline(deadline)?;

    if descriptor.directory_device() != expected_device
        || descriptor.directory_inode() != expected_inode
        || descriptor.mount_id() != expected_mount_id
    {
        return Err(TaskRootDevtmpfsAttachmentAuthenticationError::DescriptorIdentityMismatch);
    }
    if descriptor.filesystem() != policy.filesystem() || descriptor.access_mode() != policy.access_mode() {
        return Err(TaskRootDevtmpfsAttachmentAuthenticationError::DescriptorPolicyMismatch);
    }

    require_deadline(deadline)?;
    Ok(ValidatedTaskRootDevtmpfsAttachmentEvidence { descriptor })
}

fn require_deadline(deadline: Instant) -> Result<(), TaskRootDevtmpfsAttachmentAuthenticationError> {
    if Instant::now() > deadline {
        Err(TaskRootDevtmpfsAttachmentAuthenticationError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}
