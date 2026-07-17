//! Persist the exact next journal-only rollback intent.
//!
//! This executor is deliberately routing-only. It performs one conditional
//! journal advance and no reverse exchange, candidate movement, database
//! mutation, trigger, cleanup, or root-link effect. The supplied authority and
//! old lock-bearing store are dropped before descriptor-rooted reopen.

use thiserror::Error;

use crate::{
    installation,
    transition_journal::{CodecError, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::super::startup_reconciliation::{UsrRollbackResumeRouteAuthority, UsrRollbackResumeRouteAuthorityError};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, reopen_canonical_journal};

#[cfg(test)]
mod tests;

/// Which exact canonical record survived a failed conditional advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableUsrRollbackResumeRouteRecord {
    Source,
    Successor,
}

/// Persist exactly one authenticated first rollback intent, then independently
/// reopen and compare the complete canonical record.
pub(in crate::client) fn persist_usr_rollback_resume_route_and_reopen(
    journal: TransitionJournalStore,
    authority: UsrRollbackResumeRouteAuthority<'_>,
) -> Result<(TransitionJournalStore, TransitionRecord), UsrRollbackResumeRoutePersistenceError> {
    authority.revalidate(&journal)?;
    let source_record = authority.record().clone();

    // This is the sole production routing decision. No outcome is supplied:
    // RollbackDecided selects its first unresolved intent, while UsrRestored
    // selects candidate preservation. Neither source authorizes an effect.
    let successor = match source_record.rollback_successor(None) {
        Ok(successor)
            if matches!(
                successor.phase,
                Phase::ReverseExchangeIntent | Phase::CandidatePreserveIntent
            ) =>
        {
            successor
        }
        Ok(successor) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackResumeRoutePersistenceError::UnexpectedSuccessor { phase: successor.phase });
        }
        Err(source) => {
            drop(authority);
            drop(journal);
            return Err(UsrRollbackResumeRoutePersistenceError::RouteConstruction { source });
        }
    };

    before_usr_rollback_resume_route_final_revalidation();
    authority.revalidate(&journal)?;
    let installation = authority.installation().clone();
    let advance = journal.advance(&source_record, &successor);

    // Never reopen while the old store or the authority retaining its binding
    // remains alive, and never reuse either after an uncertain write result.
    drop(authority);
    drop(journal);

    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackResumeRouteReopenError::from);
    match advance {
        Ok(()) => match reopened {
            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackResumeRoutePersistenceError::ReopenAfterSuccessfulAdvance {
                    source: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(source) => Err(UsrRollbackResumeRoutePersistenceError::ReopenAfterSuccessfulAdvance { source }),
        },
        Err(advance_error) => match reopened {
            Ok((reopened, Some(actual))) if actual == source_record => {
                drop(reopened);
                Err(UsrRollbackResumeRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackResumeRouteRecord::Source,
                    source: advance_error,
                })
            }
            Ok((reopened, Some(actual))) if actual == successor => {
                drop(reopened);
                Err(UsrRollbackResumeRoutePersistenceError::Advance {
                    durable: DurableUsrRollbackResumeRouteRecord::Successor,
                    source: advance_error,
                })
            }
            Ok((reopened, actual)) => {
                drop(reopened);
                Err(UsrRollbackResumeRoutePersistenceError::AdvanceAndReopen {
                    advance: advance_error,
                    reopen: unexpected_record(&source_record, &successor, actual),
                })
            }
            Err(reopen) => Err(UsrRollbackResumeRoutePersistenceError::AdvanceAndReopen {
                advance: advance_error,
                reopen,
            }),
        },
    }
}

fn unexpected_record(
    source: &TransitionRecord,
    successor: &TransitionRecord,
    actual: Option<TransitionRecord>,
) -> UsrRollbackResumeRouteReopenError {
    UsrRollbackResumeRouteReopenError::UnexpectedRecord {
        expected_source: Box::new(source.clone()),
        expected_successor: Box::new(successor.clone()),
        actual: actual.map(Box::new),
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_AUTHORITY_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_before_usr_rollback_resume_route_final_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_usr_rollback_resume_route_final_revalidation() {
    BEFORE_FINAL_AUTHORITY_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_usr_rollback_resume_route_final_revalidation() {}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackResumeRoutePersistenceError {
    #[error("revalidate exact startup /usr rollback-resume routing authority")]
    Authority(#[from] UsrRollbackResumeRouteAuthorityError),
    #[error("derive the sole legal first startup /usr rollback intent")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("rollback-resume routing selected unexpected successor phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("journal advance failed after reopening exact durable {durable:?} record")]
    Advance {
        durable: DurableUsrRollbackResumeRouteRecord,
        #[source]
        source: StorageError,
    },
    #[error("reopen the canonical journal after its rollback-resume routing advance succeeded")]
    ReopenAfterSuccessfulAdvance {
        #[source]
        source: UsrRollbackResumeRouteReopenError,
    },
    #[error("journal advance failed ({advance}) and its canonical record could not be reconciled")]
    AdvanceAndReopen {
        advance: StorageError,
        #[source]
        reopen: UsrRollbackResumeRouteReopenError,
    },
}

#[derive(Debug, Error)]
pub(in crate::client) enum UsrRollbackResumeRouteReopenError {
    #[error("revalidate retained installation around journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical journal")]
    Journal(#[from] StorageError),
    #[error(
        "reopened canonical journal is neither the exact source nor successor record (source={expected_source:?}, successor={expected_successor:?}, actual={actual:?})"
    )]
    UnexpectedRecord {
        expected_source: Box<TransitionRecord>,
        expected_successor: Box<TransitionRecord>,
        actual: Option<Box<TransitionRecord>>,
    },
}

impl From<CanonicalJournalReopenError> for UsrRollbackResumeRouteReopenError {
    fn from(source: CanonicalJournalReopenError) -> Self {
        match source {
            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),
            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),
        }
    }
}
