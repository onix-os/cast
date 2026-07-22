//! Read-only preflight for one bound ActiveReblit boot-publication attempt.
//!
//! This layer is the first composition point which retains the exact rendered
//! plan, its zero-copy namespace inputs, and freshly revalidated opaque
//! ESP/XBOOTLDR targets together. It assesses every collision domain before a
//! later publisher can create even a parent directory, retains `Different` as
//! read-only evidence for authenticated delta classification, and brackets
//! that assessment with a second complete mounted-topology revalidation.
//!
//! Success is deliberately not mutation authority by itself. The value is
//! non-cloneable, keeps every descriptor behind the opaque target bridge, and
//! exposes scalar initial states only for diagnostics and an unforgeable
//! internal assessment seal only to the delta bridge. It does not publish,
//! replace, remove, promote a receipt, advance a journal, rediscover a target,
//! or mint a fresh deadline.

use std::{collections::TryReserveError, time::Instant};

use thiserror::Error;

use crate::linux_fs::{
    descriptor_boot_namespace::BootNamespaceDestinationState,
    mount_namespace::{
        TaskRootBootNamespaceAssessmentError,
        ValidatedTaskRootBootNamespaceAssessment,
    },
};

use super::{
    active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
    active_reblit_boot_namespace_inputs::{
        ActiveReblitBootNamespaceInputError,
        BoundActiveReblitBootNamespaceDomain,
        BoundActiveReblitBootNamespaceInputs,
    },
    active_reblit_mounted_boot_topology::{
        ActiveReblitBootPublicationTargetsError, BootTargetRole,
        RevalidatedActiveReblitBootPublicationTarget,
        RevalidatedActiveReblitBootPublicationTargets,
    },
};

#[cfg(test)]
#[path = "active_reblit_boot_publication_preflight/fixture_assessment.rs"]
mod fixture_assessment;
#[path = "active_reblit_boot_publication_preflight/assessment_seal.rs"]
mod assessment_seal;
#[path = "active_reblit_boot_publication_preflight/delta_classification.rs"]
mod delta_classification;
#[path = "active_reblit_boot_publication_preflight/immutable_attempt.rs"]
mod immutable_attempt;

pub(in crate::client) use assessment_seal::{
    ActiveReblitBootPublicationAssessmentSeal,
    SealedActiveReblitBootPublicationDesiredState,
};

pub(in crate::client) use immutable_attempt::{
    ActiveReblitBootPublicationEffectSeal,
    ActiveReblitBootSyncCompletionSeal,
    ActiveReblitBootImmutablePublicationAttemptError,
    StagedExactActiveReblitBootPublication,
};
#[allow(unused_imports)] // completed authority is retained for commit coordination
pub(in crate::client) use immutable_attempt::{
    ActiveReblitBootSyncCompletionError,
    ActiveReblitBootReceiptPromotionError,
    CompletedExactActiveReblitBootPublication,
    PromotedExactActiveReblitBootPublication,
};

/// Exact read-only inputs retained for one later publication attempt.
///
/// All borrows originate from `plan`. Keeping that reference alongside the
/// bound namespace sources and target views prevents callers from constructing
/// this capability out of independently selected plans or topologies.
pub(in crate::client) struct RevalidatedActiveReblitBootPublicationPreflight<
    'plan,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
> {
    plan: &'plan BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    namespace_inputs: BoundActiveReblitBootNamespaceInputs<'plan>,
    targets: RevalidatedActiveReblitBootPublicationTargets<'plan>,
    initial_states: Box<[BootNamespaceDestinationState]>,
    assessment_seal: ActiveReblitBootPublicationAssessmentSeal<'plan>,
}

impl std::fmt::Debug
    for RevalidatedActiveReblitBootPublicationPreflight<'_, '_, '_, '_, '_, '_, '_>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevalidatedActiveReblitBootPublicationPreflight")
            .field("publication_count", &self.initial_states.len())
            .field("deadline", &self.targets.deadline())
            .field("authority", &"retained; descriptors hidden")
            .finish()
    }
}

impl RevalidatedActiveReblitBootPublicationPreflight<'_, '_, '_, '_, '_, '_, '_> {
    pub(in crate::client) fn initial_states(&self) -> &[BootNamespaceDestinationState] {
        &self.initial_states
    }

    pub(in crate::client) fn publication_count(&self) -> usize {
        self.initial_states.len()
    }

    pub(in crate::client) fn deadline(&self) -> Instant {
        self.targets.deadline()
    }
}

/// Failure while retaining and read-only assessing one exact bound plan.
///
/// Every variant contains scalar diagnostics or nested closed errors only; no
/// target descriptor, path resolver, source reader, or retry callback escapes.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootPublicationPreflightError {
    #[error("bind the exact boot-publication plan into namespace inputs")]
    NamespaceInputs(#[from] ActiveReblitBootNamespaceInputError),
    #[error("capture the initial opaque boot-publication targets")]
    InitialTargets {
        #[source]
        source: ActiveReblitBootPublicationTargetsError,
    },
    #[error("the retained namespace inputs and publication targets have different destination layouts")]
    DestinationLayoutMismatch,
    #[error("the boot-publication preflight expected {expected} outputs but retained {actual}")]
    PublicationCountMismatch { expected: usize, actual: usize },
    #[error("the {role:?} publication target unexpectedly reports role {found:?}")]
    TargetRoleMismatch {
        role: BootTargetRole,
        found: BootTargetRole,
    },
    #[error(
        "the {role:?} boot namespace retained {requests} requests, {sources} expected sources, and {indices} plan indices"
    )]
    DomainBindingLengthMismatch {
        role: BootTargetRole,
        requests: usize,
        sources: usize,
        indices: usize,
    },
    #[error("assess the {role:?} retained boot namespace")]
    NamespaceAssessment {
        role: BootTargetRole,
        #[source]
        source: TaskRootBootNamespaceAssessmentError,
    },
    #[error(
        "the {role:?} namespace assessment returned {states} states for {indices} retained plan indices"
    )]
    AssessmentLengthMismatch {
        role: BootTargetRole,
        states: usize,
        indices: usize,
    },
    #[error(
        "the {role:?} namespace assessment identity differs from its target: expected st_dev {expected_device}, st_ino {expected_inode}, mount ID {expected_mount_id}, found st_dev {found_device}, st_ino {found_inode}, mount ID {found_mount_id}"
    )]
    AssessmentIdentityMismatch {
        role: BootTargetRole,
        expected_device: u64,
        expected_inode: u64,
        expected_mount_id: u64,
        found_device: u64,
        found_inode: u64,
        found_mount_id: u64,
    },
    #[error(
        "the {role:?} namespace domain plan index {plan_index} is outside the {publication_count}-output plan"
    )]
    PlanIndexOutOfRange {
        role: BootTargetRole,
        plan_index: usize,
        publication_count: usize,
    },
    #[error(
        "the {role:?} namespace domain plan index {plan_index} does not follow prior index {previous} in global order"
    )]
    PlanIndexOrder {
        role: BootTargetRole,
        previous: usize,
        plan_index: usize,
    },
    #[error("boot-publication plan index {plan_index} occurs in more than one retained namespace position")]
    DuplicatePlanIndex { plan_index: usize },
    #[error("boot-publication plan index {plan_index} is absent from the retained namespace inputs")]
    MissingPlanIndex { plan_index: usize },
    #[error("allocate the bounded global boot-publication preflight state map")]
    StateAllocation {
        #[source]
        source: TryReserveError,
    },
    #[error("the boot-publication collision domains changed during read-only preflight")]
    CollisionDomainDrift,
    #[error(
        "boot-publication preflight target deadline differs at {checkpoint}: expected {expected:?}, found {found:?}"
    )]
    DeadlineMismatch {
        checkpoint: &'static str,
        expected: Instant,
        found: Instant,
    },
    #[error("boot-publication preflight exceeded retained deadline {deadline:?} at {checkpoint}")]
    DeadlineExceeded {
        checkpoint: &'static str,
        deadline: Instant,
    },
    #[error("capture the terminal opaque boot-publication targets")]
    TerminalTargets {
        #[source]
        source: ActiveReblitBootPublicationTargetsError,
    },
    #[error("the initial and terminal boot-publication target layouts differ")]
    TerminalTargetLayoutMismatch,
    #[error(
        "the {role:?} terminal publication target differs from its initial target: initial st_dev {initial_device}, st_ino {initial_inode}, mount ID {initial_mount_id}, terminal st_dev {terminal_device}, st_ino {terminal_inode}, mount ID {terminal_mount_id}"
    )]
    TerminalTargetIdentityMismatch {
        role: BootTargetRole,
        initial_device: u64,
        initial_inode: u64,
        initial_mount_id: u64,
        terminal_device: u64,
        terminal_inode: u64,
        terminal_mount_id: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BootPublicationAssessmentIdentity {
    device: u64,
    inode: u64,
    mount_id: u64,
}

enum BootPublicationNamespaceAssessment {
    Retained(ValidatedTaskRootBootNamespaceAssessment),
    #[cfg(test)]
    Fixture {
        identity: BootPublicationAssessmentIdentity,
        states: Box<[BootNamespaceDestinationState]>,
    },
}

impl BootPublicationNamespaceAssessment {
    fn identity(&self) -> BootPublicationAssessmentIdentity {
        match self {
            Self::Retained(assessment) => BootPublicationAssessmentIdentity {
                device: assessment.destination_device(),
                inode: assessment.destination_inode(),
                mount_id: assessment.destination_mount_id(),
            },
            #[cfg(test)]
            Self::Fixture { identity, .. } => *identity,
        }
    }

    fn states(&self) -> &[BootNamespaceDestinationState] {
        match self {
            Self::Retained(assessment) => assessment.states(),
            #[cfg(test)]
            Self::Fixture { states, .. } => states,
        }
    }

    #[cfg(test)]
    fn fixture(
        identity: BootPublicationAssessmentIdentity,
        states: impl Into<Box<[BootNamespaceDestinationState]>>,
    ) -> Self {
        Self::Fixture {
            identity,
            states: states.into(),
        }
    }
}

impl<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
    BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
{
    /// Retain and read-only preflight every output in this exact bound plan.
    ///
    /// The method inherits the render/topology deadline unchanged. It performs
    /// no filesystem mutation and returns no detachable source or target
    /// authority.
    pub(in crate::client) fn prepare_boot_publication_preflight<'plan>(
        &'plan self,
    ) -> Result<
        RevalidatedActiveReblitBootPublicationPreflight<
            'plan,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootPublicationPreflightError,
    >
    where
        'input: 'plan,
    {
        let mut assess = assess_one_bound_namespace;
        let mut now = Instant::now;
        self.prepare_boot_publication_preflight_with_assessor(&mut assess, &mut now)
    }

    fn prepare_boot_publication_preflight_with_assessor<'plan>(
        &'plan self,
        assess: &mut impl FnMut(
            BootTargetRole,
            &RevalidatedActiveReblitBootPublicationTarget<'_>,
            &BoundActiveReblitBootNamespaceDomain<'_>,
        ) -> Result<BootPublicationNamespaceAssessment, ActiveReblitBootPublicationPreflightError>,
        now: &mut impl FnMut() -> Instant,
    ) -> Result<
        RevalidatedActiveReblitBootPublicationPreflight<
            'plan,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootPublicationPreflightError,
    >
    where
        'input: 'plan,
    {
        let deadline = self.input_deadline();
        require_deadline("entry", deadline, now)?;
        let namespace_inputs = self.bind_boot_namespace_inputs()?;
        require_deadline("after namespace-input binding", deadline, now)?;

        let targets = self
            .revalidate_publication_targets()
            .map_err(|source| ActiveReblitBootPublicationPreflightError::InitialTargets { source })?;
        require_target_deadline("initial target capture", deadline, &targets)?;
        let initial_states = assess_bound_namespaces_with(
            &targets,
            &namespace_inputs,
            self.publication_count(),
            assess,
        )?;
        require_deadline("after namespace assessment", deadline, now)?;

        if !self.collision_domains_still_match() {
            return Err(ActiveReblitBootPublicationPreflightError::CollisionDomainDrift);
        }

        let terminal_targets = self
            .revalidate_publication_targets()
            .map_err(|source| ActiveReblitBootPublicationPreflightError::TerminalTargets { source })?;
        require_target_deadline("terminal target capture", deadline, &terminal_targets)?;
        require_same_target_set(&targets, &terminal_targets)?;
        require_deadline("terminal", deadline, now)?;
        let assessment_seal = assessment_seal::seal_bound_desired_states(self, &initial_states)?;

        Ok(RevalidatedActiveReblitBootPublicationPreflight {
            plan: self,
            namespace_inputs,
            targets,
            initial_states,
            assessment_seal,
        })
    }

    #[cfg(test)]
    fn prepare_boot_publication_preflight_fixture_with<'plan>(
        &'plan self,
        assess: &mut impl FnMut(
            BootTargetRole,
            &RevalidatedActiveReblitBootPublicationTarget<'_>,
            &BoundActiveReblitBootNamespaceDomain<'_>,
        ) -> Result<BootPublicationNamespaceAssessment, ActiveReblitBootPublicationPreflightError>,
        now: &mut impl FnMut() -> Instant,
    ) -> Result<
        RevalidatedActiveReblitBootPublicationPreflight<
            'plan,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootPublicationPreflightError,
    >
    where
        'input: 'plan,
    {
        self.prepare_boot_publication_preflight_with_assessor(assess, now)
    }
}

fn assess_one_bound_namespace(
    role: BootTargetRole,
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    domain: &BoundActiveReblitBootNamespaceDomain<'_>,
) -> Result<BootPublicationNamespaceAssessment, ActiveReblitBootPublicationPreflightError> {
    #[cfg(test)]
    if let Some(assessment) = fixture_assessment::take(role, target, domain) {
        return Ok(assessment);
    }
    target
        .assess_boot_namespace(domain.requests(), domain.expected_sources())
        .map(BootPublicationNamespaceAssessment::Retained)
        .map_err(|source| ActiveReblitBootPublicationPreflightError::NamespaceAssessment {
            role,
            source,
        })
}

fn assess_bound_namespaces_with(
    targets: &RevalidatedActiveReblitBootPublicationTargets<'_>,
    inputs: &BoundActiveReblitBootNamespaceInputs<'_>,
    publication_count: usize,
    assess: &mut impl FnMut(
        BootTargetRole,
        &RevalidatedActiveReblitBootPublicationTarget<'_>,
        &BoundActiveReblitBootNamespaceDomain<'_>,
    ) -> Result<BootPublicationNamespaceAssessment, ActiveReblitBootPublicationPreflightError>,
) -> Result<Box<[BootNamespaceDestinationState]>, ActiveReblitBootPublicationPreflightError> {
    require_publication_count(publication_count, retained_publication_count(inputs))?;

    let mut states = Vec::new();
    states
        .try_reserve_exact(publication_count)
        .map_err(|source| ActiveReblitBootPublicationPreflightError::StateAllocation { source })?;
    states.resize(publication_count, None);

    match (targets, inputs) {
        (
            RevalidatedActiveReblitBootPublicationTargets::BootAliasesEsp { esp },
            BoundActiveReblitBootNamespaceInputs::BootAliasesEsp { shared },
        ) => assess_domain_with(BootTargetRole::Esp, esp, shared, &mut states, assess)?,
        (
            RevalidatedActiveReblitBootPublicationTargets::DistinctXbootldr {
                esp,
                xbootldr,
            },
            BoundActiveReblitBootNamespaceInputs::DistinctXbootldr {
                esp: esp_inputs,
                xbootldr: xbootldr_inputs,
            },
        ) => {
            assess_domain_with(BootTargetRole::Esp, esp, esp_inputs, &mut states, assess)?;
            assess_domain_with(
                BootTargetRole::Xbootldr,
                xbootldr,
                xbootldr_inputs,
                &mut states,
                assess,
            )?;
        }
        _ => {
            return Err(
                ActiveReblitBootPublicationPreflightError::DestinationLayoutMismatch,
            );
        }
    }

    close_global_states(states)
}

fn require_publication_count(
    expected: usize,
    actual: usize,
) -> Result<(), ActiveReblitBootPublicationPreflightError> {
    if actual != expected {
        Err(ActiveReblitBootPublicationPreflightError::PublicationCountMismatch {
            expected,
            actual,
        })
    } else {
        Ok(())
    }
}

fn close_global_states(
    states: Vec<Option<BootNamespaceDestinationState>>,
) -> Result<Box<[BootNamespaceDestinationState]>, ActiveReblitBootPublicationPreflightError> {
    let mut closed = Vec::new();
    closed
        .try_reserve_exact(states.len())
        .map_err(|source| ActiveReblitBootPublicationPreflightError::StateAllocation { source })?;
    for (plan_index, state) in states.into_iter().enumerate() {
        let Some(state) = state else {
            return Err(ActiveReblitBootPublicationPreflightError::MissingPlanIndex {
                plan_index,
            });
        };
        closed.push(state);
    }
    Ok(closed.into_boxed_slice())
}

fn retained_publication_count(inputs: &BoundActiveReblitBootNamespaceInputs<'_>) -> usize {
    match inputs {
        BoundActiveReblitBootNamespaceInputs::BootAliasesEsp { shared } => {
            shared.plan_indices().len()
        }
        BoundActiveReblitBootNamespaceInputs::DistinctXbootldr {
            esp,
            xbootldr,
        } => esp
            .plan_indices()
            .len()
            .saturating_add(xbootldr.plan_indices().len()),
    }
}

fn assess_domain_with(
    role: BootTargetRole,
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    domain: &BoundActiveReblitBootNamespaceDomain<'_>,
    global_states: &mut [Option<BootNamespaceDestinationState>],
    assess: &mut impl FnMut(
        BootTargetRole,
        &RevalidatedActiveReblitBootPublicationTarget<'_>,
        &BoundActiveReblitBootNamespaceDomain<'_>,
    ) -> Result<BootPublicationNamespaceAssessment, ActiveReblitBootPublicationPreflightError>,
) -> Result<(), ActiveReblitBootPublicationPreflightError> {
    if target.role() != role {
        return Err(ActiveReblitBootPublicationPreflightError::TargetRoleMismatch {
            role,
            found: target.role(),
        });
    }
    let requests = domain.requests();
    let expected = domain.expected_sources();
    let indices = domain.plan_indices();
    if requests.len() != expected.len() || requests.len() != indices.len() {
        return Err(
            ActiveReblitBootPublicationPreflightError::DomainBindingLengthMismatch {
                role,
                requests: requests.len(),
                sources: expected.len(),
                indices: indices.len(),
            },
        );
    }

    let assessment = assess(role, target, domain)?;
    merge_domain_assessment(
        role,
        target_identity(target),
        indices,
        &assessment,
        global_states,
    )
}

fn merge_domain_assessment(
    role: BootTargetRole,
    expected_identity: BootPublicationAssessmentIdentity,
    indices: &[usize],
    assessment: &BootPublicationNamespaceAssessment,
    global_states: &mut [Option<BootNamespaceDestinationState>],
) -> Result<(), ActiveReblitBootPublicationPreflightError> {
    require_assessment_identity(role, expected_identity, assessment)?;
    if assessment.states().len() != indices.len() {
        return Err(
            ActiveReblitBootPublicationPreflightError::AssessmentLengthMismatch {
                role,
                states: assessment.states().len(),
                indices: indices.len(),
            },
        );
    }

    let mut previous = None;
    for (&plan_index, &state) in indices.iter().zip(assessment.states()) {
        if plan_index >= global_states.len() {
            return Err(ActiveReblitBootPublicationPreflightError::PlanIndexOutOfRange {
                role,
                plan_index,
                publication_count: global_states.len(),
            });
        }
        if let Some(prior) = previous {
            if plan_index <= prior {
                return Err(ActiveReblitBootPublicationPreflightError::PlanIndexOrder {
                    role,
                    previous: prior,
                    plan_index,
                });
            }
        }
        previous = Some(plan_index);
        if global_states[plan_index].is_some() {
            return Err(
                ActiveReblitBootPublicationPreflightError::DuplicatePlanIndex {
                    plan_index,
                },
            );
        }
        global_states[plan_index] = Some(state);
    }
    Ok(())
}

fn require_assessment_identity(
    role: BootTargetRole,
    expected: BootPublicationAssessmentIdentity,
    assessment: &BootPublicationNamespaceAssessment,
) -> Result<(), ActiveReblitBootPublicationPreflightError> {
    let found = assessment.identity();
    if expected != found {
        return Err(
            ActiveReblitBootPublicationPreflightError::AssessmentIdentityMismatch {
                role,
                expected_device: expected.device,
                expected_inode: expected.inode,
                expected_mount_id: expected.mount_id,
                found_device: found.device,
                found_inode: found.inode,
                found_mount_id: found.mount_id,
            },
        );
    }
    Ok(())
}

fn target_identity(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
) -> BootPublicationAssessmentIdentity {
    let destination = target.destination();
    BootPublicationAssessmentIdentity {
        device: destination.raw_device(),
        inode: destination.inode(),
        mount_id: target.mount_id(),
    }
}

fn require_same_target_set(
    initial: &RevalidatedActiveReblitBootPublicationTargets<'_>,
    terminal: &RevalidatedActiveReblitBootPublicationTargets<'_>,
) -> Result<(), ActiveReblitBootPublicationPreflightError> {
    match (initial, terminal) {
        (
            RevalidatedActiveReblitBootPublicationTargets::BootAliasesEsp {
                esp: initial_esp,
            },
            RevalidatedActiveReblitBootPublicationTargets::BootAliasesEsp {
                esp: terminal_esp,
            },
        ) => require_same_target(BootTargetRole::Esp, initial_esp, terminal_esp),
        (
            RevalidatedActiveReblitBootPublicationTargets::DistinctXbootldr {
                esp: initial_esp,
                xbootldr: initial_xbootldr,
            },
            RevalidatedActiveReblitBootPublicationTargets::DistinctXbootldr {
                esp: terminal_esp,
                xbootldr: terminal_xbootldr,
            },
        ) => {
            require_same_target(
                BootTargetRole::Esp,
                initial_esp,
                terminal_esp,
            )?;
            require_same_target(
                BootTargetRole::Xbootldr,
                initial_xbootldr,
                terminal_xbootldr,
            )
        }
        _ => Err(
            ActiveReblitBootPublicationPreflightError::TerminalTargetLayoutMismatch,
        ),
    }
}

fn require_same_target(
    role: BootTargetRole,
    initial: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    terminal: &RevalidatedActiveReblitBootPublicationTarget<'_>,
) -> Result<(), ActiveReblitBootPublicationPreflightError> {
    let initial_destination = initial.destination();
    let terminal_destination = terminal.destination();
    let initial_identity = (
        initial_destination.raw_device(),
        initial_destination.inode(),
        initial.mount_id(),
    );
    let terminal_identity = (
        terminal_destination.raw_device(),
        terminal_destination.inode(),
        terminal.mount_id(),
    );
    if initial.role() != role
        || terminal.role() != role
        || initial_identity != terminal_identity
    {
        return Err(
            ActiveReblitBootPublicationPreflightError::TerminalTargetIdentityMismatch {
                role,
                initial_device: initial_identity.0,
                initial_inode: initial_identity.1,
                initial_mount_id: initial_identity.2,
                terminal_device: terminal_identity.0,
                terminal_inode: terminal_identity.1,
                terminal_mount_id: terminal_identity.2,
            },
        );
    }
    Ok(())
}

fn require_target_deadline(
    checkpoint: &'static str,
    expected: Instant,
    targets: &RevalidatedActiveReblitBootPublicationTargets<'_>,
) -> Result<(), ActiveReblitBootPublicationPreflightError> {
    let found = targets.deadline();
    if expected != found {
        Err(ActiveReblitBootPublicationPreflightError::DeadlineMismatch {
            checkpoint,
            expected,
            found,
        })
    } else {
        Ok(())
    }
}

fn require_deadline(
    checkpoint: &'static str,
    deadline: Instant,
    now: &mut impl FnMut() -> Instant,
) -> Result<(), ActiveReblitBootPublicationPreflightError> {
    if now() > deadline {
        Err(ActiveReblitBootPublicationPreflightError::DeadlineExceeded {
            checkpoint,
            deadline,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "active_reblit_boot_publication_preflight_tests.rs"]
mod tests;
