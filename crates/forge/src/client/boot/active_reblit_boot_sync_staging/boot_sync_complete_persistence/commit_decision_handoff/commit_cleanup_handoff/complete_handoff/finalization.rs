//! Sealed clean handoff after one exact live terminal journal deletion.

use thiserror::Error;

use crate::{
    Installation,
    boot_publication::{BootPublicationReceiptFingerprint, CanonicalBootPublicationReceipt},
    client::{
        Client, CoordinatorActiveStateReservation,
        active_reblit_boot_publication_preflight::ActiveReblitBootCompleteFinalizationSeal,
        active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
        startup_gate::{self, CleanSystemStartup},
        startup_reconciliation::{
            ActiveReblitCompleteFinalizationAuthority, ActiveReblitCompleteFinalizationAuthorityError,
        },
        startup_recovery::{ActiveReblitCompleteFinalizationError, finalize_active_reblit_complete},
    },
    db::state::{BootPublicationReceiptPromotionError, BootPublicationReceiptStageOutcome, Database},
    installation, state,
    transition_journal::{CodecError, Phase, TransitionRecord},
};

use super::{
    CompleteStagedActiveReblitBootSync, exact_live_options, receipt_pair,
};

const ACTIVE_REBLIT_COMPLETE_GENERATION: u64 = 15;

/// Exact terminal state after the sole bound journal deletion and its complete
/// post-delete proof. The original plan, inventory, promoted receipt, locked
/// journal store, and writer reservation remain continuously owned.
#[must_use = "clean terminal authority must be returned or deliberately discarded"]
pub(in crate::client) struct FinalizedStagedActiveReblitBootSync<'plan, 'inventory, Plan> {
    complete_record: TransitionRecord,
    receipt: CanonicalBootPublicationReceipt,
    plan: &'plan Plan,
    inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
    staging_outcome: BootPublicationReceiptStageOutcome,
    database: Database,
    installation: Installation,
    clean_startup: CleanSystemStartup,
    active_state_reservation: CoordinatorActiveStateReservation,
}

impl<Plan> std::fmt::Debug for FinalizedStagedActiveReblitBootSync<'_, '_, Plan> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FinalizedStagedActiveReblitBootSync")
            .field("complete_record", &self.complete_record)
            .field("receipt", &self.receipt)
            .field("staging_outcome", &self.staging_outcome)
            .finish_non_exhaustive()
    }
}

impl<'plan, 'inventory, Plan> FinalizedStagedActiveReblitBootSync<'plan, 'inventory, Plan> {
    pub(in crate::client) const fn complete_record(&self) -> &TransitionRecord {
        &self.complete_record
    }

    pub(in crate::client) const fn receipt_fingerprint(&self) -> BootPublicationReceiptFingerprint {
        self.receipt.fingerprint()
    }

    pub(in crate::client) const fn plan(&self) -> &'plan Plan {
        self.plan
    }

    pub(in crate::client) const fn inventory(&self) -> &'inventory PreparedActiveReblitDesiredPublicationInventory {
        self.inventory
    }

    pub(in crate::client) const fn staging_outcome(&self) -> BootPublicationReceiptStageOutcome {
        self.staging_outcome
    }

    pub(in crate::client) fn revalidate_against(
        &self,
        client: &Client,
    ) -> Result<(), FinalizedStagedActiveReblitBootSyncValidationError> {
        if !self.database.same_instance(&client.state_db)
            || !std::ptr::eq(self.installation.root_directory(), client.installation.root_directory())
        {
            return Err(FinalizedStagedActiveReblitBootSyncValidationError::ClientCapabilityMismatch);
        }
        let pair = receipt_pair(&self.receipt);
        if !crate::client::active_reblit_boot_sync_staging::supports_boot_sync(self.complete_record.operation)
            || self.complete_record.phase != Phase::Complete
            || !crate::client::active_reblit_boot_sync_staging::boot_tail_generation_is_exact(&self.complete_record)
            || !exact_live_options(&self.complete_record)
            || self.complete_record.rollback.is_some()
            || !crate::client::active_reblit_boot_sync_staging::boot_tail_identity_is_exact(&self.complete_record)
            || self.complete_record.boot_publication_receipt_correlation()? != Some(pair)
        {
            return Err(FinalizedStagedActiveReblitBootSyncValidationError::UnexpectedRecord);
        }

        let _clean_startup = &self.clean_startup;
        self.installation.revalidate_mutable_namespace()?;
        self.database.require_promoted_boot_publication_receipt(&self.receipt)?;
        let expected = state::Id::from(
            self.complete_record
                .candidate
                .id
                .expect("checked exact live Complete state"),
        );
        // A NewState published this selection during the transaction just
        // finalized, so it cannot be compared against discovery.
        let active_state = match self.complete_record.operation {
            crate::transition_journal::Operation::ActiveReblit => self
                .active_state_reservation
                .capture_for_startup_recovery(&self.installation)?,
            _ => self
                .active_state_reservation
                .capture_for_forward_boot_completion(&self.installation, expected)?,
        };
        if active_state.active() != Some(expected) {
            return Err(FinalizedStagedActiveReblitBootSyncValidationError::ActiveSelectionChanged);
        }
        match self.complete_record.operation {
            crate::transition_journal::Operation::ActiveReblit => active_state.revalidate(&self.installation)?,
            _ => active_state.revalidate_for_forward_boot_completion(&self.installation)?,
        }
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

impl<'plan, 'inventory, Plan> CompleteStagedActiveReblitBootSync<'plan, 'inventory, Plan> {
    /// Consume the retained generation-15 binding through the existing
    /// same-store terminal finalizer. No reopen, retry, or other mutation is
    /// introduced by the live adapter.
    pub(in crate::client) fn finalize(
        self,
        client: &Client,
        seal: ActiveReblitBootCompleteFinalizationSeal,
    ) -> Result<FinalizedStagedActiveReblitBootSync<'plan, 'inventory, Plan>, CompleteStagedActiveReblitFinalizationError>
    {
        self.revalidate_against(client)
            .map_err(CompleteStagedActiveReblitFinalizationError::CompleteEvidence)?;

        let CompleteStagedActiveReblitBootSync {
            commit_cleanup_complete_record: _,
            record: complete_record,
            record_binding,
            receipt,
            plan,
            inventory,
            staging_outcome,
            journal,
            database,
            installation,
            active_state_reservation,
        } = self;
        // A first install reclaimed no predecessor wrapper, so there is no
        // completed-cleanup namespace to prove before the terminal deletion.
        let journal = if crate::client::startup_reconciliation::new_state_boot_tail_terminal_is_admissible(
            &complete_record,
        ) {
            crate::client::startup_recovery::finalize_new_state_boot_tail_terminal(
                &installation,
                &database,
                journal,
                &complete_record,
                record_binding,
            )
            .map_err(CompleteStagedActiveReblitFinalizationError::NewStateTerminal)?
        } else {
            let authority = ActiveReblitCompleteFinalizationAuthority::capture_retained_binding(
                seal,
                &installation,
                &journal,
                &database,
                &active_state_reservation,
                &complete_record,
                record_binding,
            )
            .map_err(CompleteStagedActiveReblitFinalizationError::Authority)?;
            finalize_active_reblit_complete(journal, authority)
                .map_err(CompleteStagedActiveReblitFinalizationError::Finalization)?
        };
        let clean_startup =
            CleanSystemStartup::admit_clean_after_terminal_finalization(&installation, &database, journal)
                .map_err(CompleteStagedActiveReblitFinalizationError::CleanAdmission)?;

        let finalized = FinalizedStagedActiveReblitBootSync {
            complete_record,
            receipt,
            plan,
            inventory,
            staging_outcome,
            database,
            installation,
            clean_startup,
            active_state_reservation,
        };
        finalized
            .revalidate_against(client)
            .map_err(CompleteStagedActiveReblitFinalizationError::FinalizedEvidence)?;
        Ok(finalized)
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum CompleteStagedActiveReblitFinalizationError {
    #[error("revalidate exact retained generation-15 Complete handoff")]
    CompleteEvidence(#[source] super::CompleteStagedActiveReblitBootSyncValidationError),
    #[error("admit retained exact generation-15 terminal finalization authority")]
    Authority(#[source] ActiveReblitCompleteFinalizationAuthorityError),
    #[error("delete the journal-only first-install NewState terminal record")]
    NewStateTerminal(#[source] crate::client::startup_recovery::NewStateCommitCleanupPersistenceError),
    #[error("delete the exact retained generation-15 terminal journal")]
    Finalization(#[source] ActiveReblitCompleteFinalizationError),
    #[error("admit clean startup on the same terminal-finalization journal store")]
    CleanAdmission(#[source] startup_gate::Error),
    #[error("revalidate clean terminal handoff after exact journal deletion")]
    FinalizedEvidence(#[source] FinalizedStagedActiveReblitBootSyncValidationError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum FinalizedStagedActiveReblitBootSyncValidationError {
    #[error("the finalized handoff belongs to a different client capability set")]
    ClientCapabilityMismatch,
    #[error("revalidate retained installation namespace")]
    Installation(#[from] installation::Error),
    #[error("decode exact retained Complete record evidence")]
    Record(#[from] CodecError),
    #[error("the retained record evidence is not the exact live Complete route")]
    UnexpectedRecord,
    #[error("revalidate exact promoted boot-publication receipt")]
    Receipt(#[from] BootPublicationReceiptPromotionError),
    #[error("prove exact retained active-state selection")]
    ActiveState(#[from] crate::client::Error),
    #[error("the selected active state changed")]
    ActiveSelectionChanged,
}
