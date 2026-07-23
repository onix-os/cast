//! Persist the journal-only ActiveReblit `BootRepairComplete` to
//! `RollbackComplete` route.
//!
//! The executor handles only the successful boot-repair checkpoint observed at
//! startup entry. It performs one conditional journal advance, invokes boot
//! zero times, and returns immediately without finalization or deletion.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{BootRollback, CodecError, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{
    UsrRollbackActiveReblitBootRepairCompleteAuthority, UsrRollbackActiveReblitBootRepairCompleteAuthorityError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackActiveReblitBootRepairCompleteRecord {
    BootRepairComplete,
    RollbackComplete,
}

pub(in crate::client) fn persist_usr_rollback_active_reblit_boot_repair_complete_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackActiveReblitBootRepairCompleteAuthority<'_, '_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackActiveReblitBootRepairCompletePersistenceError> {
    authority.revalidate(&journal)?;
    let source_record = authority.record().clone();
    let source_boot = source_record.rollback.as_ref().map(|rollback| rollback.boot);
    let successor = authority.rollback_complete_successor()?;
    let successor_boot = successor.rollback.as_ref().map(|rollback| rollback.boot);
    if successor.phase != Phase::RollbackComplete
        || !matches!(successor_boot, Some(BootRollback::Applied | BootRollback::AlreadySatisfied))
        || successor_boot != source_boot
    {
        drop(authority);
        drop(journal);
        return Err(
            UsrRollbackActiveReblitBootRepairCompletePersistenceError::UnexpectedSuccessor {
                phase: successor.phase,
                boot: successor_boot,
            },
        );
    }

    before_usr_rollback_active_reblit_boot_repair_complete_final_revalidation();
    authority.revalidate(&journal)?;
    let installation = authority.installation().clone();
    let advance = journal.advance(&source_record, &successor);
    drop(authority);
    drop(journal);

    let reopened = reopen_canonical_journal(&installation)
        .map_err(UsrRollbackActiveReblitBootRepairCompleteReopenError::from);
    match advance {
        Ok(()) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitBootRepairCompletePersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => Err(
                UsrRollbackActiveReblitBootRepairCompletePersistenceError::ReopenAfterSuccessfulAdvance { source },
            ),
        },
        Err(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackActiveReblitBootRepairCompletePersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitBootRepairCompleteRecord::BootRepairComplete,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackActiveReblitBootRepairCompletePersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitBootRepairCompleteRecord::RollbackComplete,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitBootRepairCompletePersistenceError::AdvanceAndReopen {
                        advance: advance_error,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackActiveReblitBootRepairCompletePersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen,
                },
            ),
        },
    }
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackActiveReblitBootRepairCompleteReopenError {
    UsrRollbackActiveReblitBootRepairCompleteReopenError::UnexpectedRecord {
        expected_complete: Box::new(source.clone()),
        expected_rollback_complete: Box::new(successor.clone()),
        actual: actual.map(Box::new),
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_AUTHORITY_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_active_reblit_boot_repair_complete_final_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_active_reblit_boot_repair_complete_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_active_reblit_boot_repair_complete_final_revalidation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitBootRepairCompletePersistenceError {
    #[error("revalidate exact ActiveReblit BootRepairComplete authority")]
    Authority(#[from] UsrRollbackActiveReblitBootRepairCompleteAuthorityError),
    #[error("derive the sole legal ActiveReblit RollbackComplete successor after verified boot repair")]
    RouteConstruction(#[from] CodecError),
    #[error("ActiveReblit successful boot-repair route selected unexpected successor phase {phase:?} and boot state {boot:?}")]
    UnexpectedSuccessor { phase: Phase, boot: Option<BootRollback> },
    #[error("ActiveReblit BootRepairComplete route failed after reopening exact durable {durable:?}")]
    Advance {
        durable: DurableUsrRollbackActiveReblitBootRepairCompleteRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen exact canonical RollbackComplete after a successful ActiveReblit boot-repair route")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackActiveReblitBootRepairCompleteReopenError,
    },
    #[error("ActiveReblit BootRepairComplete route failed ({advance}) and canonical reopen was inconclusive")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackActiveReblitBootRepairCompleteReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitBootRepairCompleteReopenError {
    #[error("revalidate retained installation around ActiveReblit BootRepairComplete reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load descriptor-rooted canonical ActiveReblit BootRepairComplete journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither exact BootRepairComplete nor RollbackComplete (complete={expected_complete:?}, rollback_complete={expected_rollback_complete:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_complete: Box<TransitionRecord>,
        expected_rollback_complete: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackActiveReblitBootRepairCompleteReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
