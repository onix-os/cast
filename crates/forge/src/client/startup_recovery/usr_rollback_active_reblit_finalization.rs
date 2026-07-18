//! Delete one exact terminal ActiveReblit rollback journal in place.
//!
//! The supplied authority retains exact cleared existing-state database
//! evidence and the preserved whole-wrapper namespace. This boundary
//! authenticates the same public journal directory and exclusive lock around
//! its final authority validation, performs one conditional terminal
//! deletion, and keeps that lock-bearing store alive. Success requires a
//! reported deletion, consumed post-delete authority, and repeated public
//! absence through that same store. No reopen, namespace repair, cleanup,
//! database mutation, trigger, or retry is reachable here.

use thiserror::Error;

use crate::{
    Installation, installation,
    transition_journal::{Operation, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{
    UsrRollbackActiveReblitFinalizationAuthority, UsrRollbackActiveReblitFinalizationAuthorityError,
};

#[cfg(test)]
mod tests;

/// Which exact same-store public canonical state survived a deletion attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackActiveReblitFinalizationRecord {
    RollbackComplete,
    Absent,
}

/// Consume exact terminal authority, delete once, and return the same
/// continuously locked store only after proving exact public absence.
pub(in crate::client) fn finalize_usr_rollback_active_reblit(
    journal: TransitionJournalStore,
    authority: UsrRollbackActiveReblitFinalizationAuthority<'_>,
) -> Result<TransitionJournalStore, UsrRollbackActiveReblitFinalizationError> {
    authority
        .revalidate(&journal)
        .map_err(UsrRollbackActiveReblitFinalizationError::Authority)?;

    let source_record = authority.record().clone();
    if source_record.operation != Operation::ActiveReblit || source_record.phase != Phase::RollbackComplete {
        return Err(UsrRollbackActiveReblitFinalizationError::UnexpectedSource {
            operation: source_record.operation,
            phase: source_record.phase,
        });
    }

    let installation = authority.installation().clone();
    require_exact_public_source(&installation, &journal, &source_record)
        .map_err(UsrRollbackActiveReblitFinalizationError::PreDeleteVerification)?;

    // Every deterministic test race is injected before this final PRE. There
    // is deliberately no callback or optional work between the final
    // authority/public-binding checks and the single delete attempt.
    before_usr_rollback_active_reblit_finalization_final_revalidation();
    authority
        .revalidate(&journal)
        .map_err(UsrRollbackActiveReblitFinalizationError::Authority)?;
    require_exact_public_source(&installation, &journal, &source_record)
        .map_err(UsrRollbackActiveReblitFinalizationError::PreDeleteVerification)?;

    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackActiveReblitFinalizationVerificationError::from)
        .map_err(UsrRollbackActiveReblitFinalizationError::PreDeleteVerification)?;
    let delete = journal.delete_revalidated_retained_cast(cast, &source_record);
    after_usr_rollback_active_reblit_finalization_delete();

    reconcile_delete(delete, journal, authority, &installation, source_record)
}

fn reconcile_delete(
    delete: Result<bool, StorageError>,
    journal: TransitionJournalStore,
    authority: UsrRollbackActiveReblitFinalizationAuthority<'_>,
    installation: &Installation,
    source_record: TransitionRecord,
) -> Result<TransitionJournalStore, UsrRollbackActiveReblitFinalizationError> {
    let durable = verify_exact_durable_state(installation, &journal, authority, &source_record);
    match delete {
        Ok(true) => match durable {
            Ok(DurableUsrRollbackActiveReblitFinalizationRecord::Absent) => Ok(journal),
            Ok(DurableUsrRollbackActiveReblitFinalizationRecord::RollbackComplete) => {
                Err(UsrRollbackActiveReblitFinalizationError::DeleteSucceededButRecordPresent)
            }
            Err(source) => Err(UsrRollbackActiveReblitFinalizationError::PostDeleteVerification(source)),
        },
        Ok(false) => match durable {
            Ok(durable) => Err(UsrRollbackActiveReblitFinalizationError::DeleteReportedFalse { durable }),
            Err(source) => Err(UsrRollbackActiveReblitFinalizationError::DeleteReportedFalseAndVerification { source }),
        },
        Err(source) => match durable {
            Ok(durable) => Err(UsrRollbackActiveReblitFinalizationError::Delete { durable, source }),
            Err(verification) => Err(UsrRollbackActiveReblitFinalizationError::DeleteAndVerification {
                delete: source,
                verification,
            }),
        },
    }
}

/// Reconcile only the exact source or absence while retaining the original
/// lock. Both classifications are proven by phase-appropriate ActiveReblit
/// authority and surrounded by identical public-name inspections.
fn verify_exact_durable_state(
    installation: &Installation,
    journal: &TransitionJournalStore,
    authority: UsrRollbackActiveReblitFinalizationAuthority<'_>,
    expected: &TransitionRecord,
) -> Result<DurableUsrRollbackActiveReblitFinalizationRecord, UsrRollbackActiveReblitFinalizationVerificationError> {
    let durable = classify_exact_or_absent(expected, inspect_exact_public_record(installation, journal)?)?;
    match durable {
        DurableUsrRollbackActiveReblitFinalizationRecord::RollbackComplete => authority.revalidate(journal)?,
        DurableUsrRollbackActiveReblitFinalizationRecord::Absent => {
            authority.revalidate_after_journal_delete(journal)?
        }
    }
    before_usr_rollback_active_reblit_finalization_final_durable_inspection();
    let after = classify_exact_or_absent(expected, inspect_exact_public_record(installation, journal)?)?;
    if after == durable {
        Ok(durable)
    } else {
        Err(
            UsrRollbackActiveReblitFinalizationVerificationError::JournalChangedDuringVerification {
                before: durable,
                after,
            },
        )
    }
}

fn require_exact_public_source(
    installation: &Installation,
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackActiveReblitFinalizationVerificationError> {
    match inspect_exact_public_record(installation, journal)? {
        Some(actual) if actual == *expected => Ok(()),
        actual => Err(unexpected_record(expected, actual)),
    }
}

/// Read the canonical record only through a store which remains the exact
/// public `.cast/journal` and owns the exact public lock name before and after
/// that read.
fn inspect_exact_public_record(
    installation: &Installation,
    journal: &TransitionJournalStore,
) -> Result<Option<TransitionRecord>, UsrRollbackActiveReblitFinalizationVerificationError> {
    installation.revalidate_mutable_namespace()?;
    let cast = installation.retained_mutable_cast_directory()?;
    let record = journal.load_revalidated_retained_cast(cast)?;
    installation.revalidate_mutable_namespace()?;
    Ok(record)
}

fn classify_exact_or_absent(
    expected: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> Result<DurableUsrRollbackActiveReblitFinalizationRecord, UsrRollbackActiveReblitFinalizationVerificationError> {
    match actual {
        Some(actual) if actual == *expected => Ok(DurableUsrRollbackActiveReblitFinalizationRecord::RollbackComplete),
        None => Ok(DurableUsrRollbackActiveReblitFinalizationRecord::Absent),
        actual => Err(unexpected_record(expected, actual)),
    }
}

fn unexpected_record(
    expected: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackActiveReblitFinalizationVerificationError {
    UsrRollbackActiveReblitFinalizationVerificationError::UnexpectedRecord {
        expected_rollback_complete: Box::new(expected.clone()),
        actual: actual.map(Box::new),
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitFinalizationError {
    #[error("revalidate exact ActiveReblit RollbackComplete finalization authority")]
    Authority(#[source] UsrRollbackActiveReblitFinalizationAuthorityError),
    #[error(
        "ActiveReblit rollback finalization requires exact ActiveReblit RollbackComplete, got {operation:?} {phase:?}"
    )]
    UnexpectedSource { operation: Operation, phase: Phase },
    #[error("authenticate the exact public ActiveReblit journal source before terminal deletion")]
    PreDeleteVerification(#[source] UsrRollbackActiveReblitFinalizationVerificationError),
    #[error("terminal ActiveReblit journal deletion succeeded but exact RollbackComplete was present afterward")]
    DeleteSucceededButRecordPresent,
    #[error("verify exact public absence after successful ActiveReblit terminal deletion")]
    PostDeleteVerification(#[source] UsrRollbackActiveReblitFinalizationVerificationError),
    #[error("terminal ActiveReblit journal deletion reported false; same-store verification proved {durable:?}")]
    DeleteReportedFalse {
        durable: DurableUsrRollbackActiveReblitFinalizationRecord,
    },
    #[error("terminal ActiveReblit journal deletion reported false and same-store verification was ambiguous")]
    DeleteReportedFalseAndVerification {
        #[source]
        source: UsrRollbackActiveReblitFinalizationVerificationError,
    },
    #[error("terminal ActiveReblit journal deletion failed after same-store verification proved {durable:?}")]
    Delete {
        durable: DurableUsrRollbackActiveReblitFinalizationRecord,
        #[source]
        source: StorageError,
    },
    #[error("terminal ActiveReblit journal deletion failed ({delete}) and same-store verification was ambiguous")]
    DeleteAndVerification {
        delete: StorageError,
        #[source]
        verification: UsrRollbackActiveReblitFinalizationVerificationError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitFinalizationVerificationError {
    #[error("revalidate retained installation around same-store ActiveReblit journal inspection")]
    Installation(#[from] installation::Error),
    #[error("authenticate or load the same retained public ActiveReblit journal store")]
    Journal(#[from] StorageError),
    #[error("revalidate phase-appropriate ActiveReblit rollback-finalization authority")]
    Authority(#[from] UsrRollbackActiveReblitFinalizationAuthorityError),
    #[error(
        "same-store ActiveReblit journal is neither absent nor exact RollbackComplete (rollback_complete={expected_rollback_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_rollback_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
    #[error("same-store ActiveReblit journal changed during verification ({before:?} -> {after:?})")]
    JournalChangedDuringVerification {
        before: DurableUsrRollbackActiveReblitFinalizationRecord,
        after: DurableUsrRollbackActiveReblitFinalizationRecord,
    },
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_AUTHORITY_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_DELETE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_FINAL_DURABLE_INSPECTION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_active_reblit_finalization_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(crate) fn arm_after_usr_rollback_active_reblit_finalization_delete(hook: impl FnOnce() + 'static) {
    AFTER_DELETE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_active_reblit_finalization_final_durable_inspection(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_DURABLE_INSPECTION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_active_reblit_finalization_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_active_reblit_finalization_final_revalidation() {}

#[cfg(test)]
fn after_usr_rollback_active_reblit_finalization_delete() {
    AFTER_DELETE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_active_reblit_finalization_delete() {}

#[cfg(test)]
fn before_usr_rollback_active_reblit_finalization_final_durable_inspection() {
    BEFORE_FINAL_DURABLE_INSPECTION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_active_reblit_finalization_final_durable_inspection() {}
