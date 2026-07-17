//! Delete one exact terminal NewState rollback journal in place.
//!
//! The supplied authority retains exact jointly-absent database evidence and
//! the exact preserved-candidate namespace. This boundary authenticates the
//! same public journal directory and exclusive lock around its final
//! authority validation, performs one conditional terminal deletion, and
//! keeps that lock-bearing store alive. Success requires a reported deletion,
//! consumed post-delete database and namespace authority, and repeated public
//! absence through that same store. No journal reopen, namespace repair,
//! cleanup, database mutation, trigger, or retry is reachable here.

use thiserror::Error;

use crate::{
    Installation, installation,
    transition_journal::{Operation, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{UsrRollbackFinalizationAuthority, UsrRollbackFinalizationAuthorityError};

#[cfg(test)]
#[allow(dead_code)] // shared candidate fixture contains wider preservation helpers
#[path = "../startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs"]
mod candidate_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared invalidation fixture contains wider effect helpers
#[path = "../startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/tests/support.rs"]
mod invalidation_test_support;
#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider startup-recovery helpers
#[path = "test_support.rs"]
mod test_fixture;
#[cfg(test)]
mod tests;

/// Which exact same-store public canonical state survived a deletion attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackFinalizationRecord {
    RollbackComplete,
    Absent,
}

/// Consume exact terminal authority, delete once, and return the same
/// continuously locked store only after proving exact public absence.
pub(in crate::client) fn finalize_usr_rollback(
    journal: TransitionJournalStore,
    authority: UsrRollbackFinalizationAuthority<'_>,
) -> Result<TransitionJournalStore, UsrRollbackFinalizationError> {
    authority
        .revalidate(&journal)
        .map_err(UsrRollbackFinalizationError::Authority)?;

    let source_record = authority.record().clone();
    if source_record.operation != Operation::NewState || source_record.phase != Phase::RollbackComplete {
        return Err(UsrRollbackFinalizationError::UnexpectedSource {
            operation: source_record.operation,
            phase: source_record.phase,
        });
    }

    let installation = authority.installation().clone();
    require_exact_public_source(&installation, &journal, &source_record)
        .map_err(UsrRollbackFinalizationError::PreDeleteVerification)?;

    // Every deterministic test race is injected before this final PRE. There
    // is deliberately no callback or other optional work between the final
    // authority/public-binding checks and the single delete attempt.
    before_usr_rollback_finalization_final_revalidation();
    authority
        .revalidate(&journal)
        .map_err(UsrRollbackFinalizationError::Authority)?;
    require_exact_public_source(&installation, &journal, &source_record)
        .map_err(UsrRollbackFinalizationError::PreDeleteVerification)?;

    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackFinalizationVerificationError::from)
        .map_err(UsrRollbackFinalizationError::PreDeleteVerification)?;
    let delete = journal.delete_revalidated_retained_cast(cast, &source_record);
    after_usr_rollback_finalization_delete();

    reconcile_delete(delete, journal, authority, &installation, source_record)
}

fn reconcile_delete(
    delete: Result<bool, StorageError>,
    journal: TransitionJournalStore,
    authority: UsrRollbackFinalizationAuthority<'_>,
    installation: &Installation,
    source_record: TransitionRecord,
) -> Result<TransitionJournalStore, UsrRollbackFinalizationError> {
    let durable = verify_exact_durable_state(installation, &journal, authority, &source_record);
    match delete {
        Ok(true) => match durable {
            Ok(DurableUsrRollbackFinalizationRecord::Absent) => Ok(journal),
            Ok(DurableUsrRollbackFinalizationRecord::RollbackComplete) => {
                Err(UsrRollbackFinalizationError::DeleteSucceededButRecordPresent)
            }
            Err(source) => Err(UsrRollbackFinalizationError::PostDeleteVerification(source)),
        },
        Ok(false) => match durable {
            Ok(durable) => Err(UsrRollbackFinalizationError::DeleteReportedFalse { durable }),
            Err(source) => Err(UsrRollbackFinalizationError::DeleteReportedFalseAndVerification { source }),
        },
        Err(source) => match durable {
            Ok(durable) => Err(UsrRollbackFinalizationError::Delete { durable, source }),
            Err(verification) => Err(UsrRollbackFinalizationError::DeleteAndVerification {
                delete: source,
                verification,
            }),
        },
    }
}

/// Reconcile only exact source or absence while retaining the original lock.
/// Both classifications are proven by their phase-appropriate authority and
/// surrounded by identical public-name inspections.
fn verify_exact_durable_state(
    installation: &Installation,
    journal: &TransitionJournalStore,
    authority: UsrRollbackFinalizationAuthority<'_>,
    expected: &TransitionRecord,
) -> Result<DurableUsrRollbackFinalizationRecord, UsrRollbackFinalizationVerificationError> {
    let durable = classify_exact_or_absent(expected, inspect_exact_public_record(installation, journal)?)?;
    match durable {
        DurableUsrRollbackFinalizationRecord::RollbackComplete => authority.revalidate(journal)?,
        DurableUsrRollbackFinalizationRecord::Absent => authority.revalidate_after_journal_delete(journal)?,
    }
    before_usr_rollback_finalization_final_durable_inspection();
    let after = classify_exact_or_absent(expected, inspect_exact_public_record(installation, journal)?)?;
    if after == durable {
        Ok(durable)
    } else {
        Err(UsrRollbackFinalizationVerificationError::JournalChangedDuringVerification { before: durable, after })
    }
}

fn require_exact_public_source(
    installation: &Installation,
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackFinalizationVerificationError> {
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
) -> Result<Option<TransitionRecord>, UsrRollbackFinalizationVerificationError> {
    installation.revalidate_mutable_namespace()?;
    let cast = installation.retained_mutable_cast_directory()?;
    let record = journal.load_revalidated_retained_cast(cast)?;
    installation.revalidate_mutable_namespace()?;
    Ok(record)
}

fn classify_exact_or_absent(
    expected: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> Result<DurableUsrRollbackFinalizationRecord, UsrRollbackFinalizationVerificationError> {
    match actual {
        Some(actual) if actual == *expected => Ok(DurableUsrRollbackFinalizationRecord::RollbackComplete),
        None => Ok(DurableUsrRollbackFinalizationRecord::Absent),
        actual => Err(unexpected_record(expected, actual)),
    }
}

fn unexpected_record(
    expected: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackFinalizationVerificationError {
    UsrRollbackFinalizationVerificationError::UnexpectedRecord {
        expected_rollback_complete: Box::new(expected.clone()),
        actual: actual.map(Box::new),
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackFinalizationError {
    #[error("revalidate exact RollbackComplete finalization authority")]
    Authority(#[source] UsrRollbackFinalizationAuthorityError),
    #[error("rollback finalization requires exact NewState RollbackComplete, got {operation:?} {phase:?}")]
    UnexpectedSource { operation: Operation, phase: Phase },
    #[error("authenticate the exact public journal source before terminal deletion")]
    PreDeleteVerification(#[source] UsrRollbackFinalizationVerificationError),
    #[error("terminal journal deletion succeeded but exact RollbackComplete was present afterward")]
    DeleteSucceededButRecordPresent,
    #[error("verify exact public absence after successful terminal deletion")]
    PostDeleteVerification(#[source] UsrRollbackFinalizationVerificationError),
    #[error("terminal journal deletion reported false; same-store verification proved {durable:?}")]
    DeleteReportedFalse {
        durable: DurableUsrRollbackFinalizationRecord,
    },
    #[error("terminal journal deletion reported false and same-store verification was ambiguous")]
    DeleteReportedFalseAndVerification {
        #[source]
        source: UsrRollbackFinalizationVerificationError,
    },
    #[error("terminal journal deletion failed after same-store verification proved {durable:?}")]
    Delete {
        durable: DurableUsrRollbackFinalizationRecord,
        #[source]
        source: StorageError,
    },
    #[error("terminal journal deletion failed ({delete}) and same-store verification was ambiguous")]
    DeleteAndVerification {
        delete: StorageError,
        #[source]
        verification: UsrRollbackFinalizationVerificationError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackFinalizationVerificationError {
    #[error("revalidate retained installation around same-store journal inspection")]
    Installation(#[from] installation::Error),
    #[error("authenticate or load the same retained public journal store")]
    Journal(#[from] StorageError),
    #[error("revalidate phase-appropriate rollback-finalization authority")]
    Authority(#[from] UsrRollbackFinalizationAuthorityError),
    #[error(
        "same-store journal is neither absent nor exact RollbackComplete (rollback_complete={expected_rollback_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_rollback_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
    #[error("same-store journal changed during verification ({before:?} -> {after:?})")]
    JournalChangedDuringVerification {
        before: DurableUsrRollbackFinalizationRecord,
        after: DurableUsrRollbackFinalizationRecord,
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
pub(crate) fn arm_before_usr_rollback_finalization_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(crate) fn arm_after_usr_rollback_finalization_delete(hook: impl FnOnce() + 'static) {
    AFTER_DELETE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_finalization_final_durable_inspection(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_DURABLE_INSPECTION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_finalization_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_finalization_final_revalidation() {}

#[cfg(test)]
fn after_usr_rollback_finalization_delete() {
    AFTER_DELETE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_finalization_delete() {}

#[cfg(test)]
fn before_usr_rollback_finalization_final_durable_inspection() {
    BEFORE_FINAL_DURABLE_INSPECTION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_finalization_final_durable_inspection() {}
