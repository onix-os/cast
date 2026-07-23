//! Persist the journal-only ActiveReblit route from `CandidatePreserved` to
//! `BootRepairRequired`.
//!
//! The supplied authority retains exact cleared existing-state provenance,
//! preserved whole-wrapper namespace, journal, boot plan, installation, and
//! active-state-reservation evidence. This boundary revalidates that authority
//! twice, derives the sole `BootRepairRequired` successor, performs exactly one
//! conditional journal advance, and drops both the authority and old store
//! before canonical reopen. It performs no boot, database, namespace, cleanup,
//! retry, finalizer, or journal-delete effect.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{CodecError, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{
    UsrRollbackActiveReblitBootRepairRequiredAuthority, UsrRollbackActiveReblitBootRepairRequiredAuthorityError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackActiveReblitBootRepairRequiredRecord {
    CandidatePreserved,
    BootRepairRequired,
}

/// Persist the sole ActiveReblit boot-repair-required successor, then
/// independently reopen and compare the complete canonical record.
pub(in crate::client) fn persist_usr_rollback_active_reblit_boot_repair_required_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackActiveReblitBootRepairRequiredAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackActiveReblitBootRepairRequiredPersistenceError> {
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackActiveReblitBootRepairRequiredPersistenceError::Authority(
            source,
        ));
    }
    let source_record = authority.record().clone();
    let successor = match source_record.rollback_successor(None) {
        Ok(successor) if successor.phase == Phase::BootRepairRequired => successor,
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(
                UsrRollbackActiveReblitBootRepairRequiredPersistenceError::UnexpectedSuccessor {
                    phase: successor.phase,
                },
            );
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackActiveReblitBootRepairRequiredPersistenceError::RouteConstruction { source });
        }
    };

    before_usr_rollback_active_reblit_boot_repair_required_final_revalidation();
    if let Err(source) = authority.revalidate(&journal) {
        drop(authority);
        drop(journal);
        return Err(UsrRollbackActiveReblitBootRepairRequiredPersistenceError::Authority(
            source,
        ));
    }
    let installation = authority.installation().clone();
    let advance = journal.advance(&source_record, &successor);

    // Canonical reopen begins only after the source-bound authority and old
    // lock-bearing store are destroyed. Neither can authorize another route.
    drop(authority);
    drop(journal);

    let reopened =
        reopen_canonical_journal(&installation).map_err(UsrRollbackActiveReblitBootRepairRequiredReopenError::from);
    match advance {
        Ok(()) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitBootRepairRequiredPersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => {
                Err(UsrRollbackActiveReblitBootRepairRequiredPersistenceError::ReopenAfterSuccessfulAdvance { source })
            }
        },
        Err(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackActiveReblitBootRepairRequiredPersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitBootRepairRequiredRecord::CandidatePreserved,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackActiveReblitBootRepairRequiredPersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitBootRepairRequiredRecord::BootRepairRequired,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitBootRepairRequiredPersistenceError::AdvanceAndReopen {
                        advance: advance_error,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackActiveReblitBootRepairRequiredPersistenceError::AdvanceAndReopen {
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
) -> UsrRollbackActiveReblitBootRepairRequiredReopenError {
    UsrRollbackActiveReblitBootRepairRequiredReopenError::UnexpectedRecord {
        expected_candidate_preserved: Box::new(source.clone()),
        expected_boot_repair_required: Box::new(successor.clone()),
        actual: actual.map(Box::new),
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_AUTHORITY_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_active_reblit_boot_repair_required_final_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_active_reblit_boot_repair_required_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_active_reblit_boot_repair_required_final_revalidation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitBootRepairRequiredPersistenceError {
    #[error("revalidate exact ActiveReblit CandidatePreserved boot-repair-required authority")]
    Authority(#[source] UsrRollbackActiveReblitBootRepairRequiredAuthorityError),
    #[error("derive the sole legal ActiveReblit BootRepairRequired successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("ActiveReblit boot-repair-required routing selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error(
        "ActiveReblit boot-repair-required journal advance failed after reopening exact durable {durable:?} record"
    )]
    Advance {
        durable: DurableUsrRollbackActiveReblitBootRepairRequiredRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its ActiveReblit BootRepairRequired advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackActiveReblitBootRepairRequiredReopenError,
    },
    #[error(
        "ActiveReblit boot-repair-required journal advance failed ({advance}) and its canonical record could not be reconciled"
    )]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackActiveReblitBootRepairRequiredReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitBootRepairRequiredReopenError {
    #[error("revalidate retained installation around ActiveReblit boot-repair-required journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical ActiveReblit boot-repair-required journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact ActiveReblit CandidatePreserved nor BootRepairRequired record (candidate_preserved={expected_candidate_preserved:?}, boot_repair_required={expected_boot_repair_required:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_candidate_preserved: Box<TransitionRecord>,
        expected_boot_repair_required: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackActiveReblitBootRepairRequiredReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
