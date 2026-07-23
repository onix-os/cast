//! Terminal-evidence wrapper for live ActiveReblit commit cleanup.

use thiserror::Error;

use crate::{
    boot_publication::BootPublicationReceiptFingerprint,
    client::{
        Client,
        active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_boot_publication_preflight::ActiveReblitCommitCleanupSeal,
        active_reblit_boot_sync_staging::{
            CommitCleanupCompleteStagedActiveReblitBootSync,
            CommitCleanupCompleteStagedActiveReblitBootSyncValidationError,
            CommittedStagedActiveReblitCommitCleanupError,
        },
    },
    db::state::BootPublicationReceiptPromotionOutcome,
    transition_journal::TransitionRecord,
};

use super::{
    ActiveReblitBootTerminalEvidenceValidationError,
    ActiveReblitBootCommitDecisionHandoff,
    ActiveReblitBootCommitDecisionPostAdvanceError,
    ValidatedActiveReblitBootPublicationEffect,
    validate_committed_terminal_sandwich,
    validate_exact_terminal_evidence_snapshot,
};

/// Exact durable `CommitCleanupComplete` handoff retaining the writer
/// reservation, original plan, desired inventory, promoted receipt, and all
/// terminal publication evidence. It intentionally implements neither
/// `Clone` nor `Copy` and exposes no raw-parts escape hatch.
#[must_use = "cleanup-complete authority must enter finalization or be deliberately discarded"]
pub(in crate::client) struct ActiveReblitBootCommitCleanupCompleteHandoff<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
> {
    cleaned: CommitCleanupCompleteStagedActiveReblitBootSync<
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
    for ActiveReblitBootCommitCleanupCompleteHandoff<'_, '_, '_, '_, '_, '_, '_, '_>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActiveReblitBootCommitCleanupCompleteHandoff")
            .field("record", self.cleaned.record())
            .field("receipt_fingerprint", &self.receipt_fingerprint())
            .field("database_outcome", &self.database_outcome)
            .field("publication_count", &self.publication_count)
            .finish_non_exhaustive()
    }
}

impl ActiveReblitBootCommitCleanupCompleteHandoff<'_, '_, '_, '_, '_, '_, '_, '_> {
    pub(in crate::client) const fn record(&self) -> &TransitionRecord {
        self.cleaned.record()
    }

    pub(in crate::client) const fn receipt_fingerprint(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.cleaned.receipt_fingerprint()
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
    ActiveReblitBootCommitDecisionHandoff<
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
    /// Consume the exact generation-13 handoff through Apply cleanup and the
    /// sole durable generation-14 successor. This does not enter finalization.
    pub(in crate::client) fn persist_commit_cleanup_complete(
        self,
        client: &Client,
    ) -> Result<
        ActiveReblitBootCommitCleanupCompleteHandoff<
            'plan,
            'inventory,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootCommitCleanupError,
    > {
        self.committed
            .revalidate_against(client)
            .map_err(|source| {
                ActiveReblitBootCommitCleanupError::PreCleanup(
                    ActiveReblitBootCommitDecisionPostAdvanceError::CommittedEvidence(source),
                )
            })?;
        let retained_plan = self.committed.plan();
        validate_committed_terminal_sandwich(&self, client, retained_plan)
            .map_err(ActiveReblitBootCommitCleanupError::PreCleanup)?;

        let ActiveReblitBootCommitDecisionHandoff {
            committed,
            database_outcome,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            evidence,
        } = self;
        let seal = ActiveReblitCommitCleanupSeal { _private: () };
        let cleaned = committed
            .persist_commit_cleanup_complete(client, seal)
            .map_err(ActiveReblitBootCommitCleanupError::Persistence)?;
        let handoff = ActiveReblitBootCommitCleanupCompleteHandoff {
            cleaned,
            database_outcome,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            evidence,
        };
        validate_cleanup_complete_terminal_sandwich(&handoff, client, retained_plan)
            .map_err(ActiveReblitBootCommitCleanupError::PostCleanup)?;
        Ok(handoff)
    }
}

fn validate_cleanup_complete_terminal_sandwich<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    handoff: &ActiveReblitBootCommitCleanupCompleteHandoff<
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
) -> Result<(), ActiveReblitBootCommitCleanupPostAdvanceError>
where
    'input: 'plan,
{
    handoff
        .cleaned
        .revalidate_against(client)
        .map_err(ActiveReblitBootCommitCleanupPostAdvanceError::CleanupCompleteEvidence)?;
    if !std::ptr::eq(handoff.cleaned.plan(), retained_plan) {
        return Err(ActiveReblitBootCommitCleanupPostAdvanceError::PlanMismatch);
    }
    validate_exact_terminal_evidence_snapshot(
        retained_plan,
        handoff.receipt_fingerprint(),
        handoff.publication_count,
        handoff.published_count,
        handoff.already_exact_count,
        handoff.replaced_count,
        &handoff.evidence,
        "post-commit-cleanup handoff",
    )
    .map_err(ActiveReblitBootCommitCleanupPostAdvanceError::TerminalEvidence)?;
    handoff
        .cleaned
        .revalidate_against(client)
        .map_err(ActiveReblitBootCommitCleanupPostAdvanceError::CleanupCompleteEvidence)
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootCommitCleanupError {
    #[error("revalidate exact committed terminal handoff immediately before cleanup")]
    PreCleanup(#[source] ActiveReblitBootCommitDecisionPostAdvanceError),
    #[error("perform exact ActiveReblit Apply cleanup and persist CommitCleanupComplete")]
    Persistence(#[source] CommittedStagedActiveReblitCommitCleanupError),
    #[error("revalidate exact cleanup-complete terminal handoff; durable journal is CommitCleanupComplete")]
    PostCleanup(#[source] ActiveReblitBootCommitCleanupPostAdvanceError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootCommitCleanupPostAdvanceError {
    #[error("revalidate retained cleanup-complete staging evidence")]
    CleanupCompleteEvidence(
        #[source]
        CommitCleanupCompleteStagedActiveReblitBootSyncValidationError,
    ),
    #[error("the cleanup-complete authority returned a different retained plan")]
    PlanMismatch,
    #[error("revalidate exact terminal output and topology evidence")]
    TerminalEvidence(#[source] ActiveReblitBootTerminalEvidenceValidationError),
}
