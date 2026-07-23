//! Consuming post-promotion cleanup for owned boot-publication residue.
//!
//! Receipt promotion makes the desired publication authoritative before this
//! boundary can remove anything. Cleanup then recovers fresh descriptor-bound
//! authority for each exact rollback sidecar and predecessor-only stale leaf;
//! the historical replacement evidence retained by the promoted token is
//! never consumed. Any failure consumes the aggregate token, leaving the
//! committed receipt and `BootSyncStarted` record for restart reconciliation.

use thiserror::Error;

use crate::client::{
    Client,
    active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
    active_reblit_boot_sync_staging::ActiveReblitBootSyncPromotedValidationError,
    active_reblit_installed_boot_publication_delta::ActiveReblitBootPublicationDeltaAction,
    active_reblit_mounted_boot_topology::{
        ActiveReblitBootOwnedCleanupError,
        ActiveReblitBootPublicationTargetsError, BootTargetRole,
        RevalidatedActiveReblitBootPublicationTarget,
        RevalidatedActiveReblitBootPublicationTargets,
    },
    active_reblit_publication_plan::{
        ActiveReblitBootDestinationLayout, ActiveReblitBootDestinationRoot,
    },
};

use super::{
    ActiveReblitBootTerminalEvidenceValidationError,
    CleanedPromotedExactActiveReblitBootPublication,
    PromotedExactActiveReblitBootPublication,
    terminal_evidence::validate_exact_terminal_evidence_snapshot,
};
use super::super::ActiveReblitBootPromotedCleanupSeal;

/// Failure while discharging all receipt-owned post-promotion residue.
///
/// No variant returns the consumed promoted token or a target descriptor.
/// Recovery must re-enter from the durable promoted `BootSyncStarted` state.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootPromotedCleanupError {
    #[error("admit the exact promoted BootSyncStarted receipt before cleanup")]
    InitialPromotedEvidence(#[source] ActiveReblitBootSyncPromotedValidationError),
    #[error("validate exact desired outputs and historical effect evidence before cleanup")]
    InitialTerminalEvidence(
        #[source] ActiveReblitBootTerminalEvidenceValidationError,
    ),
    #[error("capture exact boot targets for promoted cleanup")]
    Targets(#[source] ActiveReblitBootPublicationTargetsError),
    #[error("the promoted replacement target shape does not match output {plan_index}")]
    ReplacementTargetShape { plan_index: usize },
    #[error("clean promoted replacement output {plan_index} through {role:?}")]
    ReplacementCleanup {
        role: BootTargetRole,
        plan_index: usize,
        #[source]
        source: ActiveReblitBootOwnedCleanupError,
    },
    #[error("classified stale entry {delta_index} has an invalid promoted-cleanup shape")]
    StaleEntryShape { delta_index: usize },
    #[error("the promoted stale target shape does not match delta entry {delta_index}")]
    StaleTargetShape { delta_index: usize },
    #[error("clean promoted owned-stale entry {delta_index} through {role:?}")]
    StaleCleanup {
        role: BootTargetRole,
        delta_index: usize,
        #[source]
        source: ActiveReblitBootOwnedCleanupError,
    },
    #[error("promoted cleanup accounted for {actual} replacements, expected {expected}")]
    ReplacementCountMismatch { expected: usize, actual: usize },
    #[error("promoted cleanup was required but no owned cleanup action was retained")]
    MissingCleanupAction,
    #[error("revalidate promoted durable evidence at {checkpoint}")]
    PromotedEvidence {
        checkpoint: &'static str,
        #[source]
        source: ActiveReblitBootSyncPromotedValidationError,
    },
    #[error("the retained publication plan changed at promoted-cleanup checkpoint {checkpoint}")]
    PlanMismatch { checkpoint: &'static str },
    #[error("validate exact desired outputs after promoted cleanup")]
    FinalTerminalEvidence(
        #[source] ActiveReblitBootTerminalEvidenceValidationError,
    ),
}

impl<
        'plan,
        'inventory,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
    PromotedExactActiveReblitBootPublication<
        'plan,
        'inventory,
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
    /// Reconcile and remove every exact receipt-owned rollback sidecar and
    /// predecessor-only stale output, then return the sole completion-ready
    /// typestate. The operation never removes an unowned stale output.
    pub(in crate::client) fn cleanup_promoted_outputs(
        mut self,
        client: &Client,
    ) -> Result<
        CleanedPromotedExactActiveReblitBootPublication<
            'plan,
            'inventory,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootPromotedCleanupError,
    > {
        let admitted = self
            .terminal
            .staged
            .revalidate_promoted_against(client)
            .map_err(
                ActiveReblitBootPromotedCleanupError::InitialPromotedEvidence,
            )?;
        let plan = admitted.plan();
        validate_exact_terminal_evidence_snapshot(
            plan,
            self.receipt_fingerprint(),
            self.terminal.publication_count,
            self.terminal.published_count,
            self.terminal.already_exact_count,
            self.terminal.replaced_count,
            &self.terminal.evidence,
            "promoted cleanup admission",
        )
        .map_err(
            ActiveReblitBootPromotedCleanupError::InitialTerminalEvidence,
        )?;

        if !self.promoted_cleanup_required() {
            drop(admitted);
            return Ok(CleanedPromotedExactActiveReblitBootPublication {
                promoted: self,
            });
        }

        let targets = plan
            .revalidate_publication_targets()
            .map_err(ActiveReblitBootPromotedCleanupError::Targets)?;
        self.require_promoted_cleanup_checkpoint(
            client,
            plan,
            "after promoted-cleanup target capture",
        )?;
        let seal = ActiveReblitBootPromotedCleanupSeal::new(
            self.receipt_fingerprint(),
        );

        let mut replacement_count = 0usize;
        for (plan_index, (retained, output)) in self
            .evidence()
            .iter()
            .zip(plan.outputs())
            .enumerate()
        {
            let Some(historical) = retained.replacement_authority() else {
                continue;
            };
            let (role, target) = cleanup_target(
                &targets,
                plan.destination_layout(),
                output.root(),
            )
            .ok_or(
                ActiveReblitBootPromotedCleanupError::ReplacementTargetShape {
                    plan_index,
                },
            )?;
            self.require_promoted_cleanup_checkpoint(
                client,
                plan,
                "immediately before replacement-sidecar cleanup",
            )?;
            target
                .reconcile_and_cleanup_promoted_owned_replacement(
                    &seal,
                    plan_index,
                    &output,
                    historical,
                )
                .map_err(|source| {
                    ActiveReblitBootPromotedCleanupError::ReplacementCleanup {
                        role,
                        plan_index,
                        source,
                    }
                })?;
            replacement_count = replacement_count.checked_add(1).ok_or(
                ActiveReblitBootPromotedCleanupError::ReplacementCountMismatch {
                    expected: self.replaced_count(),
                    actual: usize::MAX,
                },
            )?;
            self.require_promoted_cleanup_checkpoint(
                client,
                plan,
                "after replacement-sidecar cleanup",
            )?;
        }
        if replacement_count != self.replaced_count() {
            return Err(
                ActiveReblitBootPromotedCleanupError::ReplacementCountMismatch {
                    expected: self.replaced_count(),
                    actual: replacement_count,
                },
            );
        }

        let mut stale_count = 0usize;
        for (delta_index, entry) in
            admitted.classified_delta().entries().iter().enumerate()
        {
            match entry.action() {
                ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion => {
                    let expected = entry.installed_expected().ok_or(
                        ActiveReblitBootPromotedCleanupError::StaleEntryShape {
                            delta_index,
                        },
                    )?;
                    if entry.desired_expected().is_some() {
                        return Err(
                            ActiveReblitBootPromotedCleanupError::StaleEntryShape {
                                delta_index,
                            },
                        );
                    }
                    let (role, target) = cleanup_target(
                        &targets,
                        plan.destination_layout(),
                        entry.root(),
                    )
                    .ok_or(
                        ActiveReblitBootPromotedCleanupError::StaleTargetShape {
                            delta_index,
                        },
                    )?;
                    self.require_promoted_cleanup_checkpoint(
                        client,
                        plan,
                        "immediately before owned-stale cleanup",
                    )?;
                    target
                        .reconcile_and_cleanup_promoted_owned_stale(
                            &seal,
                            delta_index,
                            entry.relative_path(),
                            expected,
                        )
                        .map_err(|source| {
                            ActiveReblitBootPromotedCleanupError::StaleCleanup {
                                role,
                                delta_index,
                                source,
                            }
                        })?;
                    stale_count = stale_count.checked_add(1).ok_or(
                        ActiveReblitBootPromotedCleanupError::MissingCleanupAction,
                    )?;
                    self.require_promoted_cleanup_checkpoint(
                        client,
                        plan,
                        "after owned-stale cleanup",
                    )?;
                }
                ActiveReblitBootPublicationDeltaAction::PreserveUnownedStale => {
                    if entry.desired_expected().is_some()
                        || entry.installed_expected().is_none()
                    {
                        return Err(
                            ActiveReblitBootPromotedCleanupError::StaleEntryShape {
                                delta_index,
                            },
                        );
                    }
                }
                ActiveReblitBootPublicationDeltaAction::PublishDesired
                | ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired
                | ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired
                | ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired => {}
            }
        }
        if replacement_count == 0 && stale_count == 0 {
            return Err(ActiveReblitBootPromotedCleanupError::MissingCleanupAction);
        }

        drop(targets);
        drop(admitted);
        validate_exact_terminal_evidence_snapshot(
            plan,
            self.receipt_fingerprint(),
            self.terminal.publication_count,
            self.terminal.published_count,
            self.terminal.already_exact_count,
            self.terminal.replaced_count,
            &self.terminal.evidence,
            "final promoted cleanup",
        )
        .map_err(
            ActiveReblitBootPromotedCleanupError::FinalTerminalEvidence,
        )?;
        self.require_promoted_cleanup_checkpoint(
            client,
            plan,
            "final promoted cleanup",
        )?;

        self.terminal.promoted_cleanup_required = false;
        Ok(CleanedPromotedExactActiveReblitBootPublication {
            promoted: self,
        })
    }

    fn require_promoted_cleanup_checkpoint(
        &self,
        client: &Client,
        plan: &'plan BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        checkpoint: &'static str,
    ) -> Result<(), ActiveReblitBootPromotedCleanupError> {
        let fresh = self
            .terminal
            .staged
            .revalidate_promoted_against(client)
            .map_err(|source| {
                ActiveReblitBootPromotedCleanupError::PromotedEvidence {
                    checkpoint,
                    source,
                }
            })?;
        if !std::ptr::eq(fresh.plan(), plan) {
            return Err(ActiveReblitBootPromotedCleanupError::PlanMismatch {
                checkpoint,
            });
        }
        Ok(())
    }
}

fn cleanup_target<'view, 'target>(
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
            RevalidatedActiveReblitBootPublicationTargets::DistinctXbootldr {
                esp,
                ..
            },
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
