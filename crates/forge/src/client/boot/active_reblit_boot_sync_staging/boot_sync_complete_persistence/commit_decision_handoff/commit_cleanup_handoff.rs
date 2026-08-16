//! Sealed generation-14 handoff after live ActiveReblit commit cleanup.

use thiserror::Error;

use crate::{
    Installation,
    boot_publication::{BootPublicationReceiptFingerprint, CanonicalBootPublicationReceipt},
    client::{
        Client, CoordinatorActiveStateReservation,
        active_reblit_boot_publication_preflight::ActiveReblitCommitCleanupSeal,
        active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
        startup_reconciliation::{
            ActiveReblitCommitCleanupApplyReconciliation, ActiveReblitCommitCleanupAuthority,
            ActiveReblitCommitCleanupAuthorityError, ActiveReblitCommitCleanupEffectError,
        },
        startup_recovery::{
            ActiveReblitCommitCleanupPersistenceError, persist_active_reblit_commit_cleanup_complete_retaining_binding,
        },
    },
    db::state::{BootPublicationReceiptPromotionError, BootPublicationReceiptStageOutcome, Database},
    installation,
    transition_journal::{
        CodecError, ForwardPhase, Operation, Phase, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord, expected_forward_generation,
    },
};

use super::{CommittedStagedActiveReblitBootSync, receipt_pair};

const ACTIVE_REBLIT_COMMIT_CLEANUP_COMPLETE_GENERATION: u64 = 14;

/// Exact durable cleanup result retaining every capability required by the
/// later finalization slice. Private fields and the absence of constructors
/// keep this value one-origin and non-replayable.
#[must_use = "cleanup-complete authority must enter finalization or be deliberately discarded"]
pub(in crate::client) struct CommitCleanupCompleteStagedActiveReblitBootSync<'plan, 'inventory, Plan> {
    commit_decided_record: TransitionRecord,
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

impl<Plan> std::fmt::Debug for CommitCleanupCompleteStagedActiveReblitBootSync<'_, '_, Plan> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommitCleanupCompleteStagedActiveReblitBootSync")
            .field("record", &self.record)
            .field("receipt", &self.receipt)
            .field("staging_outcome", &self.staging_outcome)
            .finish_non_exhaustive()
    }
}

impl<'plan, 'inventory, Plan> CommitCleanupCompleteStagedActiveReblitBootSync<'plan, 'inventory, Plan> {
    pub(in crate::client) const fn record(&self) -> &TransitionRecord {
        &self.record
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
    ) -> Result<(), CommitCleanupCompleteStagedActiveReblitBootSyncValidationError> {
        if !self.database.same_instance(&client.state_db)
            || !std::ptr::eq(self.installation.root_directory(), client.installation.root_directory())
        {
            return Err(CommitCleanupCompleteStagedActiveReblitBootSyncValidationError::ClientCapabilityMismatch);
        }
        self.installation.revalidate_mutable_namespace()?;
        let cast = self.installation.retained_mutable_cast_directory()?;
        if !self.journal.has_record_store_binding(&self.record_binding)
            || !self
                .journal
                .has_record_binding(cast, &self.record_binding, &self.record)?
        {
            return Err(CommitCleanupCompleteStagedActiveReblitBootSyncValidationError::BindingChanged);
        }
        let expected = self.commit_decided_record.forward_successor(None)?;
        let pair = receipt_pair(&self.receipt);
        if expected != self.record
            || Some(self.commit_decided_record.generation)
                != expected_forward_generation(&self.commit_decided_record, ForwardPhase::CommitDecided)
            || Some(self.record.generation)
                != expected_forward_generation(&self.record, ForwardPhase::CommitCleanupComplete)
            || !matches!(
                self.commit_decided_record.operation,
                Operation::ActiveReblit | Operation::NewState
            )
            || self.commit_decided_record.operation != self.record.operation
            || self.commit_decided_record.phase != Phase::CommitDecided
            || self.record.phase != Phase::CommitCleanupComplete
            || !exact_live_options(&self.commit_decided_record)
            || !exact_live_options(&self.record)
            || self.commit_decided_record.rollback.is_some()
            || self.record.rollback.is_some()
            || self.commit_decided_record.boot_publication_receipt_correlation()? != Some(pair)
            || self.record.boot_publication_receipt_correlation()? != Some(pair)
        {
            return Err(CommitCleanupCompleteStagedActiveReblitBootSyncValidationError::UnexpectedRecord);
        }
        self.database.require_promoted_boot_publication_receipt(&self.receipt)?;
        self.installation.revalidate_mutable_namespace()?;
        if !self
            .journal
            .has_record_binding(cast, &self.record_binding, &self.record)?
        {
            return Err(CommitCleanupCompleteStagedActiveReblitBootSyncValidationError::BindingChanged);
        }
        Ok(())
    }
}

impl<'plan, 'inventory, Plan> CommittedStagedActiveReblitBootSync<'plan, 'inventory, Plan> {
    /// Hand a committed NewState or ActivateArchived transition to the
    /// activation terminal route, which already owns the whole tail for the two
    /// operations that reclaim nothing.
    ///
    /// The ActiveReblit chain below exists for the staging-wrapper exchange a
    /// reblit performs; these operations have no such effect, so they finish
    /// through the authority written for them rather than through evidence
    /// shaped around a wrapper they never had.
    pub(in crate::client) fn finish_activation_tail(
        self,
        client: &Client,
    ) -> Result<(), CommittedStagedActiveReblitCommitCleanupError> {
        self.revalidate_against(client)
            .map_err(CommittedStagedActiveReblitCommitCleanupError::CommittedEvidence)?;
        let CommittedStagedActiveReblitBootSync {
            record,
            record_binding,
            journal,
            database,
            installation,
            active_state_reservation,
            ..
        } = self;
        // The activation route rebinds the record itself at every step.
        drop(record_binding);
        let journal = crate::client::startup_recovery::finish_activation_after_commit(
            journal,
            &database,
            &installation,
            record,
            &active_state_reservation,
        )
        .map_err(CommittedStagedActiveReblitCommitCleanupError::ActivationTail)?;
        crate::client::startup_gate::CleanSystemStartup::admit_clean_after_terminal_finalization(
            &installation,
            &database,
            journal,
        )
        .map_err(CommittedStagedActiveReblitCommitCleanupError::CleanAdmission)?;
        Ok(())
    }

    /// Consume exact live `CommitDecided` authority through Apply cleanup, its
    /// fixed durability suffix, and the sole generation-14 journal successor.
    pub(in crate::client) fn persist_commit_cleanup_complete(
        self,
        client: &Client,
        seal: ActiveReblitCommitCleanupSeal,
    ) -> Result<
        CommitCleanupCompleteStagedActiveReblitBootSync<'plan, 'inventory, Plan>,
        CommittedStagedActiveReblitCommitCleanupError,
    > {
        self.revalidate_against(client)
            .map_err(CommittedStagedActiveReblitCommitCleanupError::CommittedEvidence)?;

        let CommittedStagedActiveReblitBootSync {
            completed_record: _,
            record: commit_decided_record,
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

        // A first-install NewState has no predecessor tree, so there is no
        // staging wrapper to reclaim and the edge is journal-only. Every other
        // shape keeps the reclaiming route below.
        if crate::client::startup_reconciliation::new_state_boot_tail_edge_is_admissible(
            &commit_decided_record,
            Phase::CommitCleanupComplete,
        ) {
            let (journal, record, record_binding) =
                crate::client::startup_recovery::persist_new_state_boot_tail_edge_retaining_binding(
                    &installation,
                    journal,
                    &commit_decided_record,
                    record_binding,
                    Phase::CommitCleanupComplete,
                )
                .map_err(CommittedStagedActiveReblitCommitCleanupError::NewStateCleanup)?;
            return Ok(CommitCleanupCompleteStagedActiveReblitBootSync {
                commit_decided_record,
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
            });
        }

        let authority = ActiveReblitCommitCleanupAuthority::capture_retained_binding(
            seal,
            &installation,
            &journal,
            &database,
            &active_state_reservation,
            &commit_decided_record,
            record_binding,
        )
        .map_err(CommittedStagedActiveReblitCommitCleanupError::Authority)?;
        let effect = authority
            .into_effect_authority(&journal)
            .map_err(CommittedStagedActiveReblitCommitCleanupError::Authority)?;
        let pending = match effect
            .reconcile(&journal)
            .map_err(CommittedStagedActiveReblitCommitCleanupError::Effect)?
        {
            ActiveReblitCommitCleanupApplyReconciliation::Applied(pending) => pending,
            ActiveReblitCommitCleanupApplyReconciliation::NotApplied => {
                return Err(CommittedStagedActiveReblitCommitCleanupError::NotApplied);
            }
            ActiveReblitCommitCleanupApplyReconciliation::Ambiguous => {
                return Err(CommittedStagedActiveReblitCommitCleanupError::Ambiguous);
            }
        };
        let durable = pending
            .complete(&journal)
            .map_err(CommittedStagedActiveReblitCommitCleanupError::Effect)?;
        let (journal, record, record_binding) =
            persist_active_reblit_commit_cleanup_complete_retaining_binding(journal, durable)
                .map_err(CommittedStagedActiveReblitCommitCleanupError::Persistence)?;

        let cleaned = CommitCleanupCompleteStagedActiveReblitBootSync {
            commit_decided_record,
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
        cleaned
            .revalidate_against(client)
            .map_err(CommittedStagedActiveReblitCommitCleanupError::CleanupCompleteEvidence)?;
        Ok(cleaned)
    }
}

/// The options a live boot-publishing transition must carry.
///
/// `archive_previous` is deliberately not constrained: ActiveReblit repairs in
/// place and never archives, while NewState over an active predecessor always
/// does. Both publish boot entries, and the generation check already pins which
/// phase chain the record actually walked (`plans/future_impl.md` §1.1d).
fn exact_live_options(record: &TransitionRecord) -> bool {
    record.options.run_system_triggers && record.options.run_boot_sync
}

#[path = "commit_cleanup_handoff/complete_handoff.rs"]
mod complete_handoff;
pub(in crate::client) use complete_handoff::{
    CommitCleanupCompleteStagedActiveReblitCompleteError, CompleteStagedActiveReblitBootSync,
    CompleteStagedActiveReblitBootSyncValidationError, CompleteStagedActiveReblitFinalizationError,
    FinalizedStagedActiveReblitBootSync, FinalizedStagedActiveReblitBootSyncValidationError,
};

#[derive(Debug, Error)]
pub(in crate::client) enum CommittedStagedActiveReblitCommitCleanupError {
    #[error("revalidate exact retained CommitDecided staging handoff")]
    CommittedEvidence(#[source] super::CommittedStagedActiveReblitBootSyncValidationError),
    #[error("admit retained exact generation-13 Apply cleanup authority")]
    Authority(#[source] ActiveReblitCommitCleanupAuthorityError),
    #[error("persist the journal-only first-install NewState cleanup edge")]
    NewStateCleanup(#[source] crate::client::startup_recovery::NewStateCommitCleanupPersistenceError),
    #[error("finish the committed activation transition through its terminal route")]
    ActivationTail(#[source] crate::client::startup_recovery::ActivationCommitCleanupPersistenceError),
    #[error("admit clean startup after the activation terminal route")]
    CleanAdmission(#[source] crate::client::startup_gate::Error),
    #[error("perform exact ActiveReblit cleanup and fixed durability suffix")]
    Effect(#[source] ActiveReblitCommitCleanupEffectError),
    #[error("the live Apply cleanup exchange was not applied")]
    NotApplied,
    #[error("the live Apply cleanup exchange outcome was ambiguous")]
    Ambiguous,
    #[error("persist exact generation-14 CommitCleanupComplete successor")]
    Persistence(#[source] ActiveReblitCommitCleanupPersistenceError),
    #[error("revalidate exact retained CommitCleanupComplete handoff")]
    CleanupCompleteEvidence(#[source] CommitCleanupCompleteStagedActiveReblitBootSyncValidationError),
}

#[derive(Debug, Error)]
pub(in crate::client) enum CommitCleanupCompleteStagedActiveReblitBootSyncValidationError {
    #[error("the cleanup-complete handoff belongs to a different client capability set")]
    ClientCapabilityMismatch,
    #[error("revalidate retained installation namespace")]
    Installation(#[from] installation::Error),
    #[error("revalidate exact retained CommitCleanupComplete journal binding")]
    Journal(#[from] StorageError),
    #[error("the retained CommitCleanupComplete journal binding changed")]
    BindingChanged,
    #[error("derive or decode exact retained CommitCleanupComplete record")]
    Record(#[from] CodecError),
    #[error("the retained record is not the sole exact CommitCleanupComplete successor")]
    UnexpectedRecord,
    #[error("revalidate exact promoted boot-publication receipt")]
    Receipt(#[from] BootPublicationReceiptPromotionError),
}
