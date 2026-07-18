//! Delete one exact terminal ActivateArchived rollback journal in place.
//!
//! The same public journal directory and exclusive lock are authenticated
//! around two final PRE checks and one retained-store conditional deletion.
//! Success requires a reported deletion, consumed post-delete authority, and
//! repeated authenticated absence through that same store. No reopen,
//! namespace repair, database mutation, cleanup, trigger, or retry is
//! reachable here.

use thiserror::Error;

use crate::{
    Installation, installation,
    transition_journal::{Operation, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{
    UsrRollbackActivateArchivedFinalizationAuthority, UsrRollbackActivateArchivedFinalizationAuthorityError,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackActivateArchivedFinalizationRecord {
    RollbackComplete,
    Absent,
}

pub(in crate::client) fn finalize_usr_rollback_activate_archived(
    journal: TransitionJournalStore,
    authority: UsrRollbackActivateArchivedFinalizationAuthority<'_>,
) -> Result<TransitionJournalStore, UsrRollbackActivateArchivedFinalizationError> {
    authority
        .revalidate(&journal)
        .map_err(UsrRollbackActivateArchivedFinalizationError::Authority)?;

    let source_record = authority.record().clone();
    if source_record.operation != Operation::ActivateArchived || source_record.phase != Phase::RollbackComplete {
        return Err(UsrRollbackActivateArchivedFinalizationError::UnexpectedSource {
            operation: source_record.operation,
            phase: source_record.phase,
        });
    }

    let installation = authority.installation().clone();
    require_exact_public_source(&installation, &journal, &source_record)
        .map_err(UsrRollbackActivateArchivedFinalizationError::PreDeleteVerification)?;

    before_usr_rollback_activate_archived_finalization_final_revalidation();
    authority
        .revalidate(&journal)
        .map_err(UsrRollbackActivateArchivedFinalizationError::Authority)?;
    require_exact_public_source(&installation, &journal, &source_record)
        .map_err(UsrRollbackActivateArchivedFinalizationError::PreDeleteVerification)?;

    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(UsrRollbackActivateArchivedFinalizationVerificationError::from)
        .map_err(UsrRollbackActivateArchivedFinalizationError::PreDeleteVerification)?;
    let delete = journal.delete_revalidated_retained_cast(cast, &source_record);
    after_usr_rollback_activate_archived_finalization_delete();

    reconcile_delete(delete, journal, authority, &installation, source_record)
}

fn reconcile_delete(
    delete: Result<bool, StorageError>,
    journal: TransitionJournalStore,
    authority: UsrRollbackActivateArchivedFinalizationAuthority<'_>,
    installation: &Installation,
    source_record: TransitionRecord,
) -> Result<TransitionJournalStore, UsrRollbackActivateArchivedFinalizationError> {
    let durable = verify_exact_durable_state(installation, &journal, authority, &source_record);
    match delete {
        Ok(true) => match durable {
            Ok(DurableUsrRollbackActivateArchivedFinalizationRecord::Absent) => Ok(journal),
            Ok(DurableUsrRollbackActivateArchivedFinalizationRecord::RollbackComplete) => {
                Err(UsrRollbackActivateArchivedFinalizationError::DeleteSucceededButRecordPresent)
            }
            Err(source) => Err(UsrRollbackActivateArchivedFinalizationError::PostDeleteVerification(
                source,
            )),
        },
        Ok(false) => match durable {
            Ok(durable) => Err(UsrRollbackActivateArchivedFinalizationError::DeleteReportedFalse { durable }),
            Err(source) => {
                Err(UsrRollbackActivateArchivedFinalizationError::DeleteReportedFalseAndVerification { source })
            }
        },
        Err(source) => match durable {
            Ok(durable) => Err(UsrRollbackActivateArchivedFinalizationError::Delete { durable, source }),
            Err(verification) => Err(UsrRollbackActivateArchivedFinalizationError::DeleteAndVerification {
                delete: source,
                verification,
            }),
        },
    }
}

fn verify_exact_durable_state(
    installation: &Installation,
    journal: &TransitionJournalStore,
    authority: UsrRollbackActivateArchivedFinalizationAuthority<'_>,
    expected: &TransitionRecord,
) -> Result<
    DurableUsrRollbackActivateArchivedFinalizationRecord,
    UsrRollbackActivateArchivedFinalizationVerificationError,
> {
    let durable = classify_exact_or_absent(expected, inspect_exact_public_record(installation, journal)?)?;
    match durable {
        DurableUsrRollbackActivateArchivedFinalizationRecord::RollbackComplete => authority.revalidate(journal)?,
        DurableUsrRollbackActivateArchivedFinalizationRecord::Absent => {
            authority.revalidate_after_journal_delete(journal)?
        }
    }
    before_usr_rollback_activate_archived_finalization_final_durable_inspection();
    let after = classify_exact_or_absent(expected, inspect_exact_public_record(installation, journal)?)?;
    if after == durable {
        Ok(durable)
    } else {
        Err(
            UsrRollbackActivateArchivedFinalizationVerificationError::JournalChangedDuringVerification {
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
) -> Result<(), UsrRollbackActivateArchivedFinalizationVerificationError> {
    match inspect_exact_public_record(installation, journal)? {
        Some(actual) if actual == *expected => Ok(()),
        actual => Err(unexpected_record(expected, actual)),
    }
}

fn inspect_exact_public_record(
    installation: &Installation,
    journal: &TransitionJournalStore,
) -> Result<Option<TransitionRecord>, UsrRollbackActivateArchivedFinalizationVerificationError> {
    installation.revalidate_mutable_namespace()?;
    let cast = installation.retained_mutable_cast_directory()?;
    let record = journal.load_revalidated_retained_cast(cast)?;
    installation.revalidate_mutable_namespace()?;
    Ok(record)
}

fn classify_exact_or_absent(
    expected: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> Result<
    DurableUsrRollbackActivateArchivedFinalizationRecord,
    UsrRollbackActivateArchivedFinalizationVerificationError,
> {
    match actual {
        Some(actual) if actual == *expected => {
            Ok(DurableUsrRollbackActivateArchivedFinalizationRecord::RollbackComplete)
        }
        None => Ok(DurableUsrRollbackActivateArchivedFinalizationRecord::Absent),
        actual => Err(unexpected_record(expected, actual)),
    }
}

fn unexpected_record(
    expected: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackActivateArchivedFinalizationVerificationError {
    UsrRollbackActivateArchivedFinalizationVerificationError::UnexpectedRecord {
        expected_rollback_complete: Box::new(expected.clone()),
        actual: actual.map(Box::new),
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActivateArchivedFinalizationError {
    #[error("revalidate exact ActivateArchived RollbackComplete finalization authority")]
    Authority(#[source] UsrRollbackActivateArchivedFinalizationAuthorityError),
    #[error(
        "ActivateArchived rollback finalization requires exact ActivateArchived RollbackComplete, got {operation:?} {phase:?}"
    )]
    UnexpectedSource { operation: Operation, phase: Phase },
    #[error("authenticate the exact public ActivateArchived journal source before terminal deletion")]
    PreDeleteVerification(#[source] UsrRollbackActivateArchivedFinalizationVerificationError),
    #[error("terminal ActivateArchived journal deletion succeeded but exact RollbackComplete was present afterward")]
    DeleteSucceededButRecordPresent,
    #[error("verify exact public absence after successful ActivateArchived terminal deletion")]
    PostDeleteVerification(#[source] UsrRollbackActivateArchivedFinalizationVerificationError),
    #[error("terminal ActivateArchived journal deletion reported false; same-store verification proved {durable:?}")]
    DeleteReportedFalse {
        durable: DurableUsrRollbackActivateArchivedFinalizationRecord,
    },
    #[error("terminal ActivateArchived journal deletion reported false and same-store verification was ambiguous")]
    DeleteReportedFalseAndVerification {
        #[source]
        source: UsrRollbackActivateArchivedFinalizationVerificationError,
    },
    #[error("terminal ActivateArchived journal deletion failed after same-store verification proved {durable:?}")]
    Delete {
        durable: DurableUsrRollbackActivateArchivedFinalizationRecord,
        #[source]
        source: StorageError,
    },
    #[error("terminal ActivateArchived journal deletion failed ({delete}) and same-store verification was ambiguous")]
    DeleteAndVerification {
        delete: StorageError,
        #[source]
        verification: UsrRollbackActivateArchivedFinalizationVerificationError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActivateArchivedFinalizationVerificationError {
    #[error("revalidate retained installation around same-store ActivateArchived journal inspection")]
    Installation(#[from] installation::Error),
    #[error("authenticate or load the same retained public ActivateArchived journal store")]
    Journal(#[from] StorageError),
    #[error("revalidate phase-appropriate ActivateArchived rollback-finalization authority")]
    Authority(#[from] UsrRollbackActivateArchivedFinalizationAuthorityError),
    #[error(
        "same-store ActivateArchived journal is neither absent nor exact RollbackComplete (rollback_complete={expected_rollback_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_rollback_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
    #[error("same-store ActivateArchived journal changed during verification ({before:?} -> {after:?})")]
    JournalChangedDuringVerification {
        before: DurableUsrRollbackActivateArchivedFinalizationRecord,
        after: DurableUsrRollbackActivateArchivedFinalizationRecord,
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
pub(crate) fn arm_before_usr_rollback_activate_archived_finalization_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(crate) fn arm_after_usr_rollback_activate_archived_finalization_delete(hook: impl FnOnce() + 'static) {
    AFTER_DELETE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_activate_archived_finalization_final_durable_inspection(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_DURABLE_INSPECTION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_activate_archived_finalization_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_activate_archived_finalization_final_revalidation() {}

#[cfg(test)]
fn after_usr_rollback_activate_archived_finalization_delete() {
    AFTER_DELETE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_usr_rollback_activate_archived_finalization_delete() {}

#[cfg(test)]
fn before_usr_rollback_activate_archived_finalization_final_durable_inspection() {
    BEFORE_FINAL_DURABLE_INSPECTION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_activate_archived_finalization_final_durable_inspection() {}
