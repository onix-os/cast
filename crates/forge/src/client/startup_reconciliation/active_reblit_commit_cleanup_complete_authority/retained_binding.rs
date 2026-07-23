//! Live retained-binding adapter for exact generation-14 completion.

use crate::{
    Installation, db,
    client::{
        active_reblit_boot_publication_preflight::ActiveReblitCommitCleanupCompleteSeal,
        active_state_snapshot::ActiveStateReservation,
    },
    transition_journal::{
        Operation, Phase, TransitionJournalRecordBinding, TransitionJournalStore,
        TransitionRecord,
    },
};

use super::{
    ActiveReblitCommitCleanupCompleteAuthority,
    ActiveReblitCommitCleanupCompleteAuthorityError,
    ActiveReblitCommitCleanupCompleteAuthorityErrorKind,
    ActiveReblitCommitCleanupCompleteCapture,
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
    ) -> Result<
        ActiveReblitCommitCleanupCompleteAuthority<'reservation>,
        ActiveReblitCommitCleanupCompleteAuthorityError,
    > {
        let receipt_pair = record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitCommitCleanupCompleteAuthorityErrorKind::Record)?;
        if record.operation != Operation::ActiveReblit
            || record.phase != Phase::CommitCleanupComplete
            || record.generation != 14
            || record.rollback.is_some()
            || record.options.archive_previous
            || !record.options.run_system_triggers
            || !record.options.run_boot_sync
            || receipt_pair.is_none()
            || !same_nonempty_candidate_and_previous(record)
        {
            return Err(
                ActiveReblitCommitCleanupCompleteAuthorityErrorKind::RetainedCommitCleanupCompleteRejected
                    .into(),
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
            | ActiveReblitCommitCleanupCompleteCapture::Apply => Err(
                ActiveReblitCommitCleanupCompleteAuthorityErrorKind::RetainedCommitCleanupCompleteRejected
                    .into(),
            ),
        }
    }
}
