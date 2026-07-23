//! Closed composition of one retained boot attachment and namespace assessment.
//!
//! The production entry point is an inherent operation on the freshly
//! revalidated attachment view. It authenticates the boot filesystem, assesses
//! a nonempty request set through the same private destination descriptor,
//! authenticates the filesystem again, and only then seals matching scalar
//! evidence. No descriptor, path, callback, reader, reopen operation, or
//! mutation authority survives.

use std::time::Instant;

use thiserror::Error;

use super::RevalidatedTaskRootedAttachment;
use crate::linux_fs::{
    descriptor_boot_filesystem::{
        BootFilesystemAuthenticationError, BootFilesystemMagicFamily, ValidatedBootFilesystemDescriptorEvidence,
    },
    descriptor_boot_namespace::{
        BootNamespaceAssessmentLimits, BootNamespaceDestinationState, BootNamespaceRequest,
        RetainedBootNamespaceAssessmentError, RetainedBootNamespaceAssessmentLimits,
        RetainedBootNamespaceExpectedSource, ValidatedRetainedBootNamespaceAssessment,
    },
};

/// Scalar-only evidence for one bounded retained boot-namespace assessment.
///
/// The value describes the exact retained descriptor used during the bounded
/// schedule, but it is not proof that the authored public name remains current.
/// A caller must freshly revalidate its mount context and attachment around any
/// dependent publication attempt.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ValidatedTaskRootBootNamespaceAssessment {
    boot_filesystem: ValidatedBootFilesystemDescriptorEvidence,
    destination_mount_id: u64,
    namespace: ValidatedRetainedBootNamespaceAssessment,
}

impl ValidatedTaskRootBootNamespaceAssessment {
    pub(crate) const fn destination_device(&self) -> u64 {
        self.boot_filesystem.destination_device()
    }

    pub(crate) const fn destination_inode(&self) -> u64 {
        self.boot_filesystem.destination_inode()
    }

    pub(crate) const fn destination_mount_id(&self) -> u64 {
        self.destination_mount_id
    }

    pub(crate) const fn boot_filesystem_magic_family(&self) -> BootFilesystemMagicFamily {
        self.boot_filesystem.magic_family()
    }

    pub(crate) fn states(&self) -> &[BootNamespaceDestinationState] {
        self.namespace.states()
    }
}

#[derive(Debug, Error)]
pub(crate) enum TaskRootBootNamespaceAssessmentError {
    #[error("retained boot-namespace attachment assessment requires at least one request")]
    EmptyRequestSet,
    #[error("retained boot-namespace attachment assessment exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("opening retained boot-filesystem authentication failed")]
    OpeningBootFilesystem {
        #[source]
        source: BootFilesystemAuthenticationError,
    },
    #[error("retained boot-namespace assessment failed")]
    NamespaceAssessment {
        #[source]
        source: RetainedBootNamespaceAssessmentError,
    },
    #[error("closing retained boot-filesystem authentication failed")]
    ClosingBootFilesystem {
        #[source]
        source: BootFilesystemAuthenticationError,
    },
    #[error("opening and closing retained boot-filesystem evidence differed")]
    BootFilesystemEvidenceDrift,
    #[error(
        "retained boot-filesystem evidence does not match attachment identity: expected st_dev {expected_device}, st_ino {expected_inode}, found st_dev {found_device}, st_ino {found_inode}"
    )]
    BootFilesystemIdentityMismatch {
        expected_device: u64,
        expected_inode: u64,
        found_device: u64,
        found_inode: u64,
    },
    #[error("successful retained boot-namespace assessment omitted observed root identity")]
    MissingObservedRootIdentity,
    #[error(
        "retained boot-namespace root does not match attachment: expected st_dev {expected_device}, st_ino {expected_inode}, mount ID {expected_mount_id}, found st_dev {found_device}, st_ino {found_inode}, mount ID {found_mount_id}"
    )]
    ObservedRootIdentityMismatch {
        expected_device: u64,
        expected_inode: u64,
        expected_mount_id: u64,
        found_device: u64,
        found_inode: u64,
        found_mount_id: u64,
    },
}

#[derive(Clone, Copy)]
struct AttachmentDestinationIdentity {
    device: u64,
    inode: u64,
    mount_id: u64,
}

#[derive(Clone, Copy, Debug)]
struct ObservedRootIdentity {
    device: u64,
    inode: u64,
    mount_id: u64,
}

impl RevalidatedTaskRootedAttachment<'_> {
    /// Assess a nonempty namespace below this exact retained boot destination.
    ///
    /// Every stage receives the caller's original absolute deadline. The
    /// descriptor stays private to the attachment capture. Success is a closed
    /// observation, not ongoing mount-name or namespace-currentness authority.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn assess_retained_boot_namespace_until(
        &self,
        requests: &[BootNamespaceRequest<'_>],
        expected: &[RetainedBootNamespaceExpectedSource<'_>],
        namespace_limits: BootNamespaceAssessmentLimits,
        live_limits: RetainedBootNamespaceAssessmentLimits,
        deadline: Instant,
    ) -> Result<ValidatedTaskRootBootNamespaceAssessment, TaskRootBootNamespaceAssessmentError> {
        if requests.is_empty() {
            return Err(TaskRootBootNamespaceAssessmentError::EmptyRequestSet);
        }
        require_deadline(deadline)?;
        let opening = self
            .current
            .authenticate_boot_filesystem_until(deadline)
            .map_err(|source| TaskRootBootNamespaceAssessmentError::OpeningBootFilesystem { source })?;
        require_deadline(deadline)?;
        let namespace = self
            .current
            .assess_retained_boot_namespace_until(requests, expected, namespace_limits, live_limits, deadline)
            .map_err(|source| TaskRootBootNamespaceAssessmentError::NamespaceAssessment { source })?;
        require_deadline(deadline)?;
        let closing = self
            .current
            .authenticate_boot_filesystem_until(deadline)
            .map_err(|source| TaskRootBootNamespaceAssessmentError::ClosingBootFilesystem { source })?;
        close_assessment_until(
            AttachmentDestinationIdentity {
                device: self.destination_device(),
                inode: self.destination_inode(),
                mount_id: self.destination_mount_id(),
            },
            opening,
            closing,
            namespace,
            deadline,
        )
    }
}

fn close_assessment_until(
    destination: AttachmentDestinationIdentity,
    opening: ValidatedBootFilesystemDescriptorEvidence,
    closing: ValidatedBootFilesystemDescriptorEvidence,
    namespace: ValidatedRetainedBootNamespaceAssessment,
    deadline: Instant,
) -> Result<ValidatedTaskRootBootNamespaceAssessment, TaskRootBootNamespaceAssessmentError> {
    let observed_root = namespace.observed_root_identity().map(|root| ObservedRootIdentity {
        device: root.device,
        inode: root.inode,
        mount_id: root.mount_id,
    });
    validate_closed_evidence(destination, opening, closing, observed_root)?;
    require_deadline(deadline)?;
    Ok(ValidatedTaskRootBootNamespaceAssessment {
        boot_filesystem: opening,
        destination_mount_id: destination.mount_id,
        namespace,
    })
}

fn validate_closed_evidence(
    destination: AttachmentDestinationIdentity,
    opening: ValidatedBootFilesystemDescriptorEvidence,
    closing: ValidatedBootFilesystemDescriptorEvidence,
    observed_root: Option<ObservedRootIdentity>,
) -> Result<(), TaskRootBootNamespaceAssessmentError> {
    if opening != closing {
        return Err(TaskRootBootNamespaceAssessmentError::BootFilesystemEvidenceDrift);
    }
    if opening.destination_device() != destination.device || opening.destination_inode() != destination.inode {
        return Err(TaskRootBootNamespaceAssessmentError::BootFilesystemIdentityMismatch {
            expected_device: destination.device,
            expected_inode: destination.inode,
            found_device: opening.destination_device(),
            found_inode: opening.destination_inode(),
        });
    }
    let root = observed_root.ok_or(TaskRootBootNamespaceAssessmentError::MissingObservedRootIdentity)?;
    if root.device != destination.device || root.inode != destination.inode || root.mount_id != destination.mount_id {
        return Err(TaskRootBootNamespaceAssessmentError::ObservedRootIdentityMismatch {
            expected_device: destination.device,
            expected_inode: destination.inode,
            expected_mount_id: destination.mount_id,
            found_device: root.device,
            found_inode: root.inode,
            found_mount_id: root.mount_id,
        });
    }
    Ok(())
}

fn require_deadline(deadline: Instant) -> Result<(), TaskRootBootNamespaceAssessmentError> {
    if Instant::now() > deadline {
        Err(TaskRootBootNamespaceAssessmentError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FixtureRetainedBootNamespaceAssessment<Payload> {
    observed_root: Option<ObservedRootIdentity>,
    payload: Payload,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FixtureValidatedTaskRootBootNamespaceAssessment<Payload> {
    boot_filesystem: ValidatedBootFilesystemDescriptorEvidence,
    destination_mount_id: u64,
    namespace: FixtureRetainedBootNamespaceAssessment<Payload>,
}

#[cfg(test)]
impl<Payload> FixtureValidatedTaskRootBootNamespaceAssessment<Payload> {
    pub(crate) const fn destination_device(&self) -> u64 {
        self.boot_filesystem.destination_device()
    }

    pub(crate) const fn destination_inode(&self) -> u64 {
        self.boot_filesystem.destination_inode()
    }

    pub(crate) const fn destination_mount_id(&self) -> u64 {
        self.destination_mount_id
    }

    pub(crate) const fn boot_filesystem_magic_family(&self) -> BootFilesystemMagicFamily {
        self.boot_filesystem.magic_family()
    }

    pub(crate) const fn payload(&self) -> &Payload {
        &self.namespace.payload
    }
}

#[cfg(test)]
impl RevalidatedTaskRootedAttachment<'_> {
    pub(crate) fn fixture_retained_boot_namespace_assessment<Payload>(
        observed_root: Option<crate::linux_fs::descriptor_boot_namespace::BootNamespaceNodeIdentity>,
        payload: Payload,
    ) -> FixtureRetainedBootNamespaceAssessment<Payload> {
        FixtureRetainedBootNamespaceAssessment {
            observed_root: observed_root.map(|root| ObservedRootIdentity {
                device: root.device,
                inode: root.inode,
                mount_id: root.mount_id,
            }),
            payload,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn validate_fixture_retained_boot_namespace_with<Payload>(
        &self,
        requests: &[BootNamespaceRequest<'_>],
        expected: &[RetainedBootNamespaceExpectedSource<'_>],
        namespace_limits: BootNamespaceAssessmentLimits,
        live_limits: RetainedBootNamespaceAssessmentLimits,
        deadline: Instant,
        authenticate_opening: impl FnOnce(
            u64,
            u64,
            Instant,
        ) -> Result<
            ValidatedBootFilesystemDescriptorEvidence,
            BootFilesystemAuthenticationError,
        >,
        assess_namespace: impl FnOnce(
            &[BootNamespaceRequest<'_>],
            &[RetainedBootNamespaceExpectedSource<'_>],
            BootNamespaceAssessmentLimits,
            RetainedBootNamespaceAssessmentLimits,
            Instant,
        ) -> Result<
            FixtureRetainedBootNamespaceAssessment<Payload>,
            RetainedBootNamespaceAssessmentError,
        >,
        authenticate_closing: impl FnOnce(
            u64,
            u64,
            Instant,
        ) -> Result<
            ValidatedBootFilesystemDescriptorEvidence,
            BootFilesystemAuthenticationError,
        >,
        clock: &mut impl FnMut() -> Instant,
    ) -> Result<FixtureValidatedTaskRootBootNamespaceAssessment<Payload>, TaskRootBootNamespaceAssessmentError> {
        if requests.is_empty() {
            return Err(TaskRootBootNamespaceAssessmentError::EmptyRequestSet);
        }
        require_deadline_with_clock(deadline, clock)?;
        let opening = authenticate_opening(self.destination_device(), self.destination_inode(), deadline)
            .map_err(|source| TaskRootBootNamespaceAssessmentError::OpeningBootFilesystem { source })?;
        require_deadline_with_clock(deadline, clock)?;
        let namespace = assess_namespace(requests, expected, namespace_limits, live_limits, deadline)
            .map_err(|source| TaskRootBootNamespaceAssessmentError::NamespaceAssessment { source })?;
        require_deadline_with_clock(deadline, clock)?;
        let closing = authenticate_closing(self.destination_device(), self.destination_inode(), deadline)
            .map_err(|source| TaskRootBootNamespaceAssessmentError::ClosingBootFilesystem { source })?;
        let destination = AttachmentDestinationIdentity {
            device: self.destination_device(),
            inode: self.destination_inode(),
            mount_id: self.destination_mount_id(),
        };
        validate_closed_evidence(destination, opening, closing, namespace.observed_root)?;
        require_deadline_with_clock(deadline, clock)?;
        Ok(FixtureValidatedTaskRootBootNamespaceAssessment {
            boot_filesystem: opening,
            destination_mount_id: destination.mount_id,
            namespace,
        })
    }
}

#[cfg(test)]
fn require_deadline_with_clock(
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> Result<(), TaskRootBootNamespaceAssessmentError> {
    if clock() > deadline {
        Err(TaskRootBootNamespaceAssessmentError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}
