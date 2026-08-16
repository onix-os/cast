//! Live retained-binding adapter for exact generation-15 finalization.

use crate::{
    Installation,
    client::{
        active_reblit_boot_publication_preflight::ActiveReblitBootCompleteFinalizationSeal,
        active_state_snapshot::ActiveStateReservation,
    },
    db,
    transition_journal::{Operation, Phase, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    ActiveReblitCompleteFinalizationAuthority, ActiveReblitCompleteFinalizationAuthorityError,
    ActiveReblitCompleteFinalizationAuthorityErrorKind, ActiveReblitCompleteFinalizationCapture,
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
    ) -> Result<ActiveReblitCompleteFinalizationAuthority<'reservation>, ActiveReblitCompleteFinalizationAuthorityError>
    {
        let receipt_pair = record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitCompleteFinalizationAuthorityErrorKind::Record)?;
        if !crate::client::active_reblit_boot_sync_staging::supports_boot_sync(record.operation)
            || record.phase != Phase::Complete
            || !crate::client::active_reblit_boot_sync_staging::boot_tail_generation_is_exact(record)
            || record.rollback.is_some()
            || !crate::client::active_reblit_boot_sync_staging::boot_tail_options_are_exact(record)
            || receipt_pair.is_none()
            || !crate::client::active_reblit_boot_sync_staging::boot_tail_identity_is_exact(record)
        {
            return Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::RetainedCompleteRejected.into());
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
            | ActiveReblitCompleteFinalizationCapture::Deferred => {
                Err(ActiveReblitCompleteFinalizationAuthorityErrorKind::RetainedCompleteRejected.into())
            }
        }
    }
}
