//! Live terminal finalization of one exact sealed `Complete` handoff.

use thiserror::Error;

use crate::{
    boot_publication::BootPublicationReceiptFingerprint,
    client::{
        Client,
        active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_boot_publication_preflight::ActiveReblitBootCompleteFinalizationSeal,
        active_reblit_boot_sync_staging::{
            CompleteStagedActiveReblitFinalizationError,
            CompleteStagedActiveReblitBootSyncValidationError,
            FinalizedStagedActiveReblitBootSync,
            FinalizedStagedActiveReblitBootSyncValidationError,
        },
        active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
    },
    db::state::{
        BootPublicationReceiptPromotionOutcome, BootPublicationReceiptStageOutcome,
    },
    transition_journal::TransitionRecord,
};

use super::{
    ActiveReblitBootCompleteHandoff, ValidatedActiveReblitBootPublicationEffect,
};

/// Clean terminal handoff produced only after the exact retained generation-15
/// journal binding has been deleted and fully revalidated. It retains every
/// live coordinator capability and all historical terminal output evidence.
#[must_use = "clean terminal authority must be returned or deliberately discarded"]
pub(in crate::client) struct ActiveReblitBootFinalizedHandoff<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
> {
    finalized: FinalizedStagedActiveReblitBootSync<
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
    for ActiveReblitBootFinalizedHandoff<'_, '_, '_, '_, '_, '_, '_, '_>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActiveReblitBootFinalizedHandoff")
            .field("complete_record", self.finalized.complete_record())
            .field("receipt_fingerprint", &self.receipt_fingerprint())
            .field("database_outcome", &self.database_outcome)
            .field("publication_count", &self.publication_count)
            .finish_non_exhaustive()
    }
}

impl ActiveReblitBootFinalizedHandoff<'_, '_, '_, '_, '_, '_, '_, '_> {
    pub(in crate::client) const fn complete_record(&self) -> &TransitionRecord {
        self.finalized.complete_record()
    }

    pub(in crate::client) const fn receipt_fingerprint(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.finalized.receipt_fingerprint()
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
        self.finalized.inventory()
    }

    pub(in crate::client) const fn staging_outcome(
        &self,
    ) -> BootPublicationReceiptStageOutcome {
        self.finalized.staging_outcome()
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
    ActiveReblitBootCompleteHandoff<
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
    /// Consume the already-sealed generation-15 handoff through the sole
    /// record-bound terminal deletion. The inherited plan deadline is not a
    /// finalization gate; all terminal output evidence moves through unchanged.
    pub(in crate::client) fn finalize(
        self,
        client: &Client,
    ) -> Result<
        ActiveReblitBootFinalizedHandoff<
            'plan,
            'inventory,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootFinalizationError,
    > {
        self.completed
            .revalidate_against(client)
            .map_err(ActiveReblitBootFinalizationError::CompleteEvidence)?;

        let ActiveReblitBootCompleteHandoff {
            completed,
            database_outcome,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            evidence,
        } = self;
        let seal = ActiveReblitBootCompleteFinalizationSeal { _private: () };
        let finalized = completed
            .finalize(client, seal)
            .map_err(ActiveReblitBootFinalizationError::Finalization)?;
        let handoff = ActiveReblitBootFinalizedHandoff {
            finalized,
            database_outcome,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            evidence,
        };
        handoff
            .finalized
            .revalidate_against(client)
            .map_err(ActiveReblitBootFinalizationError::FinalizedEvidence)?;
        Ok(handoff)
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootFinalizationError {
    #[error("revalidate exact retained generation-15 Complete handoff")]
    CompleteEvidence(#[source] CompleteStagedActiveReblitBootSyncValidationError),
    #[error("perform exact same-store generation-15 terminal finalization")]
    Finalization(#[source] CompleteStagedActiveReblitFinalizationError),
    #[error("revalidate sealed clean terminal handoff")]
    FinalizedEvidence(#[source] FinalizedStagedActiveReblitBootSyncValidationError),
}
