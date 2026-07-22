//! Atomic retirement of one exact promoted boot-publication receipt head.
//!
//! Retirement clears only the singleton head. The exact current and optional
//! predecessor bodies remain immutable and are required both before mutation
//! and on an already-retired retry.

use diesel::{
    Connection as _, SqliteConnection,
    connection::{AnsiTransactionManager, TransactionManager as _},
};
use thiserror::Error;

use super::{
    BootPublicationReceiptFingerprint, BootPublicationReceiptPair,
    BootPublicationReceiptState, BootPublicationReceiptStateError, Database,
    ReceiptReference, TransitionId, load_receipt_state, load_required_receipt,
};
use crate::db::Error as DatabaseError;

use super::super::boot_publication_receipt_head::retire_committed_row;

/// Whether this invocation cleared the exact head or proved an earlier clear.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootPublicationReceiptRetirementOutcome {
    Retired,
    AlreadyRetired,
}

/// Exact durable state admitted after an uncertain transaction report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootPublicationReceiptRetirementDurableState {
    Promoted,
    Retired,
}

impl Database {
    /// Authenticate whether one exact receipt chain is still promoted or has
    /// already had its singleton head retired. This is a read-only deferred
    /// transaction and retains no mutation authority.
    pub(crate) fn inspect_exact_boot_publication_receipt_retirement_state(
        &self,
        transition_id: &TransitionId,
        pair: &BootPublicationReceiptPair,
    ) -> Result<BootPublicationReceiptRetirementDurableState, BootPublicationReceiptRetirementError> {
        self.conn.exec(|connection| {
            connection.transaction(|connection| {
                inspect_exact_state(connection, transition_id, pair)
            })
        })
    }

    /// Retire one exact promoted receipt head without deleting immutable bodies.
    ///
    /// Exact identity is supplied as the transition ID and compact receipt
    /// pair. Canonical current and predecessor bodies are derived from durable
    /// storage. An exact already-retired retry remains read-only.
    pub(crate) fn retire_promoted_boot_publication_receipt_head(
        &self,
        transition_id: &TransitionId,
        pair: &BootPublicationReceiptPair,
    ) -> Result<BootPublicationReceiptRetirementOutcome, BootPublicationReceiptRetirementError> {
        let preflight = self.inspect_exact_boot_publication_receipt_retirement_state(
            transition_id,
            pair,
        )?;
        if preflight == BootPublicationReceiptRetirementDurableState::Retired {
            return Ok(BootPublicationReceiptRetirementOutcome::AlreadyRetired);
        }

        let mut transaction_body_succeeded = false;
        let transaction = self.conn.exclusive_tx(|connection| {
            let outcome = retire_receipt(connection, transition_id, pair)?;
            transaction_body_succeeded = true;
            Ok(outcome)
        });

        match transaction {
            Ok(outcome) => {
                if let Err(source) = after_commit_before_return() {
                    return commit_report_error(self, transition_id, pair, source);
                }
                match classify_durable_state(self, transition_id, pair) {
                    Ok(Some(BootPublicationReceiptRetirementDurableState::Retired)) => Ok(outcome),
                    Ok(Some(durable)) => {
                        Err(BootPublicationReceiptRetirementError::PostCommitDurableState { durable })
                    }
                    Ok(None) => Err(BootPublicationReceiptRetirementError::PostCommitMismatch),
                    Err(source) => {
                        Err(BootPublicationReceiptRetirementError::PostCommitState(source))
                    }
                }
            }
            Err(BootPublicationReceiptRetirementError::Database(source))
                if transaction_body_succeeded =>
            {
                commit_report_error(self, transition_id, pair, source)
            }
            Err(error) => Err(error),
        }
    }
}

fn retire_receipt(
    connection: &mut SqliteConnection,
    transition_id: &TransitionId,
    pair: &BootPublicationReceiptPair,
) -> Result<BootPublicationReceiptRetirementOutcome, BootPublicationReceiptRetirementError> {
    if inspect_exact_state(connection, transition_id, pair)?
        == BootPublicationReceiptRetirementDurableState::Retired
    {
        return Ok(BootPublicationReceiptRetirementOutcome::AlreadyRetired);
    }

    before_head_update(connection);
    let changed = retire_committed_row(connection, pair.pending)?;
    if changed != 1 {
        return Err(BootPublicationReceiptRetirementError::HeadUpdateRowMismatch {
            changed,
        });
    }
    after_head_update_before_commit(connection);

    if inspect_exact_state(connection, transition_id, pair)?
        != BootPublicationReceiptRetirementDurableState::Retired
    {
        return Err(BootPublicationReceiptRetirementError::TerminalRevalidationMismatch);
    }
    Ok(BootPublicationReceiptRetirementOutcome::Retired)
}

fn inspect_exact_state(
    connection: &mut SqliteConnection,
    transition_id: &TransitionId,
    pair: &BootPublicationReceiptPair,
) -> Result<BootPublicationReceiptRetirementDurableState, BootPublicationReceiptRetirementError> {
    let state = load_receipt_state(connection)?;
    match classify_exact_state(connection, &state, transition_id, pair)? {
        Some(durable) => Ok(durable),
        None => Err(BootPublicationReceiptRetirementError::StateMismatch {
            committed: state.head().committed(),
            pending_present: state.head().pending().is_some(),
        }),
    }
}

fn classify_exact_state(
    connection: &mut SqliteConnection,
    state: &BootPublicationReceiptState,
    transition_id: &TransitionId,
    pair: &BootPublicationReceiptPair,
) -> Result<Option<BootPublicationReceiptRetirementDurableState>, BootPublicationReceiptStateError> {
    if state.head().pending().is_some() || state.pending().is_some() {
        return Ok(None);
    }
    let durable = match state.head().committed() {
        Some(committed) if committed == pair.pending => {
            BootPublicationReceiptRetirementDurableState::Promoted
        }
        None if state.committed().is_none() => {
            BootPublicationReceiptRetirementDurableState::Retired
        }
        _ => return Ok(None),
    };

    let reference = match durable {
        BootPublicationReceiptRetirementDurableState::Promoted => ReceiptReference::Committed,
        BootPublicationReceiptRetirementDurableState::Retired => ReceiptReference::Retired,
    };
    let current = load_required_receipt(connection, reference, pair.pending)?;
    if current.body().transition_id() != transition_id
        || current.body().committed_predecessor() != pair.committed
    {
        return Ok(None);
    }
    if durable == BootPublicationReceiptRetirementDurableState::Promoted
        && state.committed() != Some(&current)
    {
        return Ok(None);
    }
    if let Some(predecessor) = pair.committed {
        load_required_receipt(
            connection,
            ReceiptReference::CommittedPredecessor,
            predecessor,
        )?;
    }
    Ok(Some(durable))
}

fn classify_durable_state(
    database: &Database,
    transition_id: &TransitionId,
    pair: &BootPublicationReceiptPair,
) -> Result<
    Option<BootPublicationReceiptRetirementDurableState>,
    BootPublicationReceiptStateError,
> {
    database.conn.exec(|connection| {
        connection.transaction(|connection| {
            let state = load_receipt_state(connection)?;
            classify_exact_state(connection, &state, transition_id, pair)
        })
    })
}

fn commit_report_error(
    database: &Database,
    transition_id: &TransitionId,
    pair: &BootPublicationReceiptPair,
    source: DatabaseError,
) -> Result<BootPublicationReceiptRetirementOutcome, BootPublicationReceiptRetirementError> {
    if let Err(cleanup) = reset_transaction_after_report(database) {
        return Err(BootPublicationReceiptRetirementError::CommitReportCleanup {
            report: source,
            cleanup,
        });
    }
    match classify_durable_state(database, transition_id, pair) {
        Ok(Some(durable)) => Err(BootPublicationReceiptRetirementError::CommitReport {
            durable,
            source,
        }),
        Ok(None) => Err(BootPublicationReceiptRetirementError::CommitReportMismatch { source }),
        Err(reconciliation) => {
            Err(BootPublicationReceiptRetirementError::CommitReportAndReconciliation {
                commit: source,
                reconciliation,
            })
        }
    }
}

fn reset_transaction_after_report(database: &Database) -> Result<(), DatabaseError> {
    database.conn.exec(|connection| {
        match AnsiTransactionManager::rollback_transaction(connection) {
            Ok(()) | Err(diesel::result::Error::NotInTransaction) => {}
            Err(source) => return Err(DatabaseError::from(source)),
        }
        connection.transaction::<(), DatabaseError, _>(|_| Ok(()))
    })
}

#[derive(Debug, Error)]
pub(crate) enum BootPublicationReceiptRetirementError {
    #[error("strictly load canonical boot-publication receipt state")]
    State(#[from] BootPublicationReceiptStateError),
    #[error("receipt state is neither exact promoted nor exact retired (committed={committed:?}, pending_present={pending_present})")]
    StateMismatch {
        committed: Option<BootPublicationReceiptFingerprint>,
        pending_present: bool,
    },
    #[error("the conditional receipt-head retirement changed {changed} rows instead of exactly one")]
    HeadUpdateRowMismatch { changed: usize },
    #[error("the in-transaction retired receipt state failed exact terminal revalidation")]
    TerminalRevalidationMismatch,
    #[error("receipt retirement committed but post-commit evidence classified {durable:?}")]
    PostCommitDurableState {
        durable: BootPublicationReceiptRetirementDurableState,
    },
    #[error("receipt retirement committed but post-commit evidence matched neither exact promoted nor retired state")]
    PostCommitMismatch,
    #[error("strictly reload receipt state after the retirement transaction committed")]
    PostCommitState(#[source] BootPublicationReceiptStateError),
    #[error("receipt-retirement transaction reported failure after its body succeeded; durable state is {durable:?}")]
    CommitReport {
        durable: BootPublicationReceiptRetirementDurableState,
        #[source]
        source: DatabaseError,
    },
    #[error("receipt-retirement transaction reported failure and durable state matched neither exact promoted nor retired state")]
    CommitReportMismatch {
        #[source]
        source: DatabaseError,
    },
    #[error("receipt-retirement transaction reported failure and a clean SQLite transaction boundary could not be restored")]
    CommitReportCleanup {
        report: DatabaseError,
        #[source]
        cleanup: DatabaseError,
    },
    #[error("receipt-retirement transaction reported failure and strict durable reconciliation also failed")]
    CommitReportAndReconciliation {
        commit: DatabaseError,
        #[source]
        reconciliation: BootPublicationReceiptStateError,
    },
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

impl From<diesel::result::Error> for BootPublicationReceiptRetirementError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database(DatabaseError::from(source))
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_HEAD_UPDATE: std::cell::RefCell<Option<Box<dyn FnOnce(&mut SqliteConnection)>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_HEAD_UPDATE_BEFORE_COMMIT: std::cell::RefCell<Option<Box<dyn FnOnce(&mut SqliteConnection)>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_COMMIT_BEFORE_RETURN: std::cell::RefCell<Option<Box<dyn FnOnce() -> Result<(), DatabaseError>>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_before_head_update(callback: impl FnOnce(&mut SqliteConnection) + 'static) {
    BEFORE_HEAD_UPDATE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(callback)).is_none());
    });
}

#[cfg(test)]
fn arm_after_head_update_before_commit(
    callback: impl FnOnce(&mut SqliteConnection) + 'static,
) {
    AFTER_HEAD_UPDATE_BEFORE_COMMIT.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(callback)).is_none());
    });
}

#[cfg(test)]
pub(crate) fn arm_boot_publication_receipt_retirement_after_commit_error(
    source: DatabaseError,
) {
    AFTER_COMMIT_BEFORE_RETURN.with(|slot| {
        assert!(
            slot
                .borrow_mut()
                .replace(Box::new(move || Err(source)))
                .is_none(),
        );
    });
}

#[cfg(test)]
fn before_head_update(connection: &mut SqliteConnection) {
    BEFORE_HEAD_UPDATE.with(|slot| {
        if let Some(callback) = slot.borrow_mut().take() {
            callback(connection);
        }
    });
}

#[cfg(not(test))]
fn before_head_update(_: &mut SqliteConnection) {}

#[cfg(test)]
fn after_head_update_before_commit(connection: &mut SqliteConnection) {
    AFTER_HEAD_UPDATE_BEFORE_COMMIT.with(|slot| {
        if let Some(callback) = slot.borrow_mut().take() {
            callback(connection);
        }
    });
}

#[cfg(not(test))]
fn after_head_update_before_commit(_: &mut SqliteConnection) {}

#[cfg(test)]
fn after_commit_before_return() -> Result<(), DatabaseError> {
    AFTER_COMMIT_BEFORE_RETURN.with(|slot| {
        slot.borrow_mut().take().map_or(Ok(()), |callback| callback())
    })
}

#[cfg(not(test))]
fn after_commit_before_return() -> Result<(), DatabaseError> {
    Ok(())
}

#[cfg(test)]
#[path = "retirement/tests.rs"]
mod tests;
