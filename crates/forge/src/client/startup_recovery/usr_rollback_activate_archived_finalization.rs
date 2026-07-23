//! Delete one exact terminal ActivateArchived rollback journal in place.
//!
//! The same public journal directory and exclusive lock are retained from
//! exact-inode admission, through one consuming bound deletion, into the
//! shared clean-startup proof. No semantic deletion fallback, reopen, retry,
//! namespace repair, database mutation, cleanup, trigger, or journal advance
//! is reachable here.

use thiserror::Error;

use crate::transition_journal::{
    Operation, Phase, TransitionJournalRecordDeleteError, TransitionJournalRecordDeleteState,
    TransitionJournalStore,
};

use super::super::startup_reconciliation::{
    UsrRollbackActivateArchivedFinalizationAfterDeleteAuthority,
    UsrRollbackActivateArchivedFinalizationAuthority, UsrRollbackActivateArchivedFinalizationAuthorityError,
};

#[cfg(test)]
mod tests;

pub(in crate::client) fn finalize_usr_rollback_activate_archived(
    journal: TransitionJournalStore,
    authority: UsrRollbackActivateArchivedFinalizationAuthority<'_>,
) -> Result<TransitionJournalStore, UsrRollbackActivateArchivedFinalizationError> {
    authority
        .revalidate(&journal)
        .map_err(UsrRollbackActivateArchivedFinalizationError::Authority)?;

    let source_record = authority.record();
    if source_record.operation != Operation::ActivateArchived || source_record.phase != Phase::RollbackComplete {
        return Err(UsrRollbackActivateArchivedFinalizationError::UnexpectedSource {
            operation: source_record.operation,
            phase: source_record.phase,
        });
    }

    before_usr_rollback_activate_archived_finalization_final_revalidation();
    let (delete, after_delete) = authority
        .attempt_record_bound_delete(&journal)
        .map_err(UsrRollbackActivateArchivedFinalizationError::Authority)?;
    after_usr_rollback_activate_archived_finalization_delete();

    reconcile_bound_delete(delete, journal, after_delete)
}

fn reconcile_bound_delete(
    delete: Result<(), TransitionJournalRecordDeleteError>,
    journal: TransitionJournalStore,
    after_delete: UsrRollbackActivateArchivedFinalizationAfterDeleteAuthority<'_>,
) -> Result<TransitionJournalStore, UsrRollbackActivateArchivedFinalizationError> {
    match delete {
        Ok(()) => {
            after_delete
                .revalidate_after_journal_delete(&journal)
                .map_err(UsrRollbackActivateArchivedFinalizationError::PostDeleteAuthority)?;
            Ok(journal)
        }
        Err(delete @ TransitionJournalRecordDeleteError::Storage {
            state: TransitionJournalRecordDeleteState::Absent,
            ..
        }) => match after_delete.revalidate_after_journal_delete(&journal) {
            Ok(()) => Err(UsrRollbackActivateArchivedFinalizationError::Delete(delete)),
            Err(verification) => Err(
                UsrRollbackActivateArchivedFinalizationError::DeleteAndPostDeleteAuthority {
                    delete,
                    verification,
                },
            ),
        },
        Err(source) => Err(UsrRollbackActivateArchivedFinalizationError::Delete(source)),
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
    #[error("delete the exact retained ActivateArchived terminal journal inode")]
    Delete(#[source] TransitionJournalRecordDeleteError),
    #[error("revalidate exact ActivateArchived evidence and public absence after terminal deletion")]
    PostDeleteAuthority(#[source] UsrRollbackActivateArchivedFinalizationAuthorityError),
    #[error(
        "exact ActivateArchived terminal deletion failed ({delete}) and post-delete absence evidence also failed"
    )]
    DeleteAndPostDeleteAuthority {
        delete: TransitionJournalRecordDeleteError,
        #[source]
        verification: UsrRollbackActivateArchivedFinalizationAuthorityError,
    },
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_AUTHORITY_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_DELETE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_activate_archived_finalization_final_revalidation(
    hook: impl FnOnce() + 'static,
) {
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
