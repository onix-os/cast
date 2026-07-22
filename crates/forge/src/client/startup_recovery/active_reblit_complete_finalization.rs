//! Delete one exact forward ActiveReblit `Complete` journal in place.
//!
//! The same lock-bearing store is retained through one consuming bound
//! deletion and post-delete receipt, database, selected-state, namespace, and
//! public-absence proof. There is no reopen, retry, residue repair, boot work,
//! cleanup effect, trigger, database mutation, or journal advance here.

use thiserror::Error;

use crate::transition_journal::{
    Operation, Phase, TransitionJournalRecordDeleteError,
    TransitionJournalRecordDeleteState, TransitionJournalStore,
};

use super::super::startup_reconciliation::{
    ActiveReblitCompleteFinalizationAfterDeleteAuthority,
    ActiveReblitCompleteFinalizationAuthority,
    ActiveReblitCompleteFinalizationAuthorityError,
};

/// Consume exact terminal authority, delete once, and return the same locked
/// store only after the full post-delete proof succeeds.
pub(in crate::client) fn finalize_active_reblit_complete(
    journal: TransitionJournalStore,
    authority: ActiveReblitCompleteFinalizationAuthority<'_>,
) -> Result<TransitionJournalStore, ActiveReblitCompleteFinalizationError> {
    authority
        .revalidate(&journal)
        .map_err(ActiveReblitCompleteFinalizationError::Authority)?;

    let source = authority.record();
    if source.operation != Operation::ActiveReblit || source.phase != Phase::Complete {
        return Err(ActiveReblitCompleteFinalizationError::UnexpectedSource {
            operation: source.operation,
            phase: source.phase,
        });
    }

    before_active_reblit_complete_finalization_final_revalidation();
    let (delete, after_delete) = authority
        .attempt_record_bound_delete(&journal)
        .map_err(ActiveReblitCompleteFinalizationError::Authority)?;
    after_active_reblit_complete_finalization_delete();
    reconcile_bound_delete(delete, journal, after_delete)
}

fn reconcile_bound_delete(
    delete: Result<(), TransitionJournalRecordDeleteError>,
    journal: TransitionJournalStore,
    after_delete: ActiveReblitCompleteFinalizationAfterDeleteAuthority<'_>,
) -> Result<TransitionJournalStore, ActiveReblitCompleteFinalizationError> {
    match delete {
        Ok(()) => {
            after_delete
                .revalidate_after_journal_delete(&journal)
                .map_err(ActiveReblitCompleteFinalizationError::PostDeleteAuthority)?;
            Ok(journal)
        }
        Err(delete @ TransitionJournalRecordDeleteError::Storage {
            state: TransitionJournalRecordDeleteState::Absent,
            ..
        }) => match after_delete.revalidate_after_journal_delete(&journal) {
            Ok(()) => Err(ActiveReblitCompleteFinalizationError::Delete(delete)),
            Err(verification) => Err(
                ActiveReblitCompleteFinalizationError::DeleteAndPostDeleteAuthority {
                    delete,
                    verification,
                },
            ),
        },
        Err(source) => Err(ActiveReblitCompleteFinalizationError::Delete(source)),
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitCompleteFinalizationError {
    #[error("revalidate exact forward ActiveReblit Complete finalization authority")]
    Authority(#[source] ActiveReblitCompleteFinalizationAuthorityError),
    #[error(
        "forward ActiveReblit finalization requires exact ActiveReblit Complete, got {operation:?} {phase:?}"
    )]
    UnexpectedSource { operation: Operation, phase: Phase },
    #[error("delete the exact retained forward ActiveReblit Complete journal inode")]
    Delete(#[source] TransitionJournalRecordDeleteError),
    #[error("revalidate exact forward ActiveReblit evidence and public absence after terminal deletion")]
    PostDeleteAuthority(#[source] ActiveReblitCompleteFinalizationAuthorityError),
    #[error("forward ActiveReblit terminal deletion failed ({delete}) and post-delete evidence also failed")]
    DeleteAndPostDeleteAuthority {
        delete: TransitionJournalRecordDeleteError,
        #[source]
        verification: ActiveReblitCompleteFinalizationAuthorityError,
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
pub(crate) fn arm_before_active_reblit_complete_finalization_final_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(crate) fn arm_after_active_reblit_complete_finalization_delete(
    hook: impl FnOnce() + 'static,
) {
    AFTER_DELETE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_active_reblit_complete_finalization_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_active_reblit_complete_finalization_final_revalidation() {}

#[cfg(test)]
fn after_active_reblit_complete_finalization_delete() {
    AFTER_DELETE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_active_reblit_complete_finalization_delete() {}
