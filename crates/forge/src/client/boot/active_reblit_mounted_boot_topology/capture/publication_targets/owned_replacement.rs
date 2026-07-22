//! Sealed replacement of one receipt-owned boot output.
//!
//! This bridge is the only client-side path from an aggregate effect seal and
//! plan-bound output to the descriptor-rooted replacement primitive. The
//! target attachment stays private. Callers supply no low-level mutation
//! request or private-name fingerprint: the exact predecessor and successor
//! identities are re-bound here, while the private rollback name is derived
//! from the pending receipt fingerprint.

use thiserror::Error;

use crate::{
    client::{
        active_reblit_bls_renderer::BoundActiveReblitBlsPublication,
        active_reblit_boot_publication_preflight::ActiveReblitBootPublicationEffectSeal,
        active_reblit_installed_boot_publication_delta::ActiveReblitBootPublicationDeltaExpected,
        active_reblit_publication_plan::ACTIVE_REBLIT_BOOT_OUTPUT_MODE,
    },
    linux_fs::{
        descriptor_boot_namespace::{
            BootNamespaceDestinationState, BootNamespaceRequest,
            RetainedBootNamespaceExpectedSource,
        },
        mount_namespace::{
            RetainedBootFileMutationFingerprint, RetainedBootFilePublicationLimits,
            RetainedBootFilePublicationRequest, RetainedBootFileReplacementError,
            RetainedBootFileReplacementRequest, RetainedBootLeafAssessmentError,
            RetainedBootLeafAssessmentLimits, RetainedBootLeafAssessmentRequest,
            RetainedBootLeafAssessmentState, RetainedBootPublicationParent,
            RetainedBootPublicationParentError, TaskRootBootNamespaceAssessmentError,
            ValidatedRetainedBootFileReplacement, ValidatedRetainedBootLeafAssessment,
        },
    },
};

use super::RevalidatedActiveReblitBootPublicationTarget;

#[cfg(test)]
#[path = "owned_replacement/fixture.rs"]
mod fixture;
#[path = "owned_replacement/validation.rs"]
mod validation;

#[cfg(test)]
pub(in crate::client) use fixture::{
    FixtureOwnedReplacementAssessmentGuard,
    arm_fixture_owned_replacement_assessments,
    fixture_owned_replacement_assessments_remaining,
    fixture_owned_replacement_validations_remaining,
};

/// Failure while replacing one exact receipt-owned output through an opaque
/// ESP/XBOOTLDR target.
///
/// Every variant contains inert diagnostics only. No target descriptor,
/// low-level mutation request, rollback name, or retry callback escapes.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootOwnedLeafReplacementError {
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
    #[error("bound publication {plan_index} differs from its classified desired identity")]
    DesiredIdentityMismatch { plan_index: usize },
    #[error("bound publication {plan_index} classified identical installed and desired bytes as a replacement")]
    IdenticalInstalledAndDesired { plan_index: usize },
    #[error("reassess the exact desired output immediately before owned replacement")]
    NamespaceReassessment(#[source] TaskRootBootNamespaceAssessmentError),
    #[error("the one-output desired reassessment returned {actual} states instead of one")]
    NamespaceAssessmentLength { actual: usize },
    #[error(
        "the desired reassessment changed target identity: expected st_dev {expected_device}, st_ino {expected_inode}, mount ID {expected_mount_id}, found st_dev {found_device}, st_ino {found_inode}, mount ID {found_mount_id}"
    )]
    NamespaceAssessmentIdentity {
        expected_device: u64,
        expected_inode: u64,
        expected_mount_id: u64,
        found_device: u64,
        found_inode: u64,
        found_mount_id: u64,
    },
    #[error("the owned replacement destination is {found:?}, expected Different")]
    DestinationNotDifferent { found: BootNamespaceDestinationState },
    #[error("authenticate the exact installed predecessor without mutating its parent chain")]
    InstalledAssessment(#[source] RetainedBootLeafAssessmentError),
    #[error("the installed predecessor assessment is {found:?}, expected Exact")]
    InstalledNotExact { found: RetainedBootLeafAssessmentState },
    #[error("the installed predecessor assessment is not bound to this exact target root")]
    InstalledAssessmentRootIdentity,
    #[error("the installed predecessor assessment is not bound to the exact plan path")]
    InstalledAssessmentPathIdentity,
    #[error("the installed predecessor assessment is not bound to the classified receipt bytes")]
    InstalledAssessmentContentIdentity,
    #[error("the installed predecessor assessment lacks one exact retained file identity")]
    InstalledAssessmentFileIdentity,
    #[error("retain the already-existing exact boot-publication parent chain")]
    PublicationParent(#[source] RetainedBootPublicationParentError),
    #[error("the retained replacement parent is not bound to this exact target root")]
    PublicationParentRootIdentity,
    #[error("the retained replacement parent changed since exact predecessor assessment")]
    PublicationParentIdentityChanged,
    #[error("replace the exact installed boot output with its plan-bound successor")]
    LeafReplacement(#[source] RetainedBootFileReplacementError),
    #[error("the replacement evidence is not bound to the requested canonical leaf")]
    ReplacementLeafIdentity,
    #[error("the replacement evidence contains an invalid or aliased inode pair")]
    ReplacementFileIdentity,
    #[error("the applied replacement evidence differs from bound publication {plan_index}")]
    ReplacementPlanIdentity { plan_index: usize },
    #[error("validate the exact applied owned replacement without mutating it")]
    LeafReplacementValidation(#[source] RetainedBootFileReplacementError),
}

impl RevalidatedActiveReblitBootPublicationTarget<'_> {
    /// Replace one exact installed predecessor without releasing attachment
    /// authority or accepting a caller-built low-level replacement request.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::client) fn replace_preflighted_owned_leaf<'plan, 'asset: 'plan>(
        &self,
        _effect_seal: &ActiveReblitBootPublicationEffectSeal,
        plan_index: usize,
        output: &BoundActiveReblitBlsPublication<'plan, 'asset>,
        namespace_request: BootNamespaceRequest<'_>,
        expected_source: &RetainedBootNamespaceExpectedSource<'_>,
        desired_expected: ActiveReblitBootPublicationDeltaExpected,
        installed_expected: ActiveReblitBootPublicationDeltaExpected,
    ) -> Result<ValidatedRetainedBootFileReplacement, ActiveReblitBootOwnedLeafReplacementError> {
        if output.mode() != ACTIVE_REBLIT_BOOT_OUTPUT_MODE {
            return Err(ActiveReblitBootOwnedLeafReplacementError::PublicationMode {
                plan_index,
                found: output.mode(),
            });
        }
        let relative_path = output.relative_path().to_str().ok_or(
            ActiveReblitBootOwnedLeafReplacementError::NonUtf8Path { plan_index },
        )?;
        let path = split_bound_replacement_path(relative_path, plan_index)?;
        let exact_namespace_request = BootNamespaceRequest::new(
            relative_path,
            output.expected_length(),
            output.expected_digest(),
        );
        if namespace_request != exact_namespace_request {
            return Err(
                ActiveReblitBootOwnedLeafReplacementError::NamespaceRequestMismatch {
                    plan_index,
                },
            );
        }
        if !expected_matches_output(desired_expected, output) {
            return Err(
                ActiveReblitBootOwnedLeafReplacementError::DesiredIdentityMismatch {
                    plan_index,
                },
            );
        }
        if desired_expected == installed_expected {
            return Err(
                ActiveReblitBootOwnedLeafReplacementError::IdenticalInstalledAndDesired {
                    plan_index,
                },
            );
        }

        let desired = publication_request(path.leaf, desired_expected);
        let installed = publication_request(path.leaf, installed_expected);
        #[cfg(test)]
        let fixture_assessment = fixture::take(
            self,
            namespace_request,
            expected_source,
        );
        #[cfg(test)]
        let desired_state = match &fixture_assessment {
            Some(assessment) => assessment.state(),
            None => reassess_desired_state(self, namespace_request, expected_source)?,
        };
        #[cfg(not(test))]
        let desired_state =
            reassess_desired_state(self, namespace_request, expected_source)?;
        if desired_state != BootNamespaceDestinationState::Different {
            return Err(
                ActiveReblitBootOwnedLeafReplacementError::DestinationNotDifferent {
                    found: desired_state,
                },
            );
        }

        #[cfg(test)]
        if let Some(fixture_assessment) = fixture_assessment {
            let evidence = fixture_assessment.replace(
                path.parents(),
                installed,
                desired,
                expected_source,
                mutation_owner(_effect_seal),
                self.deadline,
            )?;
            if evidence.canonical_leaf() != path.leaf {
                return Err(
                    ActiveReblitBootOwnedLeafReplacementError::ReplacementLeafIdentity,
                );
            }
            if evidence.installed_file_inode() == 0
                || evidence.replacement_file_inode() == 0
                || evidence.installed_file_inode()
                    == evidence.replacement_file_inode()
            {
                return Err(
                    ActiveReblitBootOwnedLeafReplacementError::ReplacementFileIdentity,
                );
            }
            return Ok(evidence);
        }

        let installed_assessment = self
            .attachment
            .assess_boot_leaf_below_parent_until(
                path.parents(),
                RetainedBootLeafAssessmentRequest::new(
                    path.leaf,
                    installed_expected.length(),
                    installed_expected.checksum(),
                    *installed_expected.content_identity().as_bytes(),
                ),
                RetainedBootLeafAssessmentLimits::default(),
                self.deadline,
            )
            .map_err(ActiveReblitBootOwnedLeafReplacementError::InstalledAssessment)?;
        require_installed_assessment(
            self,
            &path,
            installed_expected,
            &installed_assessment,
        )?;

        let parent = self
            .attachment
            .retain_existing_boot_publication_parent_until(path.parents(), self.deadline)
            .map_err(ActiveReblitBootOwnedLeafReplacementError::PublicationParent)?;
        require_parent_identity(self, &parent, &installed_assessment)?;

        let request = RetainedBootFileReplacementRequest::new(
            installed,
            desired,
            mutation_owner(_effect_seal),
        );
        let evidence = parent
            .replace_exact_boot_file_until(
                request,
                expected_source,
                RetainedBootFilePublicationLimits::default(),
                self.deadline,
            )
            .map_err(ActiveReblitBootOwnedLeafReplacementError::LeafReplacement)?;
        if evidence.canonical_leaf() != path.leaf {
            return Err(ActiveReblitBootOwnedLeafReplacementError::ReplacementLeafIdentity);
        }
        if evidence.installed_file_inode() == 0
            || evidence.replacement_file_inode() == 0
            || evidence.installed_file_inode() == evidence.replacement_file_inode()
        {
            return Err(ActiveReblitBootOwnedLeafReplacementError::ReplacementFileIdentity);
        }
        Ok(evidence)
    }
}

fn publication_request(
    leaf: &str,
    expected: ActiveReblitBootPublicationDeltaExpected,
) -> RetainedBootFilePublicationRequest<'_> {
    RetainedBootFilePublicationRequest::new(
        leaf,
        expected.length(),
        expected.checksum(),
        *expected.content_identity().as_bytes(),
    )
}

fn mutation_owner(
    effect_seal: &ActiveReblitBootPublicationEffectSeal,
) -> RetainedBootFileMutationFingerprint {
    RetainedBootFileMutationFingerprint::new(*effect_seal.pending_receipt().as_bytes())
}

fn expected_matches_output(
    expected: ActiveReblitBootPublicationDeltaExpected,
    output: &BoundActiveReblitBlsPublication<'_, '_>,
) -> bool {
    expected.length() == output.expected_length()
        && expected.checksum() == output.expected_digest()
        && expected.content_identity() == output.expected_content_identity()
}

fn reassess_desired_state(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    namespace_request: BootNamespaceRequest<'_>,
    expected_source: &RetainedBootNamespaceExpectedSource<'_>,
) -> Result<BootNamespaceDestinationState, ActiveReblitBootOwnedLeafReplacementError> {
    let assessment = target
        .assess_boot_namespace(
            std::slice::from_ref(&namespace_request),
            std::slice::from_ref(expected_source),
        )
        .map_err(ActiveReblitBootOwnedLeafReplacementError::NamespaceReassessment)?;
    let destination = target.destination();
    let expected = (
        destination.raw_device(),
        destination.inode(),
        target.mount_id(),
    );
    let found = (
        assessment.destination_device(),
        assessment.destination_inode(),
        assessment.destination_mount_id(),
    );
    if expected != found {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::NamespaceAssessmentIdentity {
                expected_device: expected.0,
                expected_inode: expected.1,
                expected_mount_id: expected.2,
                found_device: found.0,
                found_inode: found.1,
                found_mount_id: found.2,
            },
        );
    }
    let states = assessment.states();
    if states.len() != 1 {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::NamespaceAssessmentLength {
                actual: states.len(),
            },
        );
    }
    Ok(states[0])
}

fn require_installed_assessment(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    path: &BoundReplacementPath<'_>,
    installed: ActiveReblitBootPublicationDeltaExpected,
    assessment: &ValidatedRetainedBootLeafAssessment,
) -> Result<(), ActiveReblitBootOwnedLeafReplacementError> {
    if assessment.state() != RetainedBootLeafAssessmentState::Exact {
        return Err(ActiveReblitBootOwnedLeafReplacementError::InstalledNotExact {
            found: assessment.state(),
        });
    }
    let destination = target.destination();
    if assessment.assessment_root_device() != destination.raw_device()
        || assessment.assessment_root_inode() != destination.inode()
        || assessment.assessment_root_mount_id() != target.mount_id()
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::InstalledAssessmentRootIdentity,
        );
    }
    if assessment.canonical_leaf() != path.leaf
        || !assessment.parent_components().eq(path.parents().iter().copied())
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::InstalledAssessmentPathIdentity,
        );
    }
    if assessment.expected_length() != installed.length()
        || assessment.expected_xxh3() != installed.checksum()
        || assessment.expected_sha256() != *installed.content_identity().as_bytes()
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::InstalledAssessmentContentIdentity,
        );
    }
    let exact_file = (
        assessment.exact_file_device(),
        assessment.exact_file_inode(),
        assessment.exact_file_mount_id(),
    );
    if !matches!(exact_file, (Some(device), Some(inode), Some(mount_id))
        if device == destination.raw_device() && inode != 0 && mount_id == target.mount_id())
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::InstalledAssessmentFileIdentity,
        );
    }
    Ok(())
}

fn require_parent_identity(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    parent: &RetainedBootPublicationParent<'_, '_>,
    assessment: &ValidatedRetainedBootLeafAssessment,
) -> Result<(), ActiveReblitBootOwnedLeafReplacementError> {
    let destination = target.destination();
    if parent.root_device() != destination.raw_device()
        || parent.root_inode() != destination.inode()
        || parent.root_mount_id() != target.mount_id()
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::PublicationParentRootIdentity,
        );
    }
    if assessment.retained_parent_device() != Some(parent.destination_device())
        || assessment.retained_parent_inode() != Some(parent.destination_inode())
        || assessment.retained_parent_mount_id() != Some(parent.destination_mount_id())
    {
        return Err(
            ActiveReblitBootOwnedLeafReplacementError::PublicationParentIdentityChanged,
        );
    }
    Ok(())
}

struct BoundReplacementPath<'path> {
    parent_components: [&'path str; 15],
    parent_count: usize,
    leaf: &'path str,
}

impl<'path> BoundReplacementPath<'path> {
    fn parents(&self) -> &[&'path str] {
        &self.parent_components[..self.parent_count]
    }
}

fn split_bound_replacement_path(
    path: &str,
    plan_index: usize,
) -> Result<BoundReplacementPath<'_>, ActiveReblitBootOwnedLeafReplacementError> {
    let mut components = path.split('/');
    let mut prior = components.next().ok_or(
        ActiveReblitBootOwnedLeafReplacementError::InvalidPathComponent { plan_index },
    )?;
    require_bound_component(prior, plan_index)?;
    let mut parent_components = [""; 15];
    let mut parent_count = 0usize;
    for component in components {
        require_bound_component(component, plan_index)?;
        if parent_count == parent_components.len() {
            return Err(
                ActiveReblitBootOwnedLeafReplacementError::PublicationParentDepth {
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
            ActiveReblitBootOwnedLeafReplacementError::MissingPublicationParent {
                plan_index,
            },
        );
    }
    Ok(BoundReplacementPath {
        parent_components,
        parent_count,
        leaf: prior,
    })
}

fn require_bound_component(
    component: &str,
    plan_index: usize,
) -> Result<(), ActiveReblitBootOwnedLeafReplacementError> {
    if component.is_empty()
        || matches!(component, "." | "..")
        || component.len() > 255
        || component.as_bytes().contains(&0)
    {
        Err(
            ActiveReblitBootOwnedLeafReplacementError::InvalidPathComponent {
                plan_index,
            },
        )
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "owned_replacement/tests.rs"]
mod tests;
