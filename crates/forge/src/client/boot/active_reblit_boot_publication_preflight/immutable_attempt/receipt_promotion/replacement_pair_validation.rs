//! Borrowed descriptor validation of applied replacement pairs before promotion.

use crate::client::{
    active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
    active_reblit_mounted_boot_topology::{
        BootTargetRole, RevalidatedActiveReblitBootPublicationTarget,
        RevalidatedActiveReblitBootPublicationTargets,
    },
    active_reblit_publication_plan::{
        ActiveReblitBootDestinationLayout, ActiveReblitBootDestinationRoot,
    },
};

use super::super::ValidatedActiveReblitBootPublicationEffect;
use super::terminal_evidence::ActiveReblitBootTerminalEvidenceValidationError;

pub(super) fn validate_applied_replacement_pairs<
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    plan: &BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    evidence: &[ValidatedActiveReblitBootPublicationEffect],
    checkpoint: &'static str,
) -> Result<(), ActiveReblitBootTerminalEvidenceValidationError> {
    if !evidence
        .iter()
        .any(|retained| retained.replacement_authority().is_some())
    {
        return Ok(());
    }
    let targets = plan.revalidate_publication_targets().map_err(|source| {
        ActiveReblitBootTerminalEvidenceValidationError::ReplacementTargets {
            checkpoint,
            source,
        }
    })?;
    for (plan_index, (retained, output)) in
        evidence.iter().zip(plan.outputs()).enumerate()
    {
        let Some(authority) = retained.replacement_authority() else {
            continue;
        };
        let (role, target) = replacement_target(
            &targets,
            plan.destination_layout(),
            output.root(),
        )
        .ok_or(
            ActiveReblitBootTerminalEvidenceValidationError::ReplacementTargetShape {
                checkpoint,
                plan_index,
            },
        )?;
        target
            .validate_applied_owned_leaf_replacement(
                plan_index,
                &output,
                authority,
            )
            .map_err(|source| {
                ActiveReblitBootTerminalEvidenceValidationError::ReplacementPair {
                    checkpoint,
                    role,
                    plan_index,
                    source,
                }
            })?;
    }
    Ok(())
}

fn replacement_target<'view, 'target>(
    targets: &'view RevalidatedActiveReblitBootPublicationTargets<'target>,
    layout: ActiveReblitBootDestinationLayout,
    root: ActiveReblitBootDestinationRoot,
) -> Option<(
    BootTargetRole,
    &'view RevalidatedActiveReblitBootPublicationTarget<'target>,
)> {
    match (targets, layout, root) {
        (
            RevalidatedActiveReblitBootPublicationTargets::BootAliasesEsp { esp },
            ActiveReblitBootDestinationLayout::BootAliasesEsp,
            _,
        ) => Some((BootTargetRole::Esp, esp)),
        (
            RevalidatedActiveReblitBootPublicationTargets::DistinctXbootldr { esp, .. },
            ActiveReblitBootDestinationLayout::DistinctXbootldr,
            ActiveReblitBootDestinationRoot::Esp,
        ) => Some((BootTargetRole::Esp, esp)),
        (
            RevalidatedActiveReblitBootPublicationTargets::DistinctXbootldr {
                xbootldr,
                ..
            },
            ActiveReblitBootDestinationLayout::DistinctXbootldr,
            ActiveReblitBootDestinationRoot::Boot,
        ) => Some((BootTargetRole::Xbootldr, xbootldr)),
        _ => None,
    }
}
