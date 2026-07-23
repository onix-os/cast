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

use std::{collections::TryReserveError, time::Instant};

use thiserror::Error;

use crate::{
    boot_publication::BootPublicationReceiptFingerprint,
    client::{
        Client,
        active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_boot_sync_staging::{
            ActiveReblitBootSyncFreshValidationError,
            StagedActiveReblitBootSync,
        },
        active_reblit_installed_boot_publication_delta::{
            ActiveReblitBootPublicationDeltaAction,
            ActiveReblitBootPublicationDeltaError,
            ActiveReblitBootPublicationEffectScheduleError,
        },
        active_reblit_mounted_boot_topology::{
            ActiveReblitBootImmutableLeafPublicationError,
            ActiveReblitBootOwnedLeafReplacementError,
            ActiveReblitBootPublicationTargetsError, BootTargetRole,
        },
        active_reblit_publication_plan::{
            ActiveReblitBootPublicationPhase, ActiveReblitBootPublicationRole,
        },
    },
    linux_fs::{
        descriptor_boot_namespace::BootNamespaceDestinationState,
        mount_namespace::RetainedBootFilePublicationOutcome,
    },
};

use super::{
    ActiveReblitBootPublicationPreflightError,
    RevalidatedActiveReblitBootPublicationPreflight,
    require_same_target_set, require_target_deadline,
};

use execution_schedule::{
    initial_state_for_action, prepare_execution_schedule, route_publication,
    terminal_namespace_assessment,
};
#[cfg(test)]
use execution_schedule::{destination_role, domain_plan_position};

/// Unforgeable safe-code proof that the exact staged authority passed its
/// immediate pre-effect revalidation inside this aggregate executor.
///
/// The type is visible only so the opaque target bridge can require it; its
/// private field prevents any sibling client component from minting one.
pub(in crate::client) struct ActiveReblitBootPublicationEffectSeal {
    pending_receipt: BootPublicationReceiptFingerprint,
}

impl ActiveReblitBootPublicationEffectSeal {
    const fn new(pending_receipt: BootPublicationReceiptFingerprint) -> Self {
        Self { pending_receipt }
    }

    pub(in crate::client) const fn pending_receipt(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.pending_receipt
    }
}

/// Unforgeable proof that cleanup belongs to the exact receipt which has
/// already become the committed boot-publication head.
///
/// The opaque target bridge can inspect the owner fingerprint but cannot mint
/// this value. Only the consuming promoted-publication cleanup path can do so
/// after repeating its journal, database, plan, and namespace admission.
pub(in crate::client) struct ActiveReblitBootPromotedCleanupSeal {
    promoted_receipt: BootPublicationReceiptFingerprint,
}

impl ActiveReblitBootPromotedCleanupSeal {
    const fn new(promoted_receipt: BootPublicationReceiptFingerprint) -> Self {
        Self { promoted_receipt }
    }

    pub(in crate::client) const fn promoted_receipt(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.promoted_receipt
    }
}

/// Unforgeable safe-code proof that exact promoted terminal publication
/// evidence passed the completion handoff. Only descendants of this module
/// can construct the fieldless seal; the staging state owner may consume it
/// but cannot mint it.
pub(in crate::client) struct ActiveReblitBootSyncCompletionSeal {
    _private: (),
}

/// Unforgeable proof that the completed publication's exact terminal evidence
/// was repeated immediately before commit-decision coordination.
pub(in crate::client) struct ActiveReblitBootSyncCommitDecisionSeal {
    _private: (),
}

/// Unforgeable proof that the live boot coordinator retained the exact
/// generation-13 `CommitDecided` handoff through its terminal-evidence check.
/// Only descendants of this module can mint the fieldless seal; the shared
/// startup cleanup authority may consume it but cannot manufacture it.
pub(in crate::client) struct ActiveReblitCommitCleanupSeal {
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
    replaced_count: usize,
    promoted_cleanup_required: bool,
    evidence: Vec<ValidatedActiveReblitBootPublicationEffect>,
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
            .field("replaced_count", &self.replaced_count)
            .field("promoted_cleanup_required", &self.promoted_cleanup_required)
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

    pub(in crate::client) const fn replaced_count(&self) -> usize {
        self.replaced_count
    }

    pub(in crate::client) const fn promoted_cleanup_required(&self) -> bool {
        self.promoted_cleanup_required
    }

    pub(in crate::client) fn evidence(&self) -> &[ValidatedActiveReblitBootPublicationEffect] {
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
    #[error("recapture a fresh sealed boot delta immediately before effects")]
    PreEffectDeltaClassification(#[source] ActiveReblitBootPublicationDeltaError),
    #[error("the fresh sealed boot delta differs from the exact classification retained at staging")]
    PreEffectDeltaClassificationDrift,
    #[error("close the exact sealed boot delta into canonical desired-output effect order")]
    EffectSchedule(#[source] ActiveReblitBootPublicationEffectScheduleError),
    #[error("publish immutable boot output {plan_index} through {role:?}")]
    LeafPublication {
        role: BootTargetRole,
        plan_index: usize,
        #[source]
        source: ActiveReblitBootImmutableLeafPublicationError,
    },
    #[error("replace receipt-owned boot output {plan_index} through {role:?}")]
    OwnedLeafReplacement {
        role: BootTargetRole,
        plan_index: usize,
        #[source]
        source: ActiveReblitBootOwnedLeafReplacementError,
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
        let retained_plan = self.plan;
        let deadline = self.deadline();
        require_attempt_deadline("pre-effect recapture entry", deadline)?;
        drop(self);

        // This is the last admission boundary before namespace mutation. The
        // newly captured preflight is classified against the exact delta
        // retained inside the freshly revalidated staging authority. No seal,
        // schedule, target effect, or output evidence exists before equality
        // with the staging-time classification is proved.
        let fresh = staged
            .revalidate_against(client)
            .map_err(
                ActiveReblitBootImmutablePublicationAttemptError::PreEffectStagedValidation,
            )?;
        if !std::ptr::eq(fresh.plan(), retained_plan) {
            return Err(
                ActiveReblitBootImmutablePublicationAttemptError::StagedPlanMismatch {
                    checkpoint: "pre-effect durable revalidation",
                },
            );
        }
        let preflight = fresh
            .plan()
            .prepare_boot_publication_preflight()
            .map_err(ActiveReblitBootImmutablePublicationAttemptError::Preflight)?;
        let classified = preflight
            .classify_installed_boot_publication_delta(fresh.prepared_delta())
            .map_err(
                ActiveReblitBootImmutablePublicationAttemptError::PreEffectDeltaClassification,
            )?;
        if &classified != fresh.classified_delta() {
            return Err(
                ActiveReblitBootImmutablePublicationAttemptError::PreEffectDeltaClassificationDrift,
            );
        }
        let schedule = classified
            .prepare_effect_schedule(retained_plan)
            .map_err(ActiveReblitBootImmutablePublicationAttemptError::EffectSchedule)?;
        let pending_receipt = fresh.receipt_fingerprint();
        drop(fresh);

        let mut evidence = prepare_execution_schedule(&preflight, &schedule)?;
        require_attempt_deadline("after complete sealed schedule validation", deadline)?;
        after_pre_effect_schedule_validation();
        let immediate = staged
            .revalidate_against(client)
            .map_err(
                ActiveReblitBootImmutablePublicationAttemptError::PreEffectStagedValidation,
            )?;
        if !std::ptr::eq(immediate.plan(), retained_plan)
            || immediate.receipt_fingerprint() != pending_receipt
            || immediate.classified_delta() != &classified
        {
            return Err(
                ActiveReblitBootImmutablePublicationAttemptError::StagedPlanMismatch {
                    checkpoint: "immediate pre-effect durable revalidation",
                },
            );
        }
        require_attempt_deadline("immediately before first namespace effect", deadline)?;
        let effect_seal = ActiveReblitBootPublicationEffectSeal::new(
            immediate.receipt_fingerprint(),
        );
        drop(immediate);

        let mut published_count = 0usize;
        let mut already_exact_count = 0usize;
        let mut replaced_count = 0usize;
        for (scheduled, output) in schedule.entries().iter().zip(retained_plan.outputs()) {
            let plan_index = scheduled.plan_index();
            require_attempt_deadline("before classified output effect", deadline)?;
            let routed = route_publication(
                &preflight.targets,
                &preflight.namespace_inputs,
                retained_plan.destination_layout(),
                output.root(),
                plan_index,
            )?;
            let effect_evidence = match scheduled.action() {
                ActiveReblitBootPublicationDeltaAction::PublishDesired
                | ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired
                | ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired => {
                    let initial_state = initial_state_for_action(scheduled.action()).ok_or(
                        ActiveReblitBootImmutablePublicationAttemptError::InvalidPreflightState {
                            plan_index,
                        },
                    )?;
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
                            published_count = published_count.checked_add(1).ok_or(
                                ActiveReblitBootImmutablePublicationAttemptError::PublicationCounterOverflow,
                            )?;
                        }
                        RetainedBootFilePublicationOutcome::AlreadyExact => {
                            already_exact_count = already_exact_count.checked_add(1).ok_or(
                                ActiveReblitBootImmutablePublicationAttemptError::PublicationCounterOverflow,
                            )?;
                        }
                    }
                    match scheduled.action() {
                        ActiveReblitBootPublicationDeltaAction::PublishDesired => {
                            ValidatedActiveReblitBootPublicationEffect::Published {
                                plan_index,
                                evidence: publication_evidence,
                            }
                        }
                        ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired => {
                            ValidatedActiveReblitBootPublicationEffect::RetainedOwned {
                                plan_index,
                                evidence: publication_evidence,
                            }
                        }
                        ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired => {
                            ValidatedActiveReblitBootPublicationEffect::PreservedBorrowed {
                                plan_index,
                                evidence: publication_evidence,
                            }
                        }
                        _ => unreachable!("closed immutable action dispatch"),
                    }
                }
                ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired => {
                    let installed_expected = scheduled.installed_expected().ok_or(
                        ActiveReblitBootImmutablePublicationAttemptError::InvalidPreflightState {
                            plan_index,
                        },
                    )?;
                    let replacement_evidence = routed
                        .target
                        .replace_preflighted_owned_leaf(
                            &effect_seal,
                            plan_index,
                            &output,
                            routed.namespace_request,
                            routed.expected_source,
                            scheduled.desired_expected(),
                            installed_expected,
                        )
                        .map_err(|source| {
                            ActiveReblitBootImmutablePublicationAttemptError::OwnedLeafReplacement {
                                role: routed.role,
                                plan_index,
                                source,
                            }
                        })?;
                    replaced_count = replaced_count.checked_add(1).ok_or(
                        ActiveReblitBootImmutablePublicationAttemptError::PublicationCounterOverflow,
                    )?;
                    ValidatedActiveReblitBootPublicationEffect::ReplacedOwned {
                        plan_index,
                        evidence: replacement_evidence,
                    }
                }
                ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion
                | ActiveReblitBootPublicationDeltaAction::PreserveUnownedStale => {
                    return Err(
                        ActiveReblitBootImmutablePublicationAttemptError::InvalidPreflightState {
                            plan_index,
                        },
                    );
                }
            };
            evidence.push(effect_evidence);
            require_attempt_deadline("after classified output effect", deadline)?;
        }

        let terminal_states = terminal_namespace_assessment(&preflight)?;
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
        if !retained_plan.collision_domains_still_match() {
            return Err(ActiveReblitBootImmutablePublicationAttemptError::CollisionDomainDrift);
        }
        require_attempt_deadline("before terminal topology capture", deadline)?;
        let terminal_targets = retained_plan
            .revalidate_publication_targets()
            .map_err(ActiveReblitBootImmutablePublicationAttemptError::TerminalTargets)?;
        require_target_deadline("post-publication target capture", deadline, &terminal_targets)
            .map_err(
                ActiveReblitBootImmutablePublicationAttemptError::TerminalTargetMismatch,
            )?;
        require_same_target_set(&preflight.targets, &terminal_targets).map_err(
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
            if !std::ptr::eq(fresh.plan(), retained_plan) {
                return Err(
                    ActiveReblitBootImmutablePublicationAttemptError::StagedPlanMismatch {
                        checkpoint: "terminal durable revalidation",
                    },
                );
            }
        }
        require_attempt_deadline("terminal staged publication evidence", deadline)?;

        let publication_count = retained_plan.publication_count();
        let accounted = published_count
            .checked_add(already_exact_count)
            .and_then(|count| count.checked_add(replaced_count))
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
        let promoted_cleanup_required = replaced_count != 0
            || classified.entries().iter().any(|entry| {
                entry.action()
                    == ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion
            });
        Ok(StagedExactActiveReblitBootPublication {
            staged,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            promoted_cleanup_required,
            evidence,
        })
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
std::thread_local! {
    static AFTER_PRE_EFFECT_SCHEDULE_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_after_pre_effect_schedule_validation(callback: impl FnOnce() + 'static) {
    AFTER_PRE_EFFECT_SCHEDULE_VALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(callback)).is_none());
    });
}

#[cfg(test)]
fn after_pre_effect_schedule_validation() {
    AFTER_PRE_EFFECT_SCHEDULE_VALIDATION.with(|slot| {
        if let Some(callback) = slot.borrow_mut().take() {
            callback();
        }
    });
}

#[cfg(not(test))]
fn after_pre_effect_schedule_validation() {}

#[path = "immutable_attempt/effect_evidence.rs"]
mod effect_evidence;
pub(in crate::client) use effect_evidence::ValidatedActiveReblitBootPublicationEffect;
#[path = "immutable_attempt/execution_schedule.rs"]
mod execution_schedule;

#[cfg(test)]
#[path = "immutable_attempt/tests.rs"]
mod tests;

#[path = "immutable_attempt/receipt_promotion.rs"]
mod receipt_promotion;
pub(in crate::client) use receipt_promotion::{
    ActiveReblitBootCommitCleanupCompleteHandoff,
    ActiveReblitBootCommitCleanupError,
    ActiveReblitBootCommitCleanupPostAdvanceError,
    ActiveReblitBootCommitDecisionError,
    ActiveReblitBootCommitDecisionFinalValidation,
    ActiveReblitBootCommitDecisionHandoff,
    ActiveReblitBootPostCompletionValidationError,
    ActiveReblitBootPromotedCleanupError,
    ActiveReblitBootSyncCompletionError,
    ActiveReblitBootReceiptPromotionError,
    CleanedPromotedExactActiveReblitBootPublication,
    CompletedExactActiveReblitBootPublication,
    PromotedExactActiveReblitBootPublication,
};
