//! Receipt-bound one-shot immutable publication through aggregate preflight.
//!
//! This child owns access to the preflight's private plan, namespace bindings,
//! and opaque targets. It validates the complete global schedule before any
//! effect, revalidates the exact staged `BootSyncStarted` evidence immediately
//! before the first namespace mutation, and consumes that staging authority on
//! every result. Success means every requested leaf was terminally observed
//! exact while the original topology and durable staged evidence still held.
//! This publication step itself does not promote the receipt or advance the
//! journal; its terminal token may be consumed by the separate promotion
//! bridge below.

use std::{collections::TryReserveError, path::Path, time::Instant};

use thiserror::Error;

use crate::{
    boot_publication::BootPublicationReceiptFingerprint,
    client::{
        Client,
        active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_boot_namespace_inputs::{
            BoundActiveReblitBootNamespaceDomain,
            BoundActiveReblitBootNamespaceInputs,
        },
        active_reblit_boot_sync_staging::{
            ActiveReblitBootSyncFreshValidationError,
            StagedActiveReblitBootSync,
        },
        active_reblit_mounted_boot_topology::{
            ActiveReblitBootImmutableLeafPublicationError,
            ActiveReblitBootPublicationTargetsError, BootTargetRole,
            RevalidatedActiveReblitBootPublicationTarget,
            RevalidatedActiveReblitBootPublicationTargets,
        },
        active_reblit_publication_plan::{
            ACTIVE_REBLIT_BOOT_OUTPUT_MODE, ActiveReblitBootDestinationLayout,
            ActiveReblitBootDestinationRoot, ActiveReblitBootPublicationPhase,
            ActiveReblitBootPublicationRole,
        },
    },
    linux_fs::{
        descriptor_boot_namespace::{
            BootNamespaceDestinationState, BootNamespaceRequest,
            RetainedBootNamespaceExpectedSource,
        },
        mount_namespace::{
            RetainedBootFilePublicationOutcome,
            ValidatedRetainedBootFilePublication,
        },
    },
};

use super::{
    ActiveReblitBootPublicationPreflightError,
    RevalidatedActiveReblitBootPublicationPreflight,
    assess_bound_namespaces_with, assess_one_bound_namespace, require_same_target_set,
    require_target_deadline,
};

/// Unforgeable safe-code proof that the exact staged authority passed its
/// immediate pre-effect revalidation inside this aggregate executor.
///
/// The type is visible only so the opaque target bridge can require it; its
/// private field prevents any sibling client component from minting one.
pub(in crate::client) struct ActiveReblitBootPublicationEffectSeal {
    _private: (),
}

/// Terminal exact-output evidence which still owns the original staged
/// `BootSyncStarted` authority.
///
/// This value is deliberately non-`Clone`. It grants no direct pending-head
/// mutation, replacement, removal, or journal-advance operation; exact receipt
/// promotion is available only by consuming the complete token.
#[must_use = "terminal boot-publication evidence must be promoted or deliberately discarded"]
pub(in crate::client) struct StagedExactActiveReblitBootPublication<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
> {
    staged: StagedActiveReblitBootSync<
        'plan,
        'inventory,
        BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
    >,
    publication_count: usize,
    published_count: usize,
    already_exact_count: usize,
    evidence: Vec<ValidatedRetainedBootFilePublication>,
}

impl std::fmt::Debug
    for StagedExactActiveReblitBootPublication<'_, '_, '_, '_, '_, '_, '_, '_>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StagedExactActiveReblitBootPublication")
            .field("receipt_fingerprint", &self.staged.receipt_fingerprint())
            .field("publication_count", &self.publication_count)
            .field("published_count", &self.published_count)
            .field("already_exact_count", &self.already_exact_count)
            .field("evidence_count", &self.evidence.len())
            .field("durable_phase", &"BootSyncStarted")
            .finish_non_exhaustive()
    }
}

impl StagedExactActiveReblitBootPublication<'_, '_, '_, '_, '_, '_, '_, '_> {
    pub(in crate::client) const fn receipt_fingerprint(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.staged.receipt_fingerprint()
    }

    pub(in crate::client) const fn publication_count(&self) -> usize {
        self.publication_count
    }

    pub(in crate::client) const fn published_count(&self) -> usize {
        self.published_count
    }

    pub(in crate::client) const fn already_exact_count(&self) -> usize {
        self.already_exact_count
    }

    pub(in crate::client) fn evidence(&self) -> &[ValidatedRetainedBootFilePublication] {
        &self.evidence
    }
}

/// Failure of one consumed staged immutable-publication attempt.
///
/// An error never returns the staging authority. The durable journal and
/// database remain at their already-staged `BootSyncStarted`/pending state for
/// a later recovery coordinator; this layer never rolls them forward or back.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootImmutablePublicationAttemptError {
    #[error("admit the exact staged BootSyncStarted authority against this client")]
    StagedAdmission(#[source] ActiveReblitBootSyncFreshValidationError),
    #[error("read-only preflight the complete staged boot-publication namespace")]
    Preflight(#[source] ActiveReblitBootPublicationPreflightError),
    #[error("the staged authority and aggregate preflight retain different publication plans at {checkpoint}")]
    StagedPlanMismatch { checkpoint: &'static str },
    #[error("the aggregate publication attempt exceeded its retained deadline {deadline:?} at {checkpoint}")]
    DeadlineExceeded {
        checkpoint: &'static str,
        deadline: Instant,
    },
    #[error("publication {plan_index} has role {role:?} but phase {found:?}, expected {expected:?}")]
    RolePhaseMismatch {
        plan_index: usize,
        role: ActiveReblitBootPublicationRole,
        expected: ActiveReblitBootPublicationPhase,
        found: ActiveReblitBootPublicationPhase,
    },
    #[error("publication {plan_index} phase {found:?} precedes prior global phase {previous:?}")]
    GlobalPhaseOrder {
        plan_index: usize,
        previous: ActiveReblitBootPublicationPhase,
        found: ActiveReblitBootPublicationPhase,
    },
    #[error("publication {plan_index} has unsupported mode {found:o}, expected {expected:o}")]
    PublicationMode {
        plan_index: usize,
        expected: u32,
        found: u32,
    },
    #[error("publication {plan_index} path is not UTF-8")]
    NonUtf8Path { plan_index: usize },
    #[error("publication {plan_index} path has no retained parent chain")]
    MissingPublicationParent { plan_index: usize },
    #[error("publication {plan_index} path contains an invalid component")]
    InvalidPathComponent { plan_index: usize },
    #[error("publication {plan_index} path exceeds the 15-component parent ceiling")]
    PublicationParentDepth { plan_index: usize },
    #[error("the aggregate preflight target and namespace-input layouts differ")]
    DestinationLayoutMismatch,
    #[error("publication {plan_index} is absent from its expected {role:?} namespace domain")]
    DomainPlanIndexMissing {
        role: BootTargetRole,
        plan_index: usize,
    },
    #[error("publication {plan_index} maps to {found:?}, expected {expected:?}")]
    DestinationRoleMismatch {
        plan_index: usize,
        expected: BootTargetRole,
        found: BootTargetRole,
    },
    #[error("publication {plan_index} has a non-admissible preflight destination state")]
    InvalidPreflightState { plan_index: usize },
    #[error("revalidate staged journal, receipt, database, and installation immediately before effects")]
    PreEffectStagedValidation(#[source] ActiveReblitBootSyncFreshValidationError),
    #[error("publish immutable boot output {plan_index} through {role:?}")]
    LeafPublication {
        role: BootTargetRole,
        plan_index: usize,
        #[source]
        source: ActiveReblitBootImmutableLeafPublicationError,
    },
    #[error("the aggregate publication counters overflowed")]
    PublicationCounterOverflow,
    #[error("allocate the bounded global publication-evidence vector before effects")]
    EvidenceAllocation(#[source] TryReserveError),
    #[error(
        "terminal publication accounting recorded {actual} outcomes for {expected} planned outputs"
    )]
    PublicationCountMismatch { expected: usize, actual: usize },
    #[error("terminally reassess every aggregate boot-publication output")]
    TerminalNamespaceAssessment(#[source] ActiveReblitBootPublicationPreflightError),
    #[error("terminal publication {plan_index} remains {state:?} instead of Exact")]
    TerminalDestinationNotExact {
        plan_index: usize,
        state: BootNamespaceDestinationState,
    },
    #[error("the boot-publication collision domains changed after immutable publication")]
    CollisionDomainDrift,
    #[error("recapture the complete mounted boot topology after immutable publication")]
    TerminalTargets(#[source] ActiveReblitBootPublicationTargetsError),
    #[error("the terminal boot-publication targets differ from aggregate preflight")]
    TerminalTargetMismatch(#[source] ActiveReblitBootPublicationPreflightError),
    #[error("revalidate staged journal, receipt, database, and installation after terminal topology capture")]
    TerminalStagedValidation(#[source] ActiveReblitBootSyncFreshValidationError),
}

impl<
        'plan,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
    RevalidatedActiveReblitBootPublicationPreflight<
        'plan,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
where
    'input: 'plan,
{
    /// Consume one exact staged authority into a single aggregate immutable
    /// publication attempt. No retry authority is returned on failure.
    pub(in crate::client) fn publish_from_staged_authority<'inventory>(
        self,
        staged: StagedActiveReblitBootSync<
            'plan,
            'inventory,
            BoundActiveReblitBlsPublicationPlan<
                'input,
                'topology_view,
                'topology_authority,
                'attempt,
                'stone,
                'roots,
            >,
        >,
        client: &Client,
    ) -> Result<
        StagedExactActiveReblitBootPublication<
            'plan,
            'inventory,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootImmutablePublicationAttemptError,
    > {
        let deadline = self.deadline();
        require_attempt_deadline("schedule validation entry", deadline)?;
        let mut evidence = prepare_execution_schedule(&self)?;
        require_attempt_deadline("after complete schedule validation", deadline)?;

        {
            let fresh = staged
                .revalidate_against(client)
                .map_err(
                    ActiveReblitBootImmutablePublicationAttemptError::PreEffectStagedValidation,
                )?;
            if !std::ptr::eq(fresh.plan(), self.plan) {
                return Err(
                    ActiveReblitBootImmutablePublicationAttemptError::StagedPlanMismatch {
                        checkpoint: "pre-effect durable revalidation",
                    },
                );
            }
        }
        require_attempt_deadline("immediately before first namespace effect", deadline)?;
        let effect_seal = ActiveReblitBootPublicationEffectSeal { _private: () };

        let mut published_count = 0usize;
        let mut already_exact_count = 0usize;
        for (plan_index, output) in self.plan.outputs().enumerate() {
            require_attempt_deadline("before immutable output publication", deadline)?;
            let routed = route_publication(
                &self.targets,
                &self.namespace_inputs,
                self.plan.destination_layout(),
                output.root(),
                plan_index,
            )?;
            let initial_state = self.initial_states[plan_index];
            let publication_evidence = routed
                .target
                .publish_preflighted_immutable_leaf(
                    &effect_seal,
                    plan_index,
                    &output,
                    routed.namespace_request,
                    routed.expected_source,
                    initial_state,
                )
                .map_err(|source| {
                    ActiveReblitBootImmutablePublicationAttemptError::LeafPublication {
                        role: routed.role,
                        plan_index,
                        source,
                    }
                })?;
            match publication_evidence.outcome() {
                RetainedBootFilePublicationOutcome::Published => {
                    published_count = published_count
                        .checked_add(1)
                        .ok_or(
                            ActiveReblitBootImmutablePublicationAttemptError::PublicationCounterOverflow,
                        )?;
                }
                RetainedBootFilePublicationOutcome::AlreadyExact => {
                    already_exact_count = already_exact_count
                        .checked_add(1)
                        .ok_or(
                            ActiveReblitBootImmutablePublicationAttemptError::PublicationCounterOverflow,
                        )?;
                }
            }
            evidence.push(publication_evidence);
            require_attempt_deadline("after immutable output publication", deadline)?;
        }

        let terminal_states = terminal_namespace_assessment(&self)?;
        for (plan_index, state) in terminal_states.iter().copied().enumerate() {
            if state != BootNamespaceDestinationState::Exact {
                return Err(
                    ActiveReblitBootImmutablePublicationAttemptError::TerminalDestinationNotExact {
                        plan_index,
                        state,
                    },
                );
            }
        }
        if !self.plan.collision_domains_still_match() {
            return Err(ActiveReblitBootImmutablePublicationAttemptError::CollisionDomainDrift);
        }
        require_attempt_deadline("before terminal topology capture", deadline)?;
        let terminal_targets = self
            .plan
            .revalidate_publication_targets()
            .map_err(ActiveReblitBootImmutablePublicationAttemptError::TerminalTargets)?;
        require_target_deadline("post-publication target capture", deadline, &terminal_targets)
            .map_err(
                ActiveReblitBootImmutablePublicationAttemptError::TerminalTargetMismatch,
            )?;
        require_same_target_set(&self.targets, &terminal_targets).map_err(
            ActiveReblitBootImmutablePublicationAttemptError::TerminalTargetMismatch,
        )?;
        drop(terminal_targets);
        require_attempt_deadline("after terminal topology capture", deadline)?;

        {
            let fresh = staged
                .revalidate_against(client)
                .map_err(
                    ActiveReblitBootImmutablePublicationAttemptError::TerminalStagedValidation,
                )?;
            if !std::ptr::eq(fresh.plan(), self.plan) {
                return Err(
                    ActiveReblitBootImmutablePublicationAttemptError::StagedPlanMismatch {
                        checkpoint: "terminal durable revalidation",
                    },
                );
            }
        }
        require_attempt_deadline("terminal staged publication evidence", deadline)?;

        let publication_count = self.publication_count();
        let accounted = published_count
            .checked_add(already_exact_count)
            .ok_or(ActiveReblitBootImmutablePublicationAttemptError::PublicationCounterOverflow)?;
        if accounted != publication_count {
            return Err(
                ActiveReblitBootImmutablePublicationAttemptError::PublicationCountMismatch {
                    expected: publication_count,
                    actual: accounted,
                },
            );
        }
        if evidence.len() != publication_count {
            return Err(
                ActiveReblitBootImmutablePublicationAttemptError::PublicationCountMismatch {
                    expected: publication_count,
                    actual: evidence.len(),
                },
            );
        }
        Ok(StagedExactActiveReblitBootPublication {
            staged,
            publication_count,
            published_count,
            already_exact_count,
            evidence,
        })
    }
}

fn prepare_execution_schedule(
    preflight: &RevalidatedActiveReblitBootPublicationPreflight<'_, '_, '_, '_, '_, '_, '_>,
) -> Result<Vec<ValidatedRetainedBootFilePublication>, ActiveReblitBootImmutablePublicationAttemptError> {
    let mut evidence = Vec::new();
    evidence
        .try_reserve_exact(preflight.publication_count())
        .map_err(ActiveReblitBootImmutablePublicationAttemptError::EvidenceAllocation)?;
    let mut previous_phase = None;
    for (plan_index, output) in preflight.plan.outputs().enumerate() {
        let expected_phase = phase_for_role(output.role());
        if output.phase() != expected_phase {
            return Err(ActiveReblitBootImmutablePublicationAttemptError::RolePhaseMismatch {
                plan_index,
                role: output.role(),
                expected: expected_phase,
                found: output.phase(),
            });
        }
        if let Some(previous) = previous_phase {
            if output.phase() < previous {
                return Err(ActiveReblitBootImmutablePublicationAttemptError::GlobalPhaseOrder {
                    plan_index,
                    previous,
                    found: output.phase(),
                });
            }
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
        if preflight.initial_states[plan_index] == BootNamespaceDestinationState::Different {
            return Err(
                ActiveReblitBootImmutablePublicationAttemptError::InvalidPreflightState {
                    plan_index,
                },
            );
        }
    }
    Ok(evidence)
}

fn terminal_namespace_assessment(
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

struct RoutedPublication<'view, 'source> {
    role: BootTargetRole,
    target: &'view RevalidatedActiveReblitBootPublicationTarget<'source>,
    namespace_request: BootNamespaceRequest<'source>,
    expected_source: &'view RetainedBootNamespaceExpectedSource<'source>,
}

fn route_publication<'view, 'source>(
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

const fn destination_role(
    layout: ActiveReblitBootDestinationLayout,
    root: ActiveReblitBootDestinationRoot,
) -> BootTargetRole {
    match (layout, root) {
        (ActiveReblitBootDestinationLayout::BootAliasesEsp, _) |
        (ActiveReblitBootDestinationLayout::DistinctXbootldr, ActiveReblitBootDestinationRoot::Esp) => {
            BootTargetRole::Esp
        }
        (
            ActiveReblitBootDestinationLayout::DistinctXbootldr,
            ActiveReblitBootDestinationRoot::Boot,
        ) => BootTargetRole::Xbootldr,
    }
}

fn domain_plan_position(
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

struct PublicationPath<'path> {
    parent_components: [&'path str; 15],
    parent_count: usize,
    leaf: &'path str,
}

impl<'path> PublicationPath<'path> {
    fn parents(&self) -> &[&'path str] {
        &self.parent_components[..self.parent_count]
    }
}

fn split_publication_path(
    path: &Path,
    plan_index: usize,
) -> Result<PublicationPath<'_>, ActiveReblitBootImmutablePublicationAttemptError> {
    let path = path
        .to_str()
        .ok_or(ActiveReblitBootImmutablePublicationAttemptError::NonUtf8Path { plan_index })?;
    let mut components = path.split('/');
    let mut prior = components.next().ok_or(
        ActiveReblitBootImmutablePublicationAttemptError::InvalidPathComponent { plan_index },
    )?;
    require_component(prior, plan_index)?;
    let mut parent_components = [""; 15];
    let mut parent_count = 0usize;
    for component in components {
        require_component(component, plan_index)?;
        if parent_count == parent_components.len() {
            return Err(
                ActiveReblitBootImmutablePublicationAttemptError::PublicationParentDepth {
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
            ActiveReblitBootImmutablePublicationAttemptError::MissingPublicationParent {
                plan_index,
            },
        );
    }
    Ok(PublicationPath {
        parent_components,
        parent_count,
        leaf: prior,
    })
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

fn require_attempt_deadline(
    checkpoint: &'static str,
    deadline: Instant,
) -> Result<(), ActiveReblitBootImmutablePublicationAttemptError> {
    if Instant::now() > deadline {
        Err(ActiveReblitBootImmutablePublicationAttemptError::DeadlineExceeded {
            checkpoint,
            deadline,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "immutable_attempt/tests.rs"]
mod tests;

#[path = "immutable_attempt/receipt_promotion.rs"]
mod receipt_promotion;
pub(in crate::client) use receipt_promotion::{
    ActiveReblitBootReceiptPromotionError,
    PromotedExactActiveReblitBootPublication,
};
