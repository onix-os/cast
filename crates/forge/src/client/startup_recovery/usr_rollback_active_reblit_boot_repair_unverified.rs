//! Persist the conservative ActiveReblit `BootRepairStarted ->
//! BootRepairUnverified` boundary.
//!
//! This executor is entered only from a Started record observed by a fresh
//! startup entry.  It performs one journal mutation, invokes boot zero times,
//! and returns immediately.  The successor is deliberately terminal and is
//! retained for structured manual recovery rather than finalized or deleted.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{BootRollback, CodecError, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{
    UsrRollbackActiveReblitBootRepairUnverifiedAuthority, UsrRollbackActiveReblitBootRepairUnverifiedAuthorityError,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord {
    BootRepairStarted,
    BootRepairUnverified,
}

pub(in crate::client) fn persist_usr_rollback_active_reblit_boot_repair_unverified_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackActiveReblitBootRepairUnverifiedAuthority<'_, '_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackActiveReblitBootRepairUnverifiedPersistenceError> {
    authority.revalidate(&journal)?;
    let source_record = authority.record().clone();
    let successor = authority.boot_repair_unverified_successor()?;
    if successor.phase != Phase::BootRepairUnverified
        || successor.rollback.as_ref().map(|rollback| rollback.boot) != Some(BootRollback::Unverified)
    {
        drop(authority);
        drop(journal);
        return Err(
            UsrRollbackActiveReblitBootRepairUnverifiedPersistenceError::UnexpectedSuccessor {
                phase: successor.phase,
                boot: successor.rollback.as_ref().map(|rollback| rollback.boot),
            },
        );
    }

    authority.revalidate(&journal)?;
    let installation = authority.installation().clone();
    let advance = journal.advance(&source_record, &successor);
    drop(authority);
    drop(journal);

    let reopened =
        reopen_canonical_journal(&installation).map_err(UsrRollbackActiveReblitBootRepairUnverifiedReopenError::from);
    match advance {
        Ok(()) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitBootRepairUnverifiedPersistenceError::ReopenAfterSuccessfulAdvance {
                        source: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(source) => Err(
                UsrRollbackActiveReblitBootRepairUnverifiedPersistenceError::ReopenAfterSuccessfulAdvance { source },
            ),
        },
        Err(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackActiveReblitBootRepairUnverifiedPersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord::BootRepairStarted,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackActiveReblitBootRepairUnverifiedPersistenceError::Advance {
                    durable: DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord::BootRepairUnverified,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(
                    UsrRollbackActiveReblitBootRepairUnverifiedPersistenceError::AdvanceAndReopen {
                        advance: advance_error,
                        reopen: unexpected_record(&source_record, &successor, actual),
                    },
                )
            }
            Err(reopen) => Err(
                UsrRollbackActiveReblitBootRepairUnverifiedPersistenceError::AdvanceAndReopen {
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
) -> UsrRollbackActiveReblitBootRepairUnverifiedReopenError {
    UsrRollbackActiveReblitBootRepairUnverifiedReopenError::UnexpectedRecord {
        expected_started: Box::new(source.clone()),
        expected_unverified: Box::new(successor.clone()),
        actual: actual.map(Box::new),
    }
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitBootRepairUnverifiedPersistenceError {
    #[error("revalidate exact ActiveReblit BootRepairStarted authority")]
    Authority(#[from] UsrRollbackActiveReblitBootRepairUnverifiedAuthorityError),
    #[error("derive the sole legal ActiveReblit BootRepairUnverified successor")]
    RouteConstruction(#[from] CodecError),
    #[error("ActiveReblit Started route selected unexpected successor phase {phase:?} and boot state {boot:?}")]
    UnexpectedSuccessor { phase: Phase, boot: Option<BootRollback> },
    #[error("ActiveReblit Started -> Unverified advance failed after reopening exact durable {durable:?}")]
    Advance {
        durable: DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen exact canonical BootRepairUnverified after a successful ActiveReblit advance")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackActiveReblitBootRepairUnverifiedReopenError,
    },
    #[error("ActiveReblit Started -> Unverified advance failed ({advance}) and canonical reopen was inconclusive")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackActiveReblitBootRepairUnverifiedReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackActiveReblitBootRepairUnverifiedReopenError {
    #[error("revalidate retained installation around ActiveReblit BootRepairUnverified reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load descriptor-rooted canonical ActiveReblit BootRepairUnverified journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither exact BootRepairStarted nor BootRepairUnverified (started={expected_started:?}, unverified={expected_unverified:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_started: Box<TransitionRecord>,
        expected_unverified: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackActiveReblitBootRepairUnverifiedReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
