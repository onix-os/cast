//! Terminal-evidence wrapper for live ActiveReblit `Complete` roll-forward.

use thiserror::Error;

use crate::{
    boot_publication::BootPublicationReceiptFingerprint,
    client::{
        Client,
        active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_boot_publication_preflight::ActiveReblitCommitCleanupCompleteSeal,
        active_reblit_boot_sync_staging::{
            CommitCleanupCompleteStagedActiveReblitCompleteError,
            CompleteStagedActiveReblitBootSync,
            CompleteStagedActiveReblitBootSyncValidationError,
        },
        active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
    },
    db::state::{
        BootPublicationReceiptPromotionOutcome, BootPublicationReceiptStageOutcome,
    },
    transition_journal::TransitionRecord,
};

use super::{
    ActiveReblitBootCommitCleanupCompleteHandoff,
    ActiveReblitBootCommitCleanupPostAdvanceError,
    ActiveReblitBootTerminalEvidenceValidationError,
    ValidatedActiveReblitBootPublicationEffect,
    validate_cleanup_complete_terminal_sandwich,
    validate_exact_terminal_evidence_snapshot,
};

/// Exact durable generation-15 `Complete` handoff retaining the writer
/// reservation, original plan and inventory, promoted receipt, and terminal
/// publication evidence. It grants no finalization or journal deletion.
#[must_use = "Complete authority must enter later finalization or be deliberately discarded"]
pub(in crate::client) struct ActiveReblitBootCompleteHandoff<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
> {
    completed: CompleteStagedActiveReblitBootSync<
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
    for ActiveReblitBootCompleteHandoff<'_, '_, '_, '_, '_, '_, '_, '_>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActiveReblitBootCompleteHandoff")
            .field("record", self.completed.record())
            .field("receipt_fingerprint", &self.receipt_fingerprint())
            .field("database_outcome", &self.database_outcome)
            .field("publication_count", &self.publication_count)
            .finish_non_exhaustive()
    }
}

impl ActiveReblitBootCompleteHandoff<'_, '_, '_, '_, '_, '_, '_, '_> {
    pub(in crate::client) const fn record(&self) -> &TransitionRecord {
        self.completed.record()
    }

    pub(in crate::client) const fn receipt_fingerprint(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.completed.receipt_fingerprint()
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

    pub(in crate::client) const fn inventory(
        &self,
    ) -> &PreparedActiveReblitDesiredPublicationInventory {
        self.completed.inventory()
    }

    pub(in crate::client) const fn staging_outcome(
        &self,
    ) -> BootPublicationReceiptStageOutcome {
        self.completed.staging_outcome()
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
    ActiveReblitBootCommitCleanupCompleteHandoff<
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
    /// Consume exact generation-14 authority through the sole journal-only
    /// generation-15 successor. This does not enter finalization.
    pub(in crate::client) fn persist_complete(
        self,
        client: &Client,
    ) -> Result<
        ActiveReblitBootCompleteHandoff<
            'plan,
            'inventory,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootCompleteError,
    > {
        self.cleaned
            .revalidate_against(client)
            .map_err(|source| {
                ActiveReblitBootCompleteError::PreAdvance(
                    ActiveReblitBootCommitCleanupPostAdvanceError::CleanupCompleteEvidence(source),
                )
            })?;
        let retained_plan = self.cleaned.plan();
        validate_cleanup_complete_terminal_sandwich(&self, client, retained_plan)
            .map_err(ActiveReblitBootCompleteError::PreAdvance)?;

        let ActiveReblitBootCommitCleanupCompleteHandoff {
            cleaned,
            database_outcome,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            evidence,
        } = self;
        let seal = ActiveReblitCommitCleanupCompleteSeal { _private: () };
        let completed = cleaned
            .persist_complete(client, seal)
            .map_err(ActiveReblitBootCompleteError::Persistence)?;
        let handoff = ActiveReblitBootCompleteHandoff {
            completed,
            database_outcome,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            evidence,
        };
        validate_complete_terminal_sandwich(&handoff, client, retained_plan)
            .map_err(ActiveReblitBootCompleteError::PostAdvance)?;
        Ok(handoff)
    }
}

fn validate_complete_terminal_sandwich<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    handoff: &ActiveReblitBootCompleteHandoff<
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
) -> Result<(), ActiveReblitBootCompletePostAdvanceError>
where
    'input: 'plan,
{
    handoff
        .completed
        .revalidate_against(client)
        .map_err(ActiveReblitBootCompletePostAdvanceError::CompleteEvidence)?;
    if !std::ptr::eq(handoff.completed.plan(), retained_plan) {
        return Err(ActiveReblitBootCompletePostAdvanceError::PlanMismatch);
    }
    validate_exact_terminal_evidence_snapshot(
        retained_plan,
        handoff.receipt_fingerprint(),
        handoff.publication_count,
        handoff.published_count,
        handoff.already_exact_count,
        handoff.replaced_count,
        &handoff.evidence,
        "post-Complete handoff",
    )
    .map_err(ActiveReblitBootCompletePostAdvanceError::TerminalEvidence)?;
    handoff
        .completed
        .revalidate_against(client)
        .map_err(ActiveReblitBootCompletePostAdvanceError::CompleteEvidence)
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootCompleteError {
    #[error("revalidate exact cleanup-complete terminal handoff before Complete")]
    PreAdvance(#[source] ActiveReblitBootCommitCleanupPostAdvanceError),
    #[error("persist exact ActiveReblit CommitCleanupComplete to Complete handoff")]
    Persistence(#[source] CommitCleanupCompleteStagedActiveReblitCompleteError),
    #[error("revalidate exact Complete terminal handoff; durable journal is Complete")]
    PostAdvance(#[source] ActiveReblitBootCompletePostAdvanceError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootCompletePostAdvanceError {
    #[error("revalidate retained Complete staging evidence")]
    CompleteEvidence(#[source] CompleteStagedActiveReblitBootSyncValidationError),
    #[error("the Complete staging authority returned a different retained plan")]
    PlanMismatch,
    #[error("revalidate exact terminal output and topology evidence")]
    TerminalEvidence(#[source] ActiveReblitBootTerminalEvidenceValidationError),
}
