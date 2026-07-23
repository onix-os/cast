//! Sealed completed-staging handoff into durable commit decision.

use thiserror::Error;

use crate::{
    Installation,
    boot_publication::{
        BootPublicationReceiptFingerprint, BootPublicationReceiptPair,
        CanonicalBootPublicationReceipt,
    },
    client::{
        Client, CoordinatorActiveStateReservation,
        active_reblit_boot_publication_preflight::{
            ActiveReblitBootCommitDecisionFinalValidation,
            ActiveReblitBootSyncCommitDecisionSeal,
        },
        active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
        startup_reconciliation::{
            ActiveReblitBootSyncCompleteAuthority,
            ActiveReblitBootSyncCompleteAuthorityError,
        },
        startup_recovery::{
            ActiveReblitBootSyncCommitDecisionPersistenceError,
            persist_active_reblit_boot_sync_commit_decision_retaining_binding,
        },
    },
    db::state::{
        BootPublicationReceiptPromotionError, BootPublicationReceiptStageOutcome,
        Database,
    },
    installation,
    transition_journal::{
        CodecError, Operation, Phase, StorageError,
        TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    ActiveReblitBootSyncCompleteValidationError,
    CompletedStagedActiveReblitBootSync,
};

const ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_GENERATION: u64 = 12;
const ACTIVE_REBLIT_COMMIT_DECIDED_GENERATION: u64 = 13;

/// Exact durable `CommitDecided` state retaining every coordinator capability.
///
/// Private fields and the absence of constructors make this a one-origin,
/// non-cloneable handoff for the later cleanup slice.
#[must_use = "commit-decision authority must enter cleanup coordination or be deliberately discarded"]
pub(in crate::client) struct CommittedStagedActiveReblitBootSync<
    'plan,
    'inventory,
    Plan,
> {
    completed_record: TransitionRecord,
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

impl<Plan> std::fmt::Debug
    for CommittedStagedActiveReblitBootSync<'_, '_, Plan>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommittedStagedActiveReblitBootSync")
            .field("record", &self.record)
            .field("receipt", &self.receipt)
            .field("staging_outcome", &self.staging_outcome)
            .finish_non_exhaustive()
    }
}

impl<'plan, 'inventory, Plan>
    CommittedStagedActiveReblitBootSync<'plan, 'inventory, Plan>
{
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
    ) -> Result<(), CommittedStagedActiveReblitBootSyncValidationError> {
        if !self.database.same_instance(&client.state_db)
            || !std::ptr::eq(
                self.installation.root_directory(),
                client.installation.root_directory(),
            )
        {
            return Err(
                CommittedStagedActiveReblitBootSyncValidationError::ClientCapabilityMismatch,
            );
        }
        self.installation.revalidate_mutable_namespace()?;
        let cast = self.installation.retained_mutable_cast_directory()?;
        if !self.journal.has_record_store_binding(&self.record_binding)
            || !self
                .journal
                .has_record_binding(cast, &self.record_binding, &self.record)?
        {
            return Err(
                CommittedStagedActiveReblitBootSyncValidationError::BindingChanged,
            );
        }
        let expected = self.completed_record.forward_successor(None)?;
        let pair = receipt_pair(&self.receipt);
        if expected != self.record
            || self.completed_record.generation
                != ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_GENERATION
            || self.record.generation != ACTIVE_REBLIT_COMMIT_DECIDED_GENERATION
            || self.record.operation != Operation::ActiveReblit
            || self.record.phase != Phase::CommitDecided
            || self.completed_record.options.archive_previous
            || !self.completed_record.options.run_system_triggers
            || !self.completed_record.options.run_boot_sync
            || self.record.options.archive_previous
            || !self.record.options.run_system_triggers
            || !self.record.options.run_boot_sync
            || self.record.rollback.is_some()
            || self.record.boot_publication_receipt_correlation()? != Some(pair)
        {
            return Err(
                CommittedStagedActiveReblitBootSyncValidationError::UnexpectedRecord,
            );
        }
        self.database
            .require_promoted_boot_publication_receipt(&self.receipt)?;
        self.installation.revalidate_mutable_namespace()?;
        if !self
            .journal
            .has_record_binding(cast, &self.record_binding, &self.record)?
        {
            return Err(
                CommittedStagedActiveReblitBootSyncValidationError::BindingChanged,
            );
        }
        Ok(())
    }
}

impl<'plan, 'inventory, Plan>
    CompletedStagedActiveReblitBootSync<'plan, 'inventory, Plan>
{
    pub(in crate::client) fn persist_commit_decided(
        self,
        client: &Client,
        seal: ActiveReblitBootSyncCommitDecisionSeal,
        final_validation: ActiveReblitBootCommitDecisionFinalValidation<'_>,
    ) -> Result<
        CommittedStagedActiveReblitBootSync<'plan, 'inventory, Plan>,
        CompletedStagedActiveReblitCommitDecisionError,
    > {
        self.revalidate_against(client)
            .map_err(CompletedStagedActiveReblitCommitDecisionError::CompletedEvidence)?;

        let CompletedStagedActiveReblitBootSync {
            predecessor: _,
            record: completed_record,
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

        let authority = ActiveReblitBootSyncCompleteAuthority::capture_retained_binding(
            seal,
            &installation,
            &journal,
            &database,
            &active_state_reservation,
            &completed_record,
            record_binding,
        )
        .map_err(CompletedStagedActiveReblitCommitDecisionError::Authority)?;
        let (journal, record, record_binding) =
            persist_active_reblit_boot_sync_commit_decision_retaining_binding(
                journal,
                authority,
                final_validation,
            )
            .map_err(CompletedStagedActiveReblitCommitDecisionError::Persistence)?;

        let committed = CommittedStagedActiveReblitBootSync {
            completed_record,
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
        committed
            .revalidate_against(client)
            .map_err(CompletedStagedActiveReblitCommitDecisionError::CommittedEvidence)?;
        Ok(committed)
    }
}

fn receipt_pair(
    receipt: &CanonicalBootPublicationReceipt,
) -> BootPublicationReceiptPair {
    BootPublicationReceiptPair {
        pending: receipt.fingerprint(),
        committed: receipt.body().committed_predecessor(),
    }
}

#[path = "commit_decision_handoff/commit_cleanup_handoff.rs"]
mod commit_cleanup_handoff;
pub(in crate::client) use commit_cleanup_handoff::{
    CommitCleanupCompleteStagedActiveReblitCompleteError,
    CommitCleanupCompleteStagedActiveReblitBootSync,
    CommitCleanupCompleteStagedActiveReblitBootSyncValidationError,
    CommittedStagedActiveReblitCommitCleanupError,
    CompleteStagedActiveReblitBootSync,
    CompleteStagedActiveReblitBootSyncValidationError,
};

#[derive(Debug, Error)]
pub(in crate::client) enum CompletedStagedActiveReblitCommitDecisionError {
    #[error("revalidate exact completed staging immediately before commit decision")]
    CompletedEvidence(#[source] ActiveReblitBootSyncCompleteValidationError),
    #[error("admit retained exact BootSyncComplete authority")]
    Authority(#[source] ActiveReblitBootSyncCompleteAuthorityError),
    #[error("persist exact BootSyncComplete to CommitDecided successor")]
    Persistence(#[source] ActiveReblitBootSyncCommitDecisionPersistenceError),
    #[error("revalidate exact retained CommitDecided handoff")]
    CommittedEvidence(#[source] CommittedStagedActiveReblitBootSyncValidationError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum CommittedStagedActiveReblitBootSyncValidationError {
    #[error("the committed staging handoff belongs to a different client capability set")]
    ClientCapabilityMismatch,
    #[error("revalidate retained installation namespace")]
    Installation(#[from] installation::Error),
    #[error("revalidate exact retained CommitDecided journal binding")]
    Journal(#[from] StorageError),
    #[error("the retained CommitDecided journal binding changed")]
    BindingChanged,
    #[error("derive or decode exact retained CommitDecided record")]
    Record(#[from] CodecError),
    #[error("the retained record is not the sole exact CommitDecided successor")]
    UnexpectedRecord,
    #[error("revalidate exact promoted boot-publication receipt")]
    Receipt(#[from] BootPublicationReceiptPromotionError),
}
