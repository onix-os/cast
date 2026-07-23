//! Sealed generation-15 handoff after live cleanup completion roll-forward.

use thiserror::Error;

use crate::{
    Installation,
    boot_publication::{
        BootPublicationReceiptFingerprint, CanonicalBootPublicationReceipt,
    },
    client::{
        Client, CoordinatorActiveStateReservation,
        active_reblit_boot_publication_preflight::ActiveReblitCommitCleanupCompleteSeal,
        active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
        startup_reconciliation::{
            ActiveReblitCommitCleanupCompleteAuthority,
            ActiveReblitCommitCleanupCompleteAuthorityError,
        },
        startup_recovery::{
            ActiveReblitCommitCleanupCompletePersistenceError,
            persist_active_reblit_commit_cleanup_complete_to_complete_retaining_binding,
        },
    },
    db::state::{
        BootPublicationReceiptPromotionError, BootPublicationReceiptStageOutcome, Database,
    },
    installation,
    transition_journal::{
        CodecError, Operation, Phase, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    CommitCleanupCompleteStagedActiveReblitBootSync, exact_live_options, receipt_pair,
};

const ACTIVE_REBLIT_COMMIT_CLEANUP_COMPLETE_GENERATION: u64 = 14;
const ACTIVE_REBLIT_COMPLETE_GENERATION: u64 = 15;

/// Exact durable `Complete` state retaining every live coordinator capability.
/// It grants no terminal journal deletion or finalization authority.
#[must_use = "complete authority must enter a later finalization slice or be deliberately discarded"]
pub(in crate::client) struct CompleteStagedActiveReblitBootSync<
    'plan,
    'inventory,
    Plan,
> {
    commit_cleanup_complete_record: TransitionRecord,
    record: TransitionRecord,
    record_binding: TransitionJournalRecordBinding,
    receipt: CanonicalBootPublicationReceipt,
    plan: &'plan Plan,
    inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
    staging_outcome: BootPublicationReceiptStageOutcome,
    journal: TransitionJournalStore,
    database: Database,
    installation: Installation,
    active_state_reservation: CoordinatorActiveStateReservation,
}

impl<Plan> std::fmt::Debug for CompleteStagedActiveReblitBootSync<'_, '_, Plan> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompleteStagedActiveReblitBootSync")
            .field("record", &self.record)
            .field("receipt", &self.receipt)
            .field("staging_outcome", &self.staging_outcome)
            .finish_non_exhaustive()
    }
}

impl<'plan, 'inventory, Plan> CompleteStagedActiveReblitBootSync<'plan, 'inventory, Plan> {
    pub(in crate::client) const fn record(&self) -> &TransitionRecord {
        &self.record
    }

    pub(in crate::client) const fn receipt_fingerprint(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.receipt.fingerprint()
    }

    pub(in crate::client) const fn plan(&self) -> &'plan Plan {
        self.plan
    }

    pub(in crate::client) const fn inventory(
        &self,
    ) -> &'inventory PreparedActiveReblitDesiredPublicationInventory {
        self.inventory
    }

    pub(in crate::client) const fn staging_outcome(
        &self,
    ) -> BootPublicationReceiptStageOutcome {
        self.staging_outcome
    }

    pub(in crate::client) fn revalidate_against(
        &self,
        client: &Client,
    ) -> Result<(), CompleteStagedActiveReblitBootSyncValidationError> {
        if !self.database.same_instance(&client.state_db)
            || !std::ptr::eq(
                self.installation.root_directory(),
                client.installation.root_directory(),
            )
        {
            return Err(
                CompleteStagedActiveReblitBootSyncValidationError::ClientCapabilityMismatch,
            );
        }
        self.installation.revalidate_mutable_namespace()?;
        let cast = self.installation.retained_mutable_cast_directory()?;
        if !self.journal.has_record_store_binding(&self.record_binding)
            || !self
                .journal
                .has_record_binding(cast, &self.record_binding, &self.record)?
        {
            return Err(CompleteStagedActiveReblitBootSyncValidationError::BindingChanged);
        }
        let expected = self.commit_cleanup_complete_record.forward_successor(None)?;
        let pair = receipt_pair(&self.receipt);
        if expected != self.record
            || self.commit_cleanup_complete_record.generation
                != ACTIVE_REBLIT_COMMIT_CLEANUP_COMPLETE_GENERATION
            || self.record.generation != ACTIVE_REBLIT_COMPLETE_GENERATION
            || self.commit_cleanup_complete_record.operation != Operation::ActiveReblit
            || self.commit_cleanup_complete_record.phase != Phase::CommitCleanupComplete
            || self.record.operation != Operation::ActiveReblit
            || self.record.phase != Phase::Complete
            || !exact_live_options(&self.commit_cleanup_complete_record)
            || !exact_live_options(&self.record)
            || self.commit_cleanup_complete_record.rollback.is_some()
            || self.record.rollback.is_some()
            || !same_nonempty_candidate_and_previous(&self.commit_cleanup_complete_record)
            || !same_nonempty_candidate_and_previous(&self.record)
            || self
                .commit_cleanup_complete_record
                .boot_publication_receipt_correlation()?
                != Some(pair)
            || self.record.boot_publication_receipt_correlation()? != Some(pair)
        {
            return Err(CompleteStagedActiveReblitBootSyncValidationError::UnexpectedRecord);
        }
        self.database
            .require_promoted_boot_publication_receipt(&self.receipt)?;
        self.installation.revalidate_mutable_namespace()?;
        if !self
            .journal
            .has_record_binding(cast, &self.record_binding, &self.record)?
        {
            return Err(CompleteStagedActiveReblitBootSyncValidationError::BindingChanged);
        }
        Ok(())
    }
}

impl<'plan, 'inventory, Plan>
    CommitCleanupCompleteStagedActiveReblitBootSync<'plan, 'inventory, Plan>
{
    /// Consume exact live generation-14 authority through the mandatory
    /// journal-only `Complete` advance. No deadline or finalization gate is
    /// introduced here.
    pub(in crate::client) fn persist_complete(
        self,
        client: &Client,
        seal: ActiveReblitCommitCleanupCompleteSeal,
    ) -> Result<
        CompleteStagedActiveReblitBootSync<'plan, 'inventory, Plan>,
        CommitCleanupCompleteStagedActiveReblitCompleteError,
    > {
        self.revalidate_against(client)
            .map_err(CommitCleanupCompleteStagedActiveReblitCompleteError::CleanupCompleteEvidence)?;

        let CommitCleanupCompleteStagedActiveReblitBootSync {
            commit_decided_record: _,
            record: commit_cleanup_complete_record,
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

        let authority = ActiveReblitCommitCleanupCompleteAuthority::capture_retained_binding(
            seal,
            &installation,
            &journal,
            &database,
            &active_state_reservation,
            &commit_cleanup_complete_record,
            record_binding,
        )
        .map_err(CommitCleanupCompleteStagedActiveReblitCompleteError::Authority)?;
        let (journal, record, record_binding) =
            persist_active_reblit_commit_cleanup_complete_to_complete_retaining_binding(
                journal, authority,
            )
            .map_err(CommitCleanupCompleteStagedActiveReblitCompleteError::Persistence)?;

        let completed = CompleteStagedActiveReblitBootSync {
            commit_cleanup_complete_record,
            record,
            record_binding,
            receipt,
            plan,
            inventory,
            staging_outcome,
            journal,
            database,
            installation,
            active_state_reservation,
        };
        completed
            .revalidate_against(client)
            .map_err(CommitCleanupCompleteStagedActiveReblitCompleteError::CompleteEvidence)?;
        Ok(completed)
    }
}

fn same_nonempty_candidate_and_previous(record: &TransitionRecord) -> bool {
    record.candidate.id.is_some() && record.candidate.id == record.previous.id
}

#[derive(Debug, Error)]
pub(in crate::client) enum CommitCleanupCompleteStagedActiveReblitCompleteError {
    #[error("revalidate exact retained generation-14 cleanup-complete handoff")]
    CleanupCompleteEvidence(
        #[source]
        super::CommitCleanupCompleteStagedActiveReblitBootSyncValidationError,
    ),
    #[error("admit retained exact generation-14 Finish authority")]
    Authority(#[source] ActiveReblitCommitCleanupCompleteAuthorityError),
    #[error("persist exact generation-15 Complete successor")]
    Persistence(#[source] ActiveReblitCommitCleanupCompletePersistenceError),
    #[error("revalidate exact retained generation-15 Complete handoff")]
    CompleteEvidence(#[source] CompleteStagedActiveReblitBootSyncValidationError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum CompleteStagedActiveReblitBootSyncValidationError {
    #[error("the Complete handoff belongs to a different client capability set")]
    ClientCapabilityMismatch,
    #[error("revalidate retained installation namespace")]
    Installation(#[from] installation::Error),
    #[error("revalidate exact retained Complete journal binding")]
    Journal(#[from] StorageError),
    #[error("the retained Complete journal binding changed")]
    BindingChanged,
    #[error("derive or decode exact retained Complete record")]
    Record(#[from] CodecError),
    #[error("the retained record is not the sole exact Complete successor")]
    UnexpectedRecord,
    #[error("revalidate exact promoted boot-publication receipt")]
    Receipt(#[from] BootPublicationReceiptPromotionError),
}
