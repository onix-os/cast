//! Sealed immutable-leaf effects through one opaque boot target.
//!
//! The target attachment stays private to its owning module. This child first
//! rechecks the exact one-output namespace state retained by aggregate
//! preflight, then creates or retains the admitted parent chain and delegates
//! to the existing immutable leaf publisher under the original deadline.

use thiserror::Error;

use crate::client::{
    active_reblit_bls_renderer::BoundActiveReblitBlsPublication,
    active_reblit_boot_publication_preflight::ActiveReblitBootPublicationEffectSeal,
    active_reblit_publication_plan::ACTIVE_REBLIT_BOOT_OUTPUT_MODE,
};
use crate::linux_fs::{
    descriptor_boot_namespace::{
        BootNamespaceDestinationState, BootNamespaceRequest,
        RetainedBootNamespaceExpectedSource,
    },
    mount_namespace::{
        RetainedBootFilePublicationError, RetainedBootFilePublicationLimits,
        RetainedBootFilePublicationOutcome, RetainedBootFilePublicationRequest,
        RetainedBootPublicationParentError, TaskRootBootNamespaceAssessmentError,
        ValidatedRetainedBootFilePublication,
    },
};

use super::RevalidatedActiveReblitBootPublicationTarget;

#[cfg(test)]
#[path = "immutable_leaf/fixture.rs"]
mod fixture;

#[cfg(test)]
pub(in crate::client) use fixture::{
    FixtureImmutableLeafAssessmentGuard,
    arm_fixture_immutable_leaf_assessments,
    fixture_immutable_leaf_assessments_remaining,
};

/// Failure while publishing one plan-bound immutable leaf through an opaque
/// ESP/XBOOTLDR target.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootImmutableLeafPublicationError {
    #[error("the aggregate preflight supplied a non-admissible initial destination state")]
    InvalidInitialState,
    #[error("bound publication {plan_index} has unsupported mode {found:o}")]
    PublicationMode { plan_index: usize, found: u32 },
    #[error("bound publication {plan_index} path is not UTF-8")]
    NonUtf8Path { plan_index: usize },
    #[error("bound publication {plan_index} path has no retained parent chain")]
    MissingPublicationParent { plan_index: usize },
    #[error("bound publication {plan_index} path contains an invalid component")]
    InvalidPathComponent { plan_index: usize },
    #[error("bound publication {plan_index} path exceeds the 15-component parent ceiling")]
    PublicationParentDepth { plan_index: usize },
    #[error("bound publication {plan_index} differs from its retained namespace request")]
    NamespaceRequestMismatch { plan_index: usize },
    #[error("reassess the exact output immediately before its first namespace effect")]
    NamespaceReassessment(#[source] TaskRootBootNamespaceAssessmentError),
    #[error("the one-output namespace reassessment returned {actual} states instead of one")]
    NamespaceAssessmentLength { actual: usize },
    #[error(
        "the one-output namespace reassessment changed target identity: expected st_dev {expected_device}, st_ino {expected_inode}, mount ID {expected_mount_id}, found st_dev {found_device}, st_ino {found_inode}, mount ID {found_mount_id}"
    )]
    NamespaceAssessmentIdentity {
        expected_device: u64,
        expected_inode: u64,
        expected_mount_id: u64,
        found_device: u64,
        found_inode: u64,
        found_mount_id: u64,
    },
    #[error("the destination state changed after aggregate preflight: expected {expected:?}, found {found:?}")]
    DestinationStateChanged {
        expected: BootNamespaceDestinationState,
        found: BootNamespaceDestinationState,
    },
    #[error("retain or create the exact admitted boot-publication parent chain")]
    PublicationParent(#[source] RetainedBootPublicationParentError),
    #[error(
        "the retained publication parent belongs to a different boot root: expected st_dev {expected_device}, st_ino {expected_inode}, mount ID {expected_mount_id}, found st_dev {found_device}, st_ino {found_inode}, mount ID {found_mount_id}"
    )]
    PublicationParentRootIdentity {
        expected_device: u64,
        expected_inode: u64,
        expected_mount_id: u64,
        found_device: u64,
        found_inode: u64,
        found_mount_id: u64,
    },
    #[error("publish or revalidate the immutable boot leaf")]
    LeafPublication(#[source] RetainedBootFilePublicationError),
    #[error("the immutable leaf evidence is not bound to its retained publication parent")]
    LeafParentIdentity,
    #[error("the immutable leaf evidence differs from the exact requested content")]
    LeafContentIdentity,
    #[error("the immutable leaf outcome does not match the preflight state: expected {expected:?}, found {found:?}")]
    LeafOutcome {
        expected: RetainedBootFilePublicationOutcome,
        found: RetainedBootFilePublicationOutcome,
    },
}

impl RevalidatedActiveReblitBootPublicationTarget<'_> {
    /// Publish one exact plan-bound output without releasing attachment
    /// authority, accepting a replacement path, or minting a fresh deadline.
    pub(in crate::client) fn publish_preflighted_immutable_leaf<'plan, 'asset: 'plan>(
        &self,
        _effect_seal: &ActiveReblitBootPublicationEffectSeal,
        plan_index: usize,
        output: &BoundActiveReblitBlsPublication<'plan, 'asset>,
        namespace_request: BootNamespaceRequest<'_>,
        expected_source: &RetainedBootNamespaceExpectedSource<'_>,
        initial_state: BootNamespaceDestinationState,
    ) -> Result<ValidatedRetainedBootFilePublication, ActiveReblitBootImmutableLeafPublicationError> {
        if output.mode() != ACTIVE_REBLIT_BOOT_OUTPUT_MODE {
            return Err(ActiveReblitBootImmutableLeafPublicationError::PublicationMode {
                plan_index,
                found: output.mode(),
            });
        }
        let relative_path = output
            .relative_path()
            .to_str()
            .ok_or(ActiveReblitBootImmutableLeafPublicationError::NonUtf8Path {
                plan_index,
            })?;
        let path = split_bound_publication_path(relative_path, plan_index)?;
        let exact_namespace_request = BootNamespaceRequest::new(
            relative_path,
            output.expected_length(),
            output.expected_digest(),
        );
        if namespace_request != exact_namespace_request {
            return Err(
                ActiveReblitBootImmutableLeafPublicationError::NamespaceRequestMismatch {
                    plan_index,
                },
            );
        }
        let leaf_request = RetainedBootFilePublicationRequest::new(
            path.leaf,
            output.expected_length(),
            output.expected_digest(),
            *output.expected_content_identity().as_bytes(),
        );
        let expected_outcome = match initial_state {
            BootNamespaceDestinationState::Absent => RetainedBootFilePublicationOutcome::Published,
            BootNamespaceDestinationState::Exact => RetainedBootFilePublicationOutcome::AlreadyExact,
            BootNamespaceDestinationState::Different => {
                return Err(ActiveReblitBootImmutableLeafPublicationError::InvalidInitialState);
            }
        };
        #[cfg(test)]
        if let Some(fixture_assessment) = fixture::take(self, namespace_request, expected_source) {
            let reassessed_state = fixture_assessment.state();
            if reassessed_state != initial_state {
                return Err(ActiveReblitBootImmutableLeafPublicationError::DestinationStateChanged {
                    expected: initial_state,
                    found: reassessed_state,
                });
            }
            return fixture_assessment.publish(
                path.parents(),
                leaf_request,
                expected_source,
                expected_outcome,
                self.deadline,
            );
        }
        let reassessed_state = reassess_destination_state(self, namespace_request, expected_source)?;
        if reassessed_state != initial_state {
            return Err(ActiveReblitBootImmutableLeafPublicationError::DestinationStateChanged {
                expected: initial_state,
                found: reassessed_state,
            });
        }

        let parent = self
            .attachment
            .retain_boot_publication_parent_until(path.parents(), self.deadline)
            .map_err(ActiveReblitBootImmutableLeafPublicationError::PublicationParent)?;
        require_parent_root_identity(self, &parent)?;
        let evidence = parent
            .publish_immutable_boot_file_until(
                leaf_request,
                expected_source,
                RetainedBootFilePublicationLimits::default(),
                self.deadline,
            )
            .map_err(ActiveReblitBootImmutableLeafPublicationError::LeafPublication)?;
        if !parent.matches_leaf_evidence(&evidence) {
            return Err(ActiveReblitBootImmutableLeafPublicationError::LeafParentIdentity);
        }
        if evidence.length() != leaf_request.expected_length()
            || evidence.xxh3() != leaf_request.expected_xxh3()
            || evidence.sha256() != leaf_request.expected_sha256()
        {
            return Err(ActiveReblitBootImmutableLeafPublicationError::LeafContentIdentity);
        }
        if evidence.outcome() != expected_outcome {
            return Err(ActiveReblitBootImmutableLeafPublicationError::LeafOutcome {
                expected: expected_outcome,
                found: evidence.outcome(),
            });
        }
        Ok(evidence)
    }
}

fn reassess_destination_state(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    namespace_request: BootNamespaceRequest<'_>,
    expected_source: &RetainedBootNamespaceExpectedSource<'_>,
) -> Result<BootNamespaceDestinationState, ActiveReblitBootImmutableLeafPublicationError> {
    let assessment = target
        .assess_boot_namespace(
            std::slice::from_ref(&namespace_request),
            std::slice::from_ref(expected_source),
        )
        .map_err(ActiveReblitBootImmutableLeafPublicationError::NamespaceReassessment)?;
    require_assessment_identity(target, &assessment)?;
    let states = assessment.states();
    if states.len() != 1 {
        return Err(
            ActiveReblitBootImmutableLeafPublicationError::NamespaceAssessmentLength {
                actual: states.len(),
            },
        );
    }
    Ok(states[0])
}

struct BoundPublicationPath<'path> {
    parent_components: [&'path str; 15],
    parent_count: usize,
    leaf: &'path str,
}

impl<'path> BoundPublicationPath<'path> {
    fn parents(&self) -> &[&'path str] {
        &self.parent_components[..self.parent_count]
    }
}

fn split_bound_publication_path(
    path: &str,
    plan_index: usize,
) -> Result<BoundPublicationPath<'_>, ActiveReblitBootImmutableLeafPublicationError> {
    let mut components = path.split('/');
    let mut prior = components.next().ok_or(
        ActiveReblitBootImmutableLeafPublicationError::InvalidPathComponent { plan_index },
    )?;
    require_bound_component(prior, plan_index)?;
    let mut parent_components = [""; 15];
    let mut parent_count = 0usize;
    for component in components {
        require_bound_component(component, plan_index)?;
        if parent_count == parent_components.len() {
            return Err(
                ActiveReblitBootImmutableLeafPublicationError::PublicationParentDepth {
                    plan_index,
                },
            );
        }
        parent_components[parent_count] = prior;
        parent_count += 1;
        prior = component;
    }
    if parent_count == 0 {
        return Err(
            ActiveReblitBootImmutableLeafPublicationError::MissingPublicationParent {
                plan_index,
            },
        );
    }
    Ok(BoundPublicationPath {
        parent_components,
        parent_count,
        leaf: prior,
    })
}

fn require_bound_component(
    component: &str,
    plan_index: usize,
) -> Result<(), ActiveReblitBootImmutableLeafPublicationError> {
    if component.is_empty()
        || matches!(component, "." | "..")
        || component.len() > 255
        || component.as_bytes().contains(&0)
    {
        Err(ActiveReblitBootImmutableLeafPublicationError::InvalidPathComponent {
            plan_index,
        })
    } else {
        Ok(())
    }
}

fn require_assessment_identity(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    assessment: &crate::linux_fs::mount_namespace::ValidatedTaskRootBootNamespaceAssessment,
) -> Result<(), ActiveReblitBootImmutableLeafPublicationError> {
    let destination = target.destination();
    let expected = (destination.raw_device(), destination.inode(), target.mount_id());
    let found = (
        assessment.destination_device(),
        assessment.destination_inode(),
        assessment.destination_mount_id(),
    );
    if expected != found {
        return Err(ActiveReblitBootImmutableLeafPublicationError::NamespaceAssessmentIdentity {
            expected_device: expected.0,
            expected_inode: expected.1,
            expected_mount_id: expected.2,
            found_device: found.0,
            found_inode: found.1,
            found_mount_id: found.2,
        });
    }
    Ok(())
}

fn require_parent_root_identity(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    parent: &crate::linux_fs::mount_namespace::RetainedBootPublicationParent<'_, '_>,
) -> Result<(), ActiveReblitBootImmutableLeafPublicationError> {
    let destination = target.destination();
    let expected = (destination.raw_device(), destination.inode(), target.mount_id());
    let found = (parent.root_device(), parent.root_inode(), parent.root_mount_id());
    if expected != found {
        return Err(ActiveReblitBootImmutableLeafPublicationError::PublicationParentRootIdentity {
            expected_device: expected.0,
            expected_inode: expected.1,
            expected_mount_id: expected.2,
            found_device: found.0,
            found_inode: found.1,
            found_mount_id: found.2,
        });
    }
    Ok(())
}
