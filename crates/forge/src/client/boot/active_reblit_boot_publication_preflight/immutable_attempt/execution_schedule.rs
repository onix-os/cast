//! Validation and routing for one sealed, plan-ordered effect schedule.

use std::path::Path;

use crate::{
    client::{
        active_reblit_boot_namespace_inputs::{
            BoundActiveReblitBootNamespaceDomain, BoundActiveReblitBootNamespaceInputs,
        },
        active_reblit_installed_boot_publication_delta::{
            ActiveReblitBootPublicationDeltaAction,
            ActiveReblitBootPublicationEffectSchedule,
        },
        active_reblit_mounted_boot_topology::{
            BootTargetRole, RevalidatedActiveReblitBootPublicationTarget,
            RevalidatedActiveReblitBootPublicationTargets,
        },
        active_reblit_publication_plan::{
            ACTIVE_REBLIT_BOOT_OUTPUT_MODE, ActiveReblitBootDestinationLayout,
            ActiveReblitBootDestinationRoot, ActiveReblitBootPublicationPhase,
            ActiveReblitBootPublicationRole,
        },
    },
    linux_fs::descriptor_boot_namespace::{
        BootNamespaceDestinationState, BootNamespaceRequest,
        RetainedBootNamespaceExpectedSource,
    },
};

use super::{
    ActiveReblitBootImmutablePublicationAttemptError,
    RevalidatedActiveReblitBootPublicationPreflight,
    ValidatedActiveReblitBootPublicationEffect,
};
use super::super::{assess_bound_namespaces_with, assess_one_bound_namespace};

pub(super) fn prepare_execution_schedule(
    preflight: &RevalidatedActiveReblitBootPublicationPreflight<'_, '_, '_, '_, '_, '_, '_>,
    schedule: &ActiveReblitBootPublicationEffectSchedule,
) -> Result<Vec<ValidatedActiveReblitBootPublicationEffect>, ActiveReblitBootImmutablePublicationAttemptError> {
    let mut evidence = Vec::new();
    evidence
        .try_reserve_exact(preflight.publication_count())
        .map_err(ActiveReblitBootImmutablePublicationAttemptError::EvidenceAllocation)?;
    if schedule.entries().len() != preflight.publication_count() {
        return Err(
            ActiveReblitBootImmutablePublicationAttemptError::PublicationCountMismatch {
                expected: preflight.publication_count(),
                actual: schedule.entries().len(),
            },
        );
    }
    let mut previous_phase = None;
    for (plan_index, (scheduled, output)) in schedule
        .entries()
        .iter()
        .zip(preflight.plan.outputs())
        .enumerate()
    {
        if scheduled.plan_index() != plan_index || scheduled.root() != output.root() {
            return Err(
                ActiveReblitBootImmutablePublicationAttemptError::InvalidPreflightState {
                    plan_index,
                },
            );
        }
        let expected_phase = phase_for_role(output.role());
        if output.phase() != expected_phase {
            return Err(ActiveReblitBootImmutablePublicationAttemptError::RolePhaseMismatch {
                plan_index,
                role: output.role(),
                expected: expected_phase,
                found: output.phase(),
            });
        }
        if let Some(previous) = previous_phase
            && output.phase() < previous
        {
            return Err(ActiveReblitBootImmutablePublicationAttemptError::GlobalPhaseOrder {
                plan_index,
                previous,
                found: output.phase(),
            });
        }
        previous_phase = Some(output.phase());
        if output.mode() != ACTIVE_REBLIT_BOOT_OUTPUT_MODE {
            return Err(ActiveReblitBootImmutablePublicationAttemptError::PublicationMode {
                plan_index,
                expected: ACTIVE_REBLIT_BOOT_OUTPUT_MODE,
                found: output.mode(),
            });
        }
        split_publication_path(output.relative_path(), plan_index)?;
        route_publication(
            &preflight.targets,
            &preflight.namespace_inputs,
            preflight.plan.destination_layout(),
            output.root(),
            plan_index,
        )?;
        if initial_state_for_action(scheduled.action())
            != Some(preflight.initial_states[plan_index])
        {
            return Err(
                ActiveReblitBootImmutablePublicationAttemptError::InvalidPreflightState {
                    plan_index,
                },
            );
        }
    }
    Ok(evidence)
}

pub(super) const fn initial_state_for_action(
    action: ActiveReblitBootPublicationDeltaAction,
) -> Option<BootNamespaceDestinationState> {
    match action {
        ActiveReblitBootPublicationDeltaAction::PublishDesired => {
            Some(BootNamespaceDestinationState::Absent)
        }
        ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired
        | ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired => {
            Some(BootNamespaceDestinationState::Exact)
        }
        ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired => {
            Some(BootNamespaceDestinationState::Different)
        }
        ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion
        | ActiveReblitBootPublicationDeltaAction::PreserveUnownedStale => None,
    }
}

pub(super) fn terminal_namespace_assessment(
    preflight: &RevalidatedActiveReblitBootPublicationPreflight<'_, '_, '_, '_, '_, '_, '_>,
) -> Result<Box<[BootNamespaceDestinationState]>, ActiveReblitBootImmutablePublicationAttemptError> {
    let mut assess = assess_one_bound_namespace;
    assess_bound_namespaces_with(
        &preflight.targets,
        &preflight.namespace_inputs,
        preflight.plan.publication_count(),
        &mut assess,
    )
    .map_err(ActiveReblitBootImmutablePublicationAttemptError::TerminalNamespaceAssessment)
}

pub(super) struct RoutedPublication<'view, 'source> {
    pub(super) role: BootTargetRole,
    pub(super) target: &'view RevalidatedActiveReblitBootPublicationTarget<'source>,
    pub(super) namespace_request: BootNamespaceRequest<'source>,
    pub(super) expected_source: &'view RetainedBootNamespaceExpectedSource<'source>,
}

pub(super) fn route_publication<'view, 'source>(
    targets: &'view RevalidatedActiveReblitBootPublicationTargets<'source>,
    inputs: &'view BoundActiveReblitBootNamespaceInputs<'source>,
    layout: ActiveReblitBootDestinationLayout,
    root: ActiveReblitBootDestinationRoot,
    plan_index: usize,
) -> Result<RoutedPublication<'view, 'source>, ActiveReblitBootImmutablePublicationAttemptError> {
    let role = destination_role(layout, root);
    match (targets, inputs, layout, role) {
        (
            RevalidatedActiveReblitBootPublicationTargets::BootAliasesEsp { esp },
            BoundActiveReblitBootNamespaceInputs::BootAliasesEsp { shared },
            ActiveReblitBootDestinationLayout::BootAliasesEsp,
            BootTargetRole::Esp,
        ) => domain_publication(BootTargetRole::Esp, esp, shared, plan_index),
        (
            RevalidatedActiveReblitBootPublicationTargets::DistinctXbootldr { esp, .. },
            BoundActiveReblitBootNamespaceInputs::DistinctXbootldr {
                esp: esp_inputs,
                ..
            },
            ActiveReblitBootDestinationLayout::DistinctXbootldr,
            BootTargetRole::Esp,
        ) => domain_publication(BootTargetRole::Esp, esp, esp_inputs, plan_index),
        (
            RevalidatedActiveReblitBootPublicationTargets::DistinctXbootldr {
                xbootldr,
                ..
            },
            BoundActiveReblitBootNamespaceInputs::DistinctXbootldr {
                xbootldr: xbootldr_inputs,
                ..
            },
            ActiveReblitBootDestinationLayout::DistinctXbootldr,
            BootTargetRole::Xbootldr,
        ) => domain_publication(
            BootTargetRole::Xbootldr,
            xbootldr,
            xbootldr_inputs,
            plan_index,
        ),
        _ => Err(ActiveReblitBootImmutablePublicationAttemptError::DestinationLayoutMismatch),
    }
}

fn domain_publication<'view, 'source>(
    role: BootTargetRole,
    target: &'view RevalidatedActiveReblitBootPublicationTarget<'source>,
    domain: &'view BoundActiveReblitBootNamespaceDomain<'source>,
    plan_index: usize,
) -> Result<RoutedPublication<'view, 'source>, ActiveReblitBootImmutablePublicationAttemptError> {
    if target.role() != role {
        return Err(ActiveReblitBootImmutablePublicationAttemptError::DestinationRoleMismatch {
            plan_index,
            expected: role,
            found: target.role(),
        });
    }
    let position = domain_plan_position(role, domain.plan_indices(), plan_index)?;
    let namespace_request = domain.requests().get(position).copied().ok_or(
        ActiveReblitBootImmutablePublicationAttemptError::DomainPlanIndexMissing {
            role,
            plan_index,
        },
    )?;
    let expected_source = domain.expected_sources().get(position).ok_or(
        ActiveReblitBootImmutablePublicationAttemptError::DomainPlanIndexMissing {
            role,
            plan_index,
        },
    )?;
    Ok(RoutedPublication {
        role,
        target,
        namespace_request,
        expected_source,
    })
}

pub(super) const fn destination_role(
    layout: ActiveReblitBootDestinationLayout,
    root: ActiveReblitBootDestinationRoot,
) -> BootTargetRole {
    match (layout, root) {
        (ActiveReblitBootDestinationLayout::BootAliasesEsp, _)
        | (
            ActiveReblitBootDestinationLayout::DistinctXbootldr,
            ActiveReblitBootDestinationRoot::Esp,
        ) => BootTargetRole::Esp,
        (
            ActiveReblitBootDestinationLayout::DistinctXbootldr,
            ActiveReblitBootDestinationRoot::Boot,
        ) => BootTargetRole::Xbootldr,
    }
}

pub(super) fn domain_plan_position(
    role: BootTargetRole,
    plan_indices: &[usize],
    plan_index: usize,
) -> Result<usize, ActiveReblitBootImmutablePublicationAttemptError> {
    plan_indices.binary_search(&plan_index).map_err(|_| {
        ActiveReblitBootImmutablePublicationAttemptError::DomainPlanIndexMissing {
            role,
            plan_index,
        }
    })
}

fn split_publication_path(
    path: &Path,
    plan_index: usize,
) -> Result<(), ActiveReblitBootImmutablePublicationAttemptError> {
    let path = path
        .to_str()
        .ok_or(ActiveReblitBootImmutablePublicationAttemptError::NonUtf8Path { plan_index })?;
    let mut components = path.split('/');
    let mut prior = components.next().ok_or(
        ActiveReblitBootImmutablePublicationAttemptError::InvalidPathComponent { plan_index },
    )?;
    require_component(prior, plan_index)?;
    let mut parent_count = 0usize;
    for component in components {
        require_component(component, plan_index)?;
        if parent_count == 15 {
            return Err(
                ActiveReblitBootImmutablePublicationAttemptError::PublicationParentDepth {
                    plan_index,
                },
            );
        }
        parent_count += 1;
        prior = component;
    }
    if parent_count == 0 || prior.is_empty() {
        return Err(
            ActiveReblitBootImmutablePublicationAttemptError::MissingPublicationParent {
                plan_index,
            },
        );
    }
    Ok(())
}

fn require_component(
    component: &str,
    plan_index: usize,
) -> Result<(), ActiveReblitBootImmutablePublicationAttemptError> {
    if component.is_empty()
        || matches!(component, "." | "..")
        || component.len() > 255
        || component.as_bytes().contains(&0)
    {
        Err(ActiveReblitBootImmutablePublicationAttemptError::InvalidPathComponent {
            plan_index,
        })
    } else {
        Ok(())
    }
}

const fn phase_for_role(role: ActiveReblitBootPublicationRole) -> ActiveReblitBootPublicationPhase {
    match role {
        ActiveReblitBootPublicationRole::Payload => ActiveReblitBootPublicationPhase::Payload,
        ActiveReblitBootPublicationRole::Entry => ActiveReblitBootPublicationPhase::Entry,
        ActiveReblitBootPublicationRole::LoaderControl => {
            ActiveReblitBootPublicationPhase::LoaderControl
        }
        ActiveReblitBootPublicationRole::FallbackBootloader
        | ActiveReblitBootPublicationRole::SystemdBootloader => {
            ActiveReblitBootPublicationPhase::Bootloader
        }
    }
}
