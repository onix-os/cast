//! Terminal-evidence coordination of durable ActiveReblit commit decision.

use std::time::Instant;

use thiserror::Error;

use crate::{
    boot_publication::BootPublicationReceiptFingerprint,
    client::{
        Client,
        active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_boot_publication_preflight::ActiveReblitBootSyncCommitDecisionSeal,
        active_reblit_boot_sync_staging::{
            CommittedStagedActiveReblitBootSync,
            CommittedStagedActiveReblitBootSyncValidationError,
            CompletedStagedActiveReblitCommitDecisionError,
        },
    },
    db::state::BootPublicationReceiptPromotionOutcome,
    transition_journal::TransitionRecord,
};

use super::{
    ActiveReblitBootPostCompletionValidationError,
    ActiveReblitBootTerminalEvidenceValidationError,
    CompletedExactActiveReblitBootPublication,
    ValidatedActiveReblitBootPublicationEffect,
    validate_completed_terminal_sandwich,
    validate_exact_terminal_evidence_snapshot,
};

/// One-origin final terminal validation consumed immediately before the bound
/// journal advance.
///
/// The callback is private, non-cloneable, and one-shot so no lower layer can
/// manufacture or replay terminal publication authority.
pub(in crate::client) struct ActiveReblitBootCommitDecisionFinalValidation<
    'validation,
> {
    callback: Box<
        dyn FnOnce()
                -> Result<Instant, ActiveReblitBootPostCompletionValidationError>
            + 'validation,
    >,
}

impl ActiveReblitBootCommitDecisionFinalValidation<'_> {
    pub(in crate::client) fn validate(
        self,
    ) -> Result<Instant, ActiveReblitBootPostCompletionValidationError> {
        (self.callback)()
    }
}

/// Exact durable `CommitDecided` handoff which still owns the continuously
/// held writer reservation and all original publication-plan evidence.
///
/// This type intentionally implements neither `Clone` nor `Copy` and exposes
/// no constructor or raw-parts escape hatch.
#[must_use = "commit-decision authority must enter cleanup coordination or be deliberately discarded"]
pub(in crate::client) struct ActiveReblitBootCommitDecisionHandoff<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
> {
    committed: CommittedStagedActiveReblitBootSync<
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
    database_outcome: BootPublicationReceiptPromotionOutcome,
    publication_count: usize,
    published_count: usize,
    already_exact_count: usize,
    replaced_count: usize,
    evidence: Vec<ValidatedActiveReblitBootPublicationEffect>,
}

impl std::fmt::Debug
    for ActiveReblitBootCommitDecisionHandoff<'_, '_, '_, '_, '_, '_, '_, '_>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActiveReblitBootCommitDecisionHandoff")
            .field("record", self.committed.record())
            .field("receipt_fingerprint", &self.receipt_fingerprint())
            .field("database_outcome", &self.database_outcome)
            .field("publication_count", &self.publication_count)
            .finish_non_exhaustive()
    }
}

impl ActiveReblitBootCommitDecisionHandoff<'_, '_, '_, '_, '_, '_, '_, '_> {
    pub(in crate::client) const fn record(&self) -> &TransitionRecord {
        self.committed.record()
    }

    pub(in crate::client) const fn receipt_fingerprint(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.committed.receipt_fingerprint()
    }

    pub(in crate::client) const fn database_outcome(
        &self,
    ) -> BootPublicationReceiptPromotionOutcome {
        self.database_outcome
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

    pub(in crate::client) fn evidence(
        &self,
    ) -> &[ValidatedActiveReblitBootPublicationEffect] {
        &self.evidence
    }
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
    CompletedExactActiveReblitBootPublication<
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
    /// Consume exact `BootSyncComplete` publication authority through the sole
    /// durable `CommitDecided` successor, without entering cleanup.
    pub(in crate::client) fn persist_commit_decided(
        self,
        client: &Client,
    ) -> Result<
        ActiveReblitBootCommitDecisionHandoff<
            'plan,
            'inventory,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootCommitDecisionError,
    > {
        let retained_plan = self.completed.revalidate_against(client)
            .map_err(|source| ActiveReblitBootCommitDecisionError::PreAdvance(
                ActiveReblitBootPostCompletionValidationError::CompletedStagedEvidence {
                    checkpoint: "commit-decision admission",
                    source,
                },
            ))?
            .plan();
        validate_completed_terminal_sandwich(
            &self.completed,
            client,
            retained_plan,
            self.publication_count,
            self.published_count,
            self.already_exact_count,
            self.replaced_count,
            &self.evidence,
            "immediate pre-commit-decision",
        )
        .map_err(ActiveReblitBootCommitDecisionError::PreAdvance)?;

        after_active_reblit_commit_decision_terminal_validation();
        validate_completed_terminal_sandwich(
            &self.completed,
            client,
            retained_plan,
            self.publication_count,
            self.published_count,
            self.already_exact_count,
            self.replaced_count,
            &self.evidence,
            "final pre-commit-decision",
        )
        .map_err(ActiveReblitBootCommitDecisionError::PreAdvance)?;

        let Self {
            completed,
            database_outcome,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            evidence,
        } = self;
        let receipt_fingerprint = completed.receipt_fingerprint();
        let final_validation = ActiveReblitBootCommitDecisionFinalValidation {
            callback: Box::new(|| {
                validate_exact_terminal_evidence_snapshot(
                    retained_plan,
                    receipt_fingerprint,
                    publication_count,
                    published_count,
                    already_exact_count,
                    replaced_count,
                    &evidence,
                    "bound commit-decision advance",
                )
                .map_err(|source| {
                    ActiveReblitBootPostCompletionValidationError::TerminalEvidence {
                        checkpoint: "bound commit-decision advance",
                        source,
                    }
                })?;
                Ok(retained_plan.input_deadline())
            }),
        };
        let seal = ActiveReblitBootSyncCommitDecisionSeal { _private: () };
        let committed = completed
            .persist_commit_decided(client, seal, final_validation)
            .map_err(ActiveReblitBootCommitDecisionError::Persistence)?;
        let handoff = ActiveReblitBootCommitDecisionHandoff {
            committed,
            database_outcome,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            evidence,
        };
        validate_committed_terminal_sandwich(&handoff, client, retained_plan)
            .map_err(ActiveReblitBootCommitDecisionError::PostAdvance)?;
        Ok(handoff)
    }
}

fn validate_committed_terminal_sandwich<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    handoff: &ActiveReblitBootCommitDecisionHandoff<
        'plan,
        'inventory,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    client: &Client,
    retained_plan: &'plan BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
) -> Result<(), ActiveReblitBootCommitDecisionPostAdvanceError>
where
    'input: 'plan,
{
    handoff.committed.revalidate_against(client)
        .map_err(ActiveReblitBootCommitDecisionPostAdvanceError::CommittedEvidence)?;
    if !std::ptr::eq(handoff.committed.plan(), retained_plan) {
        return Err(ActiveReblitBootCommitDecisionPostAdvanceError::PlanMismatch);
    }
    validate_exact_terminal_evidence_snapshot(
        retained_plan,
        handoff.receipt_fingerprint(),
        handoff.publication_count,
        handoff.published_count,
        handoff.already_exact_count,
        handoff.replaced_count,
        &handoff.evidence,
        "post-commit-decision handoff",
    )
    .map_err(ActiveReblitBootCommitDecisionPostAdvanceError::TerminalEvidence)?;
    handoff.committed.revalidate_against(client)
        .map_err(ActiveReblitBootCommitDecisionPostAdvanceError::CommittedEvidence)
}

#[path = "commit_decision/commit_cleanup.rs"]
mod commit_cleanup;
pub(in crate::client) use commit_cleanup::{
    ActiveReblitBootCommitCleanupCompleteHandoff,
    ActiveReblitBootCommitCleanupError,
    ActiveReblitBootCommitCleanupPostAdvanceError,
};

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootCommitDecisionError {
    #[error("revalidate exact completed terminal evidence immediately before commit decision")]
    PreAdvance(#[source] ActiveReblitBootPostCompletionValidationError),
    #[error("persist exact ActiveReblit BootSyncComplete to CommitDecided handoff")]
    Persistence(#[source] CompletedStagedActiveReblitCommitDecisionError),
    #[error("revalidate exact committed terminal handoff; durable journal is CommitDecided")]
    PostAdvance(#[source] ActiveReblitBootCommitDecisionPostAdvanceError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootCommitDecisionPostAdvanceError {
    #[error("revalidate retained committed staging evidence")]
    CommittedEvidence(
        #[source]
        CommittedStagedActiveReblitBootSyncValidationError,
    ),
    #[error("the committed staging authority returned a different retained plan")]
    PlanMismatch,
    #[error("revalidate exact terminal output and topology evidence")]
    TerminalEvidence(
        #[source]
        ActiveReblitBootTerminalEvidenceValidationError,
    ),
}

#[cfg(test)]
std::thread_local! {
    static AFTER_TERMINAL_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_after_active_reblit_commit_decision_terminal_validation(
    hook: impl FnOnce() + 'static,
) {
    AFTER_TERMINAL_VALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn assert_after_active_reblit_commit_decision_terminal_validation_hook_consumed() {
    AFTER_TERMINAL_VALIDATION.with(|slot| {
        assert!(slot.borrow().is_none(), "commit-decision terminal hook was not consumed");
    });
}

#[cfg(test)]
fn after_active_reblit_commit_decision_terminal_validation() {
    AFTER_TERMINAL_VALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_active_reblit_commit_decision_terminal_validation() {}
