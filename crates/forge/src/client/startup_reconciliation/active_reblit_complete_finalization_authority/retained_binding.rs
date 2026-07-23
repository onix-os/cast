//! Live retained-binding adapter for exact generation-15 finalization.

use crate::{
    Installation, db,
    client::{
        active_reblit_boot_publication_preflight::ActiveReblitBootCompleteFinalizationSeal,
        active_state_snapshot::ActiveStateReservation,
    },
    transition_journal::{
        Operation, Phase, TransitionJournalRecordBinding, TransitionJournalStore,
        TransitionRecord,
    },
};

use super::{
    ActiveReblitCompleteFinalizationAuthority,
    ActiveReblitCompleteFinalizationAuthorityError,
    ActiveReblitCompleteFinalizationAuthorityErrorKind,
    ActiveReblitCompleteFinalizationCapture,
    same_nonempty_candidate_and_previous,
};

impl ActiveReblitCompleteFinalizationAuthority<'_> {
    /// Admit only the exact live promoted generation-15 route while consuming
    /// the journal binding retained continuously by boot coordination.
    pub(in crate::client) fn capture_retained_binding<'reservation>(
        _seal: ActiveReblitBootCompleteFinalizationSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        journal_record_binding: TransitionJournalRecordBinding,
    ) -> Result<
        ActiveReblitCompleteFinalizationAuthority<'reservation>,
        ActiveReblitCompleteFinalizationAuthorityError,
    > {
        let receipt_pair = record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitCompleteFinalizationAuthorityErrorKind::Record)?;
        if record.operation != Operation::ActiveReblit
            || record.phase != Phase::Complete
            || record.generation != 15
            || record.rollback.is_some()
            || record.options.archive_previous
            || !record.options.run_system_triggers
            || !record.options.run_boot_sync
            || receipt_pair.is_none()
            || !same_nonempty_candidate_and_previous(record)
        {
            return Err(
                ActiveReblitCompleteFinalizationAuthorityErrorKind::RetainedCompleteRejected
                    .into(),
            );
        }

        match Self::capture_with_record_binding(
            installation,
            journal,
            state_db,
            active_state_reservation,
            record,
            || Ok((journal.binding(), journal_record_binding)),
        )? {
            ActiveReblitCompleteFinalizationCapture::Ready(authority) => Ok(authority),
            ActiveReblitCompleteFinalizationCapture::NotApplicable
            | ActiveReblitCompleteFinalizationCapture::Deferred => Err(
                ActiveReblitCompleteFinalizationAuthorityErrorKind::RetainedCompleteRejected
                    .into(),
            ),
        }
    }
}
