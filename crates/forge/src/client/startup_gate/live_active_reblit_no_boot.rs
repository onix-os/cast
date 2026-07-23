//! Continuously owned live tail for the exact no-boot ActiveReblit route.
//!
//! This adapter adds no cleanup policy. It consumes the coordinator's sealed
//! generation-11 handoff through the existing startup reconciliation effects,
//! retained-binding persistence edges, terminal finalizer, and shared clean
//! admission gate.

use thiserror::Error;

use crate::{
    Installation,
    client::{
        CoordinatorActiveStateReservation,
        startup_reconciliation::{
            ActiveReblitCommitCleanupAdmission,
            ActiveReblitCommitCleanupApplyReconciliation,
            ActiveReblitCommitCleanupAuthority,
            ActiveReblitCommitCleanupAuthorityError,
            ActiveReblitCommitCleanupCompleteAdmission,
            ActiveReblitCommitCleanupCompleteAuthority,
            ActiveReblitCommitCleanupCompleteAuthorityError,
            ActiveReblitCommitCleanupEffectError,
            ActiveReblitCompleteFinalizationAdmission,
            ActiveReblitCompleteFinalizationAuthority,
            ActiveReblitCompleteFinalizationAuthorityError,
        },
        startup_recovery::{
            ActiveReblitCommitCleanupCompletePersistenceError,
            ActiveReblitCommitCleanupPersistenceError,
            ActiveReblitCompleteFinalizationError,
            finalize_active_reblit_complete,
            persist_active_reblit_commit_cleanup_complete_retaining_binding,
            persist_active_reblit_commit_cleanup_complete_to_complete_retaining_binding,
        },
    },
    db,
    transition_identity::ActiveReblitNoBootTailSeal,
    transition_journal::{TransitionJournalStore, TransitionRecord},
};

use super::{
    ActiveReblitCommitCleanupCompleteSeal, ActiveReblitCompleteFinalizationSeal,
    CleanSystemStartup,
};

/// Exact clean terminal authority returned to the future live client route.
///
/// Field order is deliberate: the clean-startup journal guard is released
/// before the cooperating-writer reservation.
#[must_use = "the clean no-boot terminal authority must be retained through caller validation"]
pub(crate) struct FinalizedActiveReblitNoBoot {
    complete_record: TransitionRecord,
    _clean_startup: CleanSystemStartup,
    _active_state_reservation: CoordinatorActiveStateReservation,
}

impl std::fmt::Debug for FinalizedActiveReblitNoBoot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FinalizedActiveReblitNoBoot")
            .field("complete_record", &self.complete_record)
            .finish_non_exhaustive()
    }
}

impl FinalizedActiveReblitNoBoot {
    #[cfg(test)]
    pub(crate) const fn complete_record(&self) -> &TransitionRecord {
        &self.complete_record
    }
}

/// Drive only the already-defined no-boot cleanup and terminal suffix.
pub(crate) fn finish_active_reblit_no_boot(
    _seal: ActiveReblitNoBootTailSeal,
    installation: Installation,
    state_db: db::state::Database,
    journal: TransitionJournalStore,
    record: TransitionRecord,
    active_state_reservation: CoordinatorActiveStateReservation,
) -> Result<FinalizedActiveReblitNoBoot, ActiveReblitNoBootTailError> {
    let pending = match ActiveReblitCommitCleanupAuthority::capture(
        &installation,
        &journal,
        &state_db,
        &active_state_reservation,
        &record,
    )
    .map_err(ActiveReblitNoBootTailErrorKind::CommitCleanupAuthority)?
    {
        ActiveReblitCommitCleanupAdmission::NotApplicable => {
            return Err(ActiveReblitNoBootTailErrorKind::CommitCleanupNotApplicable.into());
        }
        ActiveReblitCommitCleanupAdmission::Deferred => {
            return Err(ActiveReblitNoBootTailErrorKind::CommitCleanupDeferred.into());
        }
        ActiveReblitCommitCleanupAdmission::Apply(authority) => {
            let effect = authority
                .into_effect_authority(&journal)
                .map_err(ActiveReblitNoBootTailErrorKind::CommitCleanupAuthority)?;
            match effect
                .reconcile(&journal)
                .map_err(ActiveReblitNoBootTailErrorKind::CommitCleanupEffect)?
            {
                ActiveReblitCommitCleanupApplyReconciliation::Applied(pending) => pending,
                ActiveReblitCommitCleanupApplyReconciliation::NotApplied => {
                    return Err(ActiveReblitNoBootTailErrorKind::CommitCleanupNotApplied.into());
                }
                ActiveReblitCommitCleanupApplyReconciliation::Ambiguous => {
                    return Err(ActiveReblitNoBootTailErrorKind::CommitCleanupAmbiguous.into());
                }
            }
        }
        ActiveReblitCommitCleanupAdmission::Finish(authority) => authority
            .into_effect_authority(&journal)
            .map_err(ActiveReblitNoBootTailErrorKind::CommitCleanupAuthority)?
            .into_durability(&journal)
            .map_err(ActiveReblitNoBootTailErrorKind::CommitCleanupEffect)?,
    };
    let durable = pending
        .complete(&journal)
        .map_err(ActiveReblitNoBootTailErrorKind::CommitCleanupEffect)?;
    let (journal, record, record_binding) =
        persist_active_reblit_commit_cleanup_complete_retaining_binding(journal, durable)
            .map_err(ActiveReblitNoBootTailErrorKind::CommitCleanupPersistence)?;
    drop(record_binding);

    let cleanup_complete_seal = ActiveReblitCommitCleanupCompleteSeal::new();
    let cleanup_complete = match ActiveReblitCommitCleanupCompleteAuthority::capture(
        &cleanup_complete_seal,
        &installation,
        &journal,
        &state_db,
        &active_state_reservation,
        &record,
    )
    .map_err(ActiveReblitNoBootTailErrorKind::CleanupCompleteAuthority)?
    {
        ActiveReblitCommitCleanupCompleteAdmission::NotApplicable => {
            return Err(ActiveReblitNoBootTailErrorKind::CleanupCompleteNotApplicable.into());
        }
        ActiveReblitCommitCleanupCompleteAdmission::Deferred => {
            return Err(ActiveReblitNoBootTailErrorKind::CleanupCompleteDeferred.into());
        }
        ActiveReblitCommitCleanupCompleteAdmission::Ready(authority) => authority,
    };
    let (journal, complete_record, complete_binding) =
        persist_active_reblit_commit_cleanup_complete_to_complete_retaining_binding(
            journal,
            cleanup_complete,
        )
        .map_err(ActiveReblitNoBootTailErrorKind::CompletePersistence)?;
    drop(complete_binding);

    let finalization_seal = ActiveReblitCompleteFinalizationSeal::new();
    let finalization = match ActiveReblitCompleteFinalizationAuthority::capture(
        &finalization_seal,
        &installation,
        &journal,
        &state_db,
        &active_state_reservation,
        &complete_record,
    )
    .map_err(ActiveReblitNoBootTailErrorKind::FinalizationAuthority)?
    {
        ActiveReblitCompleteFinalizationAdmission::NotApplicable => {
            return Err(ActiveReblitNoBootTailErrorKind::FinalizationNotApplicable.into());
        }
        ActiveReblitCompleteFinalizationAdmission::Deferred => {
            return Err(ActiveReblitNoBootTailErrorKind::FinalizationDeferred.into());
        }
        ActiveReblitCompleteFinalizationAdmission::Ready(authority) => authority,
    };
    let journal = finalize_active_reblit_complete(journal, finalization)
        .map_err(ActiveReblitNoBootTailErrorKind::Finalization)?;
    let clean_startup = CleanSystemStartup::admit_clean_after_terminal_finalization(
        &installation,
        &state_db,
        journal,
    )
    .map_err(ActiveReblitNoBootTailErrorKind::CleanAdmission)?;

    Ok(FinalizedActiveReblitNoBoot {
        complete_record,
        _clean_startup: clean_startup,
        _active_state_reservation: active_state_reservation,
    })
}

#[derive(Debug, Error)]
#[error(transparent)]
pub(crate) struct ActiveReblitNoBootTailError(#[from] ActiveReblitNoBootTailErrorKind);

#[derive(Debug, Error)]
enum ActiveReblitNoBootTailErrorKind {
    #[error("capture exact no-boot ActiveReblit CommitDecided cleanup authority")]
    CommitCleanupAuthority(#[source] ActiveReblitCommitCleanupAuthorityError),
    #[error("exact no-boot ActiveReblit CommitDecided was not applicable to cleanup")]
    CommitCleanupNotApplicable,
    #[error("exact no-boot ActiveReblit CommitDecided cleanup evidence was deferred")]
    CommitCleanupDeferred,
    #[error("run exact no-boot ActiveReblit commit cleanup")]
    CommitCleanupEffect(#[source] ActiveReblitCommitCleanupEffectError),
    #[error("no-boot ActiveReblit cleanup exchange was proven not applied")]
    CommitCleanupNotApplied,
    #[error("no-boot ActiveReblit cleanup exchange outcome was ambiguous")]
    CommitCleanupAmbiguous,
    #[error("persist exact no-boot ActiveReblit CommitCleanupComplete successor")]
    CommitCleanupPersistence(#[source] ActiveReblitCommitCleanupPersistenceError),
    #[error("capture exact no-boot ActiveReblit CommitCleanupComplete authority")]
    CleanupCompleteAuthority(#[source] ActiveReblitCommitCleanupCompleteAuthorityError),
    #[error("exact no-boot ActiveReblit CommitCleanupComplete was not applicable")]
    CleanupCompleteNotApplicable,
    #[error("exact no-boot ActiveReblit CommitCleanupComplete evidence was deferred")]
    CleanupCompleteDeferred,
    #[error("persist exact no-boot ActiveReblit Complete successor")]
    CompletePersistence(#[source] ActiveReblitCommitCleanupCompletePersistenceError),
    #[error("capture exact no-boot ActiveReblit Complete finalization authority")]
    FinalizationAuthority(#[source] ActiveReblitCompleteFinalizationAuthorityError),
    #[error("exact no-boot ActiveReblit Complete was not applicable to finalization")]
    FinalizationNotApplicable,
    #[error("exact no-boot ActiveReblit Complete finalization evidence was deferred")]
    FinalizationDeferred,
    #[error("delete exact no-boot ActiveReblit Complete journal")]
    Finalization(#[source] ActiveReblitCompleteFinalizationError),
    #[error("admit clean system state on the terminal finalizer's locked journal store")]
    CleanAdmission(#[source] super::Error),
}
