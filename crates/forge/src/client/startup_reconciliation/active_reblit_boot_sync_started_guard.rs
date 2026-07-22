//! Read-only startup guard for an ActiveReblit `BootSyncStarted` record.
//!
//! A receipt which is still exactly pending remains eligible for the existing
//! conservative rollback route. Once that exact receipt has been promoted,
//! rollback is no longer an admissible interpretation: startup must retain the
//! unchanged forward checkpoint until durable cleanup recovery is available.

use crate::{
    db,
    transition_journal::{CodecError, Operation, Phase, TransitionRecord},
};

/// Read-only classification of the one checkpoint which straddles receipt
/// promotion. No variant grants journal, database, or filesystem mutation.
pub(in crate::client) enum ActiveReblitBootSyncStartedGuardAdmission {
    NotApplicable,
    RollbackEligible,
    Promoted,
}

pub(in crate::client) struct ActiveReblitBootSyncStartedGuard;

impl ActiveReblitBootSyncStartedGuard {
    pub(in crate::client) fn inspect(
        state_db: &db::state::Database,
        record: &TransitionRecord,
    ) -> Result<ActiveReblitBootSyncStartedGuardAdmission, ActiveReblitBootSyncStartedGuardError> {
        if record.operation != Operation::ActiveReblit || record.phase != Phase::BootSyncStarted {
            return Ok(ActiveReblitBootSyncStartedGuardAdmission::NotApplicable);
        }

        let Some(receipt_pair) = record
            .boot_publication_receipt_correlation()
            .map_err(ActiveReblitBootSyncStartedGuardError::Record)?
        else {
            // Valid v1/v2 records predate durable receipt correlation and keep
            // their existing conservative rollback behavior.
            return Ok(ActiveReblitBootSyncStartedGuardAdmission::RollbackEligible);
        };

        match state_db.load_exact_promoted_boot_publication_receipt_state(
            &record.transition_id,
            &receipt_pair,
        ) {
            Ok(_) => Ok(ActiveReblitBootSyncStartedGuardAdmission::Promoted),
            Err(db::state::ExactPromotedBootPublicationReceiptStateError::PendingHeadPresent {
                ..
            }) => {
                let pending = state_db
                    .boot_publication_receipt_state()
                    .map_err(ActiveReblitBootSyncStartedGuardError::PendingReceiptState)?;
                if pending.receipt_pair_for(&record.transition_id) == Some(receipt_pair) {
                    Ok(ActiveReblitBootSyncStartedGuardAdmission::RollbackEligible)
                } else {
                    Err(ActiveReblitBootSyncStartedGuardError::PendingReceiptCorrelationMismatch)
                }
            }
            Err(source) => Err(ActiveReblitBootSyncStartedGuardError::ReceiptCorrelation(source)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum ActiveReblitBootSyncStartedGuardError {
    #[error("decode and validate the exact ActiveReblit BootSyncStarted journal record")]
    Record(#[source] CodecError),
    #[error("strictly load the pending boot-publication receipt state")]
    PendingReceiptState(#[source] db::state::BootPublicationReceiptStateError),
    #[error("the pending boot-publication receipt does not match the exact journal pair")]
    PendingReceiptCorrelationMismatch,
    #[error("the boot-publication receipt state conflicts with the exact promoted journal pair")]
    ReceiptCorrelation(#[source] db::state::ExactPromotedBootPublicationReceiptStateError),
}
