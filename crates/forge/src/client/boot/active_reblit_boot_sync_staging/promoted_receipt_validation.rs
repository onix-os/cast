//! Read-only validation after exact boot-publication receipt promotion.
//!
//! Promotion changes the database head, but deliberately leaves the journal
//! at the exact receipt-bearing `BootSyncStarted` record. This validator
//! reauthenticates that cross-store shape without advancing the journal or
//! granting any publication, replacement, deletion, or cleanup authority.

use thiserror::Error;

use crate::{
    boot_publication::{
        BootPublicationReceiptFingerprint, BootPublicationReceiptPair,
        CanonicalBootPublicationReceipt,
    },
    client::active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
    db::state::BootPublicationReceiptPromotionError,
    installation,
    transition_journal::{CodecError, Operation, Phase, StorageError, TransitionRecord},
};

use super::{Client, StagedActiveReblitBootSync};

/// Fresh read-only correlation of a promoted staged receipt with its client.
///
/// Private fields prevent sibling components from manufacturing this view or
/// reaching the retained plan without first passing promoted cross-store
/// validation. It carries no mutation or journal-advance authority.
pub(in crate::client) struct FreshPromotedStagedActiveReblitBootSync<
    'staged,
    'client,
    'plan,
    'inventory,
    Plan,
> {
    staged: &'staged StagedActiveReblitBootSync<'plan, 'inventory, Plan>,
    _client: &'client Client,
}

impl<'plan, 'inventory, Plan> StagedActiveReblitBootSync<'plan, 'inventory, Plan> {
    /// Revalidate this exact `BootSyncStarted` staging result after its receipt
    /// has become the committed database head.
    ///
    /// The retained journal record intentionally continues to carry the
    /// predecessor-to-receipt staging pair while the database must now carry
    /// the receipt as committed with no pending head. The database helper also
    /// requires the immutable predecessor body when the receipt names one.
    pub(in crate::client) fn revalidate_promoted_against<'staged, 'client>(
        &'staged self,
        client: &'client Client,
    ) -> Result<
        FreshPromotedStagedActiveReblitBootSync<
            'staged,
            'client,
            'plan,
            'inventory,
            Plan,
        >,
        ActiveReblitBootSyncPromotedValidationError,
    > {
        if !self.database.same_instance(&client.state_db)
            || !std::ptr::eq(
                self.installation.root_directory(),
                client.installation.root_directory(),
            )
        {
            return Err(
                ActiveReblitBootSyncPromotedValidationError::ClientCapabilityMismatch,
            );
        }
        if !self.journal.has_record_store_binding(&self.record_binding) {
            return Err(
                ActiveReblitBootSyncPromotedValidationError::JournalCapabilityMismatch,
            );
        }

        self.installation
            .revalidate_mutable_namespace()
            .map_err(ActiveReblitBootSyncPromotedValidationError::Installation)?;
        let cast = self
            .installation
            .retained_mutable_cast_directory()
            .map_err(ActiveReblitBootSyncPromotedValidationError::Installation)?;
        if !self
            .journal
            .has_record_binding(cast, &self.record_binding, &self.record)
            .map_err(ActiveReblitBootSyncPromotedValidationError::Journal)?
        {
            return Err(
                ActiveReblitBootSyncPromotedValidationError::BootSyncStartedBindingChanged,
            );
        }
        self.require_exact_promoted_journal_record()?;

        self.database
            .require_promoted_boot_publication_receipt(&self.receipt)
            .map_err(ActiveReblitBootSyncPromotedValidationError::ReceiptState)?;

        self.installation
            .revalidate_mutable_namespace()
            .map_err(ActiveReblitBootSyncPromotedValidationError::Installation)?;
        if !self
            .journal
            .has_record_binding(cast, &self.record_binding, &self.record)
            .map_err(ActiveReblitBootSyncPromotedValidationError::Journal)?
        {
            return Err(
                ActiveReblitBootSyncPromotedValidationError::BootSyncStartedBindingChanged,
            );
        }
        Ok(FreshPromotedStagedActiveReblitBootSync {
            staged: self,
            _client: client,
        })
    }

    fn require_exact_promoted_journal_record(
        &self,
    ) -> Result<(), ActiveReblitBootSyncPromotedValidationError> {
        // `pending` is the journal codec's name for the successor receipt. It
        // remains in BootSyncStarted after the database head is promoted.
        let expected = BootPublicationReceiptPair {
            committed: self.receipt.body().committed_predecessor(),
            pending: self.receipt.fingerprint(),
        };
        let actual = self
            .record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitBootSyncPromotedValidationError::RecordReceipt)?;
        if self.record.operation != Operation::ActiveReblit
            || self.record.phase != Phase::BootSyncStarted
            || &self.record.transition_id != self.receipt.body().transition_id()
            || actual != Some(expected)
        {
            return Err(
                ActiveReblitBootSyncPromotedValidationError::RecordReceiptMismatch,
            );
        }
        Ok(())
    }
}

impl<'plan, 'inventory, Plan>
    FreshPromotedStagedActiveReblitBootSync<'_, '_, 'plan, 'inventory, Plan>
{
    pub(in crate::client) const fn record(&self) -> &TransitionRecord {
        self.staged.record()
    }

    pub(in crate::client) const fn receipt(&self) -> &CanonicalBootPublicationReceipt {
        self.staged.receipt()
    }

    pub(in crate::client) const fn receipt_fingerprint(&self) -> BootPublicationReceiptFingerprint {
        self.staged.receipt_fingerprint()
    }

    pub(in crate::client) const fn plan(&self) -> &'plan Plan {
        self.staged.plan
    }

    pub(in crate::client) const fn inventory(
        &self,
    ) -> &'inventory PreparedActiveReblitDesiredPublicationInventory {
        self.staged.inventory
    }
}

/// Fail-closed error from read-only post-promotion cross-store validation.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncPromotedValidationError {
    #[error("the promoted boot-sync result belongs to a different client capability set")]
    ClientCapabilityMismatch,
    #[error("the retained BootSyncStarted binding belongs to a different journal store")]
    JournalCapabilityMismatch,
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[source] installation::Error),
    #[error("revalidate the exact retained BootSyncStarted journal binding")]
    Journal(#[source] StorageError),
    #[error("the retained BootSyncStarted journal binding changed")]
    BootSyncStartedBindingChanged,
    #[error("decode the retained BootSyncStarted receipt pair")]
    RecordReceipt(#[source] CodecError),
    #[error("the retained BootSyncStarted record does not carry the exact promoted receipt pair")]
    RecordReceiptMismatch,
    #[error("require the exact canonical receipt as committed with no pending database head")]
    ReceiptState(#[source] BootPublicationReceiptPromotionError),
}
