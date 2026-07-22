//! Opaque publication-target bridge over retained mounted-topology attachments.
//!
//! A successful bridge owns freshly revalidated attachment views for exactly
//! the ESP/XBOOTLDR shape admitted by the complete mounted topology. The
//! complete topology is revalidated before and after those views are captured,
//! and every view is bound to the retained destination device, inode, and mount
//! ID. The caller's original absolute deadline is used unchanged throughout.
//!
//! No descriptor, selector, pathname, reopen operation, namespace mutation, or
//! publication operation is exposed here. Later aggregate operations must be
//! implemented as narrow methods on these opaque targets so the underlying
//! attachment authority cannot escape.

use std::{io, time::Instant};

use thiserror::Error;

use crate::linux_fs::{
    descriptor_boot_namespace::{
        BootNamespaceAssessmentLimits, BootNamespaceRequest,
        RetainedBootNamespaceAssessmentLimits,
        RetainedBootNamespaceExpectedSource,
    },
    mount_namespace::{
        RevalidatedTaskRootedAttachment,
        TaskRootBootNamespaceAssessmentError,
        ValidatedTaskRootBootNamespaceAssessment,
    },
};

use super::{
    model::{
        PreparedMountedBootTarget, PreparedMountedBootTargets,
        RevalidatedActiveReblitMountedBootTopology,
    },
    ActiveReblitMountedBootTopologyCaptureError,
};
use super::super::{
    BootTargetRole, BoundActiveReblitMountedBootTarget,
    BoundActiveReblitMountedBootTopology, MountedBootDestinationIdentity,
};

#[path = "publication_targets/immutable_leaf.rs"]
mod immutable_leaf;
#[path = "publication_targets/owned_replacement.rs"]
mod owned_replacement;

pub(in crate::client) use immutable_leaf::ActiveReblitBootImmutableLeafPublicationError;
#[allow(unused_imports)] // consumed by the aggregate owned-replacement executor
pub(in crate::client) use owned_replacement::ActiveReblitBootOwnedLeafReplacementError;
#[cfg(test)]
pub(in crate::client) use immutable_leaf::{
    FixtureImmutableLeafAssessmentGuard,
    arm_fixture_immutable_leaf_assessments,
    fixture_immutable_leaf_assessments_remaining,
};

/// Failure while bracketing opaque publication targets with full topology
/// revalidation.
///
/// Every variant contains diagnostic scalars only. No descriptor, path, or
/// callback authority can escape through an error.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootPublicationTargetsError {
    #[error("boot publication-target capture exceeded the retained caller deadline {deadline:?} at {checkpoint}")]
    DeadlineExceeded {
        checkpoint: &'static str,
        deadline: Instant,
    },
    #[error("opening complete mounted boot-topology revalidation failed")]
    OpeningTopology {
        #[source]
        source: ActiveReblitMountedBootTopologyCaptureError,
    },
    #[error("closing complete mounted boot-topology revalidation failed")]
    ClosingTopology {
        #[source]
        source: ActiveReblitMountedBootTopologyCaptureError,
    },
    #[error("{role:?} prepared publication attachment revalidation failed")]
    Attachment {
        role: BootTargetRole,
        #[source]
        source: io::Error,
    },
    #[error("the complete mounted topology and prepared publication attachments have different shapes")]
    TopologyShapeChanged,
    #[error(
        "{role:?} publication target identity differs from retained topology: expected st_dev {expected_device}, st_ino {expected_inode}, mount ID {expected_mount_id}, found st_dev {found_device}, st_ino {found_inode}, mount ID {found_mount_id}"
    )]
    TargetIdentityMismatch {
        role: BootTargetRole,
        expected_device: u64,
        expected_inode: u64,
        expected_mount_id: u64,
        found_device: u64,
        found_inode: u64,
        found_mount_id: u64,
    },
}

/// One role-typed publication target with its attachment view kept private.
///
/// The scalar accessors are evidence for joins and diagnostics only. Mutation
/// must remain behind future module-owned methods which revalidate the retained
/// attachment again under the same aggregate schedule.
pub(in crate::client) struct RevalidatedActiveReblitBootPublicationTarget<'prepared> {
    role: BootTargetRole,
    attachment: RevalidatedTaskRootedAttachment<'prepared>,
    destination: MountedBootDestinationIdentity,
    mount_id: u64,
    deadline: Instant,
}

impl std::fmt::Debug for RevalidatedActiveReblitBootPublicationTarget<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevalidatedActiveReblitBootPublicationTarget")
            .field("role", &self.role)
            .field("destination", &self.destination)
            .field("mount_id", &self.mount_id)
            .field("deadline", &self.deadline)
            .field("authority", &"retained; descriptor hidden")
            .finish()
    }
}

impl RevalidatedActiveReblitBootPublicationTarget<'_> {
    pub(in crate::client) const fn role(&self) -> BootTargetRole {
        self.role
    }

    pub(in crate::client) const fn destination(&self) -> MountedBootDestinationIdentity {
        self.destination
    }

    pub(in crate::client) const fn mount_id(&self) -> u64 {
        self.mount_id
    }

    pub(in crate::client) const fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Assess one already-bound collision domain through this exact retained
    /// target without exposing its attachment or accepting a fresh deadline.
    ///
    /// The returned value contains scalar evidence only. In particular, this
    /// operation grants no publication, replacement, removal, or descriptor
    /// authority to its caller.
    pub(in crate::client) fn assess_boot_namespace(
        &self,
        requests: &[BootNamespaceRequest<'_>],
        expected: &[RetainedBootNamespaceExpectedSource<'_>],
    ) -> Result<
        ValidatedTaskRootBootNamespaceAssessment,
        TaskRootBootNamespaceAssessmentError,
    > {
        self.attachment.assess_retained_boot_namespace_until(
            requests,
            expected,
            BootNamespaceAssessmentLimits::default(),
            RetainedBootNamespaceAssessmentLimits::default(),
            self.deadline,
        )
    }
}

/// Fresh alias/distinct publication-target authority, bound to one retained
/// mounted topology and deliberately non-`Clone`.
pub(in crate::client) enum RevalidatedActiveReblitBootPublicationTargets<'prepared> {
    BootAliasesEsp {
        esp: RevalidatedActiveReblitBootPublicationTarget<'prepared>,
    },
    DistinctXbootldr {
        esp: RevalidatedActiveReblitBootPublicationTarget<'prepared>,
        xbootldr: RevalidatedActiveReblitBootPublicationTarget<'prepared>,
    },
}

impl std::fmt::Debug for RevalidatedActiveReblitBootPublicationTargets<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BootAliasesEsp { esp } => formatter
                .debug_struct("RevalidatedActiveReblitBootPublicationTargets::BootAliasesEsp")
                .field("esp", esp)
                .finish(),
            Self::DistinctXbootldr { esp, xbootldr } => formatter
                .debug_struct("RevalidatedActiveReblitBootPublicationTargets::DistinctXbootldr")
                .field("esp", esp)
                .field("xbootldr", xbootldr)
                .finish(),
        }
    }
}

impl RevalidatedActiveReblitBootPublicationTargets<'_> {
    pub(in crate::client) fn deadline(&self) -> Instant {
        match self {
            Self::BootAliasesEsp { esp } | Self::DistinctXbootldr { esp, .. } => esp.deadline(),
        }
    }

    fn require_matches(
        &self,
        topology: BoundActiveReblitMountedBootTopology<'_>,
    ) -> Result<(), ActiveReblitBootPublicationTargetsError> {
        match (self, topology) {
            (Self::BootAliasesEsp { esp }, BoundActiveReblitMountedBootTopology::BootAliasesEsp { esp: facts }) => {
                require_bound_identity(esp.role, facts, observed_identity(&esp.attachment))
            }
            (
                Self::DistinctXbootldr { esp, xbootldr },
                BoundActiveReblitMountedBootTopology::DistinctXbootldr {
                    esp: esp_facts,
                    xbootldr: xbootldr_facts,
                },
            ) => {
                require_bound_identity(esp.role, esp_facts, observed_identity(&esp.attachment))?;
                require_bound_identity(
                    xbootldr.role,
                    xbootldr_facts,
                    observed_identity(&xbootldr.attachment),
                )
            }
            _ => Err(ActiveReblitBootPublicationTargetsError::TopologyShapeChanged),
        }
    }
}

impl RevalidatedActiveReblitMountedBootTopology<'_> {
    /// Revalidate the complete topology around exact retained attachment views.
    ///
    /// The returned borrow cannot outlive this topology view, and every stage
    /// inherits its original caller-owned absolute deadline unchanged.
    pub(in crate::client) fn revalidate_publication_targets<'view>(
        &'view self,
    ) -> Result<RevalidatedActiveReblitBootPublicationTargets<'view>, ActiveReblitBootPublicationTargetsError> {
        let mut now = Instant::now;
        self.revalidate_publication_targets_with(&mut now, || {})
    }

    fn revalidate_publication_targets_with<'view>(
        &'view self,
        now: &mut impl FnMut() -> Instant,
        between_attachment_and_closing_topology: impl FnOnce(),
    ) -> Result<RevalidatedActiveReblitBootPublicationTargets<'view>, ActiveReblitBootPublicationTargetsError> {
        let deadline = self.deadline;
        require_deadline("entry", deadline, now)?;
        let opening = self
            .prepared
            .revalidate_until(self._installation, deadline)
            .map_err(|source| ActiveReblitBootPublicationTargetsError::OpeningTopology { source })?;
        require_deadline("after opening topology", deadline, now)?;

        let targets = capture_exact_targets(
            &self.prepared.targets,
            &self.prepared.anchor,
            opening.topology(),
            deadline,
        )?;
        require_deadline("after attachment capture", deadline, now)?;
        drop(opening);

        between_attachment_and_closing_topology();
        let closing = self
            .prepared
            .revalidate_until(self._installation, deadline)
            .map_err(|source| ActiveReblitBootPublicationTargetsError::ClosingTopology { source })?;
        targets.require_matches(closing.topology())?;
        require_deadline("terminal", deadline, now)?;
        Ok(targets)
    }

    #[cfg(test)]
    pub(in crate::client) fn revalidate_publication_targets_fixture_with<'view>(
        &'view self,
        now: &mut impl FnMut() -> Instant,
        between_attachment_and_closing_topology: impl FnOnce(),
    ) -> Result<RevalidatedActiveReblitBootPublicationTargets<'view>, ActiveReblitBootPublicationTargetsError> {
        self.revalidate_publication_targets_with(now, between_attachment_and_closing_topology)
    }
}

fn capture_exact_targets<'prepared>(
    prepared: &'prepared PreparedMountedBootTargets,
    anchor: &crate::linux_fs::mount_namespace::PreparedMountNamespaceAnchor,
    topology: BoundActiveReblitMountedBootTopology<'_>,
    deadline: Instant,
) -> Result<RevalidatedActiveReblitBootPublicationTargets<'prepared>, ActiveReblitBootPublicationTargetsError> {
    match (prepared, topology) {
        (
            PreparedMountedBootTargets::BootAliasesEsp { esp },
            BoundActiveReblitMountedBootTopology::BootAliasesEsp { esp: facts },
        ) => Ok(RevalidatedActiveReblitBootPublicationTargets::BootAliasesEsp {
            esp: capture_exact_target(esp, anchor, BootTargetRole::Esp, facts, deadline)?,
        }),
        (
            PreparedMountedBootTargets::DistinctXbootldr { esp, xbootldr },
            BoundActiveReblitMountedBootTopology::DistinctXbootldr {
                esp: esp_facts,
                xbootldr: xbootldr_facts,
            },
        ) => Ok(RevalidatedActiveReblitBootPublicationTargets::DistinctXbootldr {
            esp: capture_exact_target(esp, anchor, BootTargetRole::Esp, esp_facts, deadline)?,
            xbootldr: capture_exact_target(
                xbootldr,
                anchor,
                BootTargetRole::Xbootldr,
                xbootldr_facts,
                deadline,
            )?,
        }),
        _ => Err(ActiveReblitBootPublicationTargetsError::TopologyShapeChanged),
    }
}

fn capture_exact_target<'prepared>(
    prepared: &'prepared PreparedMountedBootTarget,
    anchor: &crate::linux_fs::mount_namespace::PreparedMountNamespaceAnchor,
    role: BootTargetRole,
    facts: BoundActiveReblitMountedBootTarget<'_>,
    deadline: Instant,
) -> Result<RevalidatedActiveReblitBootPublicationTarget<'prepared>, ActiveReblitBootPublicationTargetsError> {
    let attachment = prepared
        .attachment
        .revalidate_against_until(anchor, deadline)
        .map_err(|source| ActiveReblitBootPublicationTargetsError::Attachment { role, source })?;
    let found = observed_identity(&attachment);
    require_bound_identity(role, facts, found)?;
    Ok(RevalidatedActiveReblitBootPublicationTarget {
        role,
        attachment,
        destination: facts.destination,
        mount_id: facts.mount_id,
        deadline,
    })
}

#[derive(Clone, Copy)]
struct PublicationTargetIdentity {
    device: u64,
    inode: u64,
    mount_id: u64,
}

fn observed_identity(attachment: &RevalidatedTaskRootedAttachment<'_>) -> PublicationTargetIdentity {
    PublicationTargetIdentity {
        device: attachment.destination_device(),
        inode: attachment.destination_inode(),
        mount_id: attachment.destination_mount_id(),
    }
}

fn require_bound_identity(
    role: BootTargetRole,
    facts: BoundActiveReblitMountedBootTarget<'_>,
    found: PublicationTargetIdentity,
) -> Result<(), ActiveReblitBootPublicationTargetsError> {
    require_identity(
        role,
        PublicationTargetIdentity {
            device: facts.destination.raw_device(),
            inode: facts.destination.inode(),
            mount_id: facts.mount_id,
        },
        found,
    )
}

fn require_identity(
    role: BootTargetRole,
    expected: PublicationTargetIdentity,
    found: PublicationTargetIdentity,
) -> Result<(), ActiveReblitBootPublicationTargetsError> {
    if expected.device != found.device || expected.inode != found.inode || expected.mount_id != found.mount_id {
        return Err(ActiveReblitBootPublicationTargetsError::TargetIdentityMismatch {
            role,
            expected_device: expected.device,
            expected_inode: expected.inode,
            expected_mount_id: expected.mount_id,
            found_device: found.device,
            found_inode: found.inode,
            found_mount_id: found.mount_id,
        });
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::client) fn validate_fixture_publication_target_binding(
    role: BootTargetRole,
    expected: (u64, u64, u64),
    found: (u64, u64, u64),
) -> Result<(), ActiveReblitBootPublicationTargetsError> {
    require_identity(
        role,
        PublicationTargetIdentity {
            device: expected.0,
            inode: expected.1,
            mount_id: expected.2,
        },
        PublicationTargetIdentity {
            device: found.0,
            inode: found.1,
            mount_id: found.2,
        },
    )
}

fn require_deadline(
    checkpoint: &'static str,
    deadline: Instant,
    now: &mut impl FnMut() -> Instant,
) -> Result<(), ActiveReblitBootPublicationTargetsError> {
    if now() > deadline {
        Err(ActiveReblitBootPublicationTargetsError::DeadlineExceeded {
            checkpoint,
            deadline,
        })
    } else {
        Ok(())
    }
}
