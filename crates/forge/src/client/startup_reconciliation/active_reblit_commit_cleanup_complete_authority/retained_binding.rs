//! Live retained-binding adapter for exact generation-14 completion.

use crate::{
    Installation,
    client::{
        active_reblit_boot_publication_preflight::ActiveReblitCommitCleanupCompleteSeal,
        active_state_snapshot::ActiveStateReservation,
    },
    db,
    transition_journal::{Operation, Phase, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    ActiveReblitCommitCleanupCompleteAuthority, ActiveReblitCommitCleanupCompleteAuthorityError,
    ActiveReblitCommitCleanupCompleteAuthorityErrorKind, ActiveReblitCommitCleanupCompleteCapture,
    same_nonempty_candidate_and_previous,
};

impl ActiveReblitCommitCleanupCompleteAuthority<'_> {
    /// Admit only the exact live promoted generation-14 Finish layout while
    /// consuming the journal binding retained by cleanup coordination.
    pub(in crate::client) fn capture_retained_binding<'reservation>(
        _seal: ActiveReblitCommitCleanupCompleteSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        journal_record_binding: TransitionJournalRecordBinding,
    ) -> Result<ActiveReblitCommitCleanupCompleteAuthority<'reservation>, ActiveReblitCommitCleanupCompleteAuthorityError>
    {
        let receipt_pair = record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::Record)?;
        if !crate::client::active_reblit_boot_sync_staging::supports_boot_sync(record.operation)
            || record.phase != Phase::CommitCleanupComplete
            || !crate::client::active_reblit_boot_sync_staging::boot_tail_generation_is_exact(record)
            || record.rollback.is_some()
            || !crate::client::active_reblit_boot_sync_staging::boot_tail_options_are_exact(record)
            || receipt_pair.is_none()
            || !crate::client::active_reblit_boot_sync_staging::boot_tail_identity_is_exact(record)
        {
            return Err(
                ActiveReblitCommitCleanupCompleteAuthorityErrorKind::RetainedCommitCleanupCompleteRejected.into(),
            );
        }

        match Self::capture_with_record_binding(
            installation,
            journal,
            state_db,
            active_state_reservation,
            record,
            || Ok(journal_record_binding),
        )? {
            ActiveReblitCommitCleanupCompleteCapture::Ready(authority) => Ok(authority),
            ActiveReblitCommitCleanupCompleteCapture::NotApplicable
            | ActiveReblitCommitCleanupCompleteCapture::Deferred
            | ActiveReblitCommitCleanupCompleteCapture::Apply => {
                Err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::RetainedCommitCleanupCompleteRejected.into())
            }
        }
    }
}
