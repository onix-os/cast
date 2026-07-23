//! Unforgeable binding between one retained preflight and its desired states.

use std::path::Path;

use crate::linux_fs::descriptor_boot_namespace::BootNamespaceDestinationState;

use super::{
    ActiveReblitBootPublicationPreflightError,
    BoundActiveReblitBlsPublicationPlan,
};
use crate::client::{
    active_reblit_publication_plan::{
        ActiveReblitBootDestinationLayout,
        ActiveReblitBootDestinationRoot,
    },
    boot_content_identity::BootContentIdentity,
};

/// Private evidence that scalar states came from the retained, twice-bracketed
/// preflight for one exact desired plan.
///
/// The type is visible only so the installed-delta module can consume it. Its
/// fields and constructor remain inside the preflight module, so a client
/// sibling cannot turn caller-authored scalar observations into a classified
/// delta.
pub(in crate::client) struct ActiveReblitBootPublicationAssessmentSeal<'plan> {
    destination_layout: ActiveReblitBootDestinationLayout,
    desired_states: Box<[SealedActiveReblitBootPublicationDesiredState<'plan>]>,
}

pub(in crate::client) struct SealedActiveReblitBootPublicationDesiredState<'plan> {
    root: ActiveReblitBootDestinationRoot,
    relative_path: &'plan Path,
    checksum: u128,
    length: u64,
    content_identity: BootContentIdentity,
    state: BootNamespaceDestinationState,
}

impl ActiveReblitBootPublicationAssessmentSeal<'_> {
    pub(in crate::client) const fn destination_layout(
        &self,
    ) -> ActiveReblitBootDestinationLayout {
        self.destination_layout
    }

    pub(in crate::client) fn desired_states(
        &self,
    ) -> &[SealedActiveReblitBootPublicationDesiredState<'_>] {
        &self.desired_states
    }
}

impl SealedActiveReblitBootPublicationDesiredState<'_> {
    pub(in crate::client) const fn root(&self) -> ActiveReblitBootDestinationRoot {
        self.root
    }

    pub(in crate::client) fn relative_path(&self) -> &Path {
        self.relative_path
    }

    pub(in crate::client) const fn checksum(&self) -> u128 {
        self.checksum
    }

    pub(in crate::client) const fn length(&self) -> u64 {
        self.length
    }

    pub(in crate::client) const fn content_identity(&self) -> BootContentIdentity {
        self.content_identity
    }

    pub(in crate::client) const fn state(&self) -> BootNamespaceDestinationState {
        self.state
    }
}

pub(super) fn seal_bound_desired_states<
    'plan,
    'input: 'plan,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    plan: &'plan BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    states: &[BootNamespaceDestinationState],
) -> Result<ActiveReblitBootPublicationAssessmentSeal<'plan>, ActiveReblitBootPublicationPreflightError>
{
    if plan.publication_count() != states.len() {
        return Err(
            ActiveReblitBootPublicationPreflightError::PublicationCountMismatch {
                expected: plan.publication_count(),
                actual: states.len(),
            },
        );
    }

    let mut desired_states = Vec::new();
    desired_states.try_reserve_exact(states.len()).map_err(|source| {
        ActiveReblitBootPublicationPreflightError::StateAllocation { source }
    })?;
    for (output, state) in plan.outputs().zip(states.iter().copied()) {
        desired_states.push(SealedActiveReblitBootPublicationDesiredState {
            root: output.root(),
            relative_path: output.relative_path(),
            checksum: output.expected_digest(),
            length: output.expected_length(),
            content_identity: output.expected_content_identity(),
            state,
        });
    }
    Ok(ActiveReblitBootPublicationAssessmentSeal {
        destination_layout: plan.destination_layout(),
        desired_states: desired_states.into_boxed_slice(),
    })
}
