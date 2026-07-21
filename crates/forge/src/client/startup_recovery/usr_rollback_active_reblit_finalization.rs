//! Delete one exact terminal ActiveReblit rollback journal in place.
//!
//! The same public journal directory and exclusive lock are retained from
//! exact-inode admission, through one consuming bound deletion, into the
//! shared clean-startup proof. No semantic deletion fallback, reopen, retry,
//! namespace repair, database mutation, cleanup, trigger, boot action, or
//! journal advance is reachable here.

use thiserror::Error;

use crate::transition_journal::{
    Operation, Phase, TransitionJournalRecordDeleteError, TransitionJournalRecordDeleteState,
    TransitionJournalStore,
};

use super::super::startup_reconciliation::{
    UsrRollbackActiveReblitFinalizationAfterDeleteAuthority, UsrRollbackActiveReblitFinalizationAuthority,
    UsrRollbackActiveReblitFinalizationAuthorityError,
};

#[cfg(test)]
mod tests;

/// Consume exact terminal authority, delete once, and return the same
/// continuously locked store only after proving exact public absence.
pub(in crate::client) fn finalize_usr_rollback_active_reblit(
    journal: TransitionJournalStore,
    authority: UsrRollbackActiveReblitFinalizationAuthority<'_>,
) -> Result<TransitionJournalStore, UsrRollbackActiveReblitFinalizationError> {
    authority
        .revalidate(&journal)
        .map_err(UsrRollbackActiveReblitFinalizationError::Authority)?;

    let source_record = authority.record();
    if source_record.operation != Operation::ActiveReblit || source_record.phase != Phase::RollbackComplete {
        return Err(UsrRollbackActiveReblitFinalizationError::UnexpectedSource {
            operation: source_record.operation,
            phase: source_record.phase,
        });
    }

    before_usr_rollback_active_reblit_finalization_final_revalidation();
    let (delete, after_delete) = authority
        .attempt_record_bound_delete(&journal)
        .map_err(UsrRollbackActiveReblitFinalizationError::Authority)?;
    after_usr_rollback_active_reblit_finalization_delete();

    reconcile_bound_delete(delete, journal, after_delete)
}

fn reconcile_bound_delete(
    delete: Result<(), TransitionJournalRecordDeleteError>,
    journal: TransitionJournalStore,
    after_delete: UsrRollbackActiveReblitFinalizationAfterDeleteAuthority<'_>,
) -> Result<TransitionJournalStore, UsrRollbackActiveReblitFinalizationError> {
    match delete {
        Ok(()) => {
            after_delete
                .revalidate_after_journal_delete(&journal)
                .map_err(UsrRollbackActiveReblitFinalizationError::PostDeleteAuthority)?;
            Ok(journal)
        }
        Err(delete @ TransitionJournalRecordDeleteError::Storage {
            state: TransitionJournalRecordDeleteState::Absent,
            ..
        }) => match after_delete.revalidate_after_journal_delete(&journal) {
            Ok(()) => Err(UsrRollbackActiveReblitFinalizationError::Delete(delete)),
            Err(verification) => Err(
                UsrRollbackActiveReblitFinalizationError::DeleteAndPostDeleteAuthority {
                    delete,
                    verification,
                },
            ),
        },
        Err(source) => Err(UsrRollbackActiveReblitFinalizationError::Delete(source)),
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
    #[error("delete the exact retained ActiveReblit terminal journal inode")]
    Delete(#[source] TransitionJournalRecordDeleteError),
    #[error("revalidate exact ActiveReblit evidence and public absence after terminal deletion")]
    PostDeleteAuthority(#[source] UsrRollbackActiveReblitFinalizationAuthorityError),
    #[error("exact ActiveReblit terminal deletion failed ({delete}) and post-delete absence evidence also failed")]
    DeleteAndPostDeleteAuthority {
        delete: TransitionJournalRecordDeleteError,
        #[source]
        verification: UsrRollbackActiveReblitFinalizationAuthorityError,
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
