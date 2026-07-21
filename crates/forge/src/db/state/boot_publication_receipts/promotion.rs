//! Atomic promotion of one exact pending boot-publication receipt.
//!
//! Promotion changes only the receipt-head singleton. The immutable receipt
//! bodies remain untouched so the predecessor chain survives every successful
//! update and retry. This module grants no journal, filesystem, publication,
//! replacement, deletion, cleanup, or garbage-collection authority.

use diesel::{
    Connection as _, SqliteConnection,
    connection::{AnsiTransactionManager, TransactionManager as _},
};
use thiserror::Error;

use super::{
    BootPublicationReceiptFingerprint, BootPublicationReceiptPair,
    BootPublicationReceiptState, BootPublicationReceiptStateError,
    CanonicalBootPublicationReceipt, Database, ReceiptReference,
    TransitionId, load_receipt_state, load_required_receipt,
};
use crate::db::Error as DatabaseError;

use super::super::boot_publication_receipt_head::promote_pending_row;

/// Whether this invocation changed the exact head or proved an earlier change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootPublicationReceiptPromotionOutcome {
    Promoted,
    AlreadyPromoted,
}

/// Exact durable state admitted after an uncertain transaction report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootPublicationReceiptPromotionDurableState {
    Pending,
    Promoted,
}

impl Database {
    /// Atomically make one exact pending canonical receipt the committed head.
    ///
    /// Every identity is derived from `receipt`. Exact retry after a prior
    /// successful commit is read-only. Any other head/body state fails closed.
    pub(crate) fn promote_boot_publication_receipt(
        &self,
        receipt: &CanonicalBootPublicationReceipt,
    ) -> Result<BootPublicationReceiptPromotionOutcome, BootPublicationReceiptPromotionError> {
        let preflight = self.conn.exec(|connection| {
            connection.transaction(|connection| inspect_exact_state(connection, receipt))
        })?;
        if preflight == BootPublicationReceiptPromotionDurableState::Promoted {
            return Ok(BootPublicationReceiptPromotionOutcome::AlreadyPromoted);
        }

        let mut transaction_body_succeeded = false;
        let transaction = self.conn.exclusive_tx(|connection| {
            let outcome = promote_receipt(connection, receipt)?;
            transaction_body_succeeded = true;
            Ok(outcome)
        });

        match transaction {
            Ok(outcome) => {
                if let Err(source) = after_commit_before_return() {
                    return commit_report_error(self, receipt, source);
                }
                match classify_durable_state(self, receipt) {
                    Ok(Some(BootPublicationReceiptPromotionDurableState::Promoted)) => Ok(outcome),
                    Ok(Some(durable)) => Err(
                        BootPublicationReceiptPromotionError::PostCommitDurableState { durable },
                    ),
                    Ok(None) => Err(BootPublicationReceiptPromotionError::PostCommitMismatch),
                    Err(source) => Err(BootPublicationReceiptPromotionError::PostCommitState(source)),
                }
            }
            Err(BootPublicationReceiptPromotionError::Database(source))
                if transaction_body_succeeded =>
            {
                commit_report_error(self, receipt, source)
            }
            Err(error) => Err(error),
        }
    }
}

fn promote_receipt(
    connection: &mut SqliteConnection,
    receipt: &CanonicalBootPublicationReceipt,
) -> Result<BootPublicationReceiptPromotionOutcome, BootPublicationReceiptPromotionError> {
    if inspect_exact_state(connection, receipt)?
        == BootPublicationReceiptPromotionDurableState::Promoted
    {
        return Ok(BootPublicationReceiptPromotionOutcome::AlreadyPromoted);
    }

    let pair = receipt_pair(receipt);
    before_head_update(connection);
    let changed = promote_pending_row(connection, receipt.body().transition_id(), &pair)?;
    if changed != 1 {
        return Err(BootPublicationReceiptPromotionError::HeadUpdateRowMismatch {
            changed,
        });
    }
    after_head_update_before_commit(connection);

    let after = load_receipt_state(connection)?;
    if !is_exact_promoted(connection, &after, receipt)? {
        return Err(BootPublicationReceiptPromotionError::TerminalRevalidationMismatch);
    }
    Ok(BootPublicationReceiptPromotionOutcome::Promoted)
}

fn inspect_exact_state(
    connection: &mut SqliteConnection,
    receipt: &CanonicalBootPublicationReceipt,
) -> Result<BootPublicationReceiptPromotionDurableState, BootPublicationReceiptPromotionError> {
    let state = load_receipt_state(connection)?;
    if is_exact_promoted(connection, &state, receipt)? {
        Ok(BootPublicationReceiptPromotionDurableState::Promoted)
    } else {
        require_exact_pending(&state, receipt, receipt_pair(receipt))?;
        Ok(BootPublicationReceiptPromotionDurableState::Pending)
    }
}

fn receipt_pair(receipt: &CanonicalBootPublicationReceipt) -> BootPublicationReceiptPair {
    BootPublicationReceiptPair {
        committed: receipt.body().committed_predecessor(),
        pending: receipt.fingerprint(),
    }
}

fn require_exact_pending(
    state: &BootPublicationReceiptState,
    receipt: &CanonicalBootPublicationReceipt,
    pair: BootPublicationReceiptPair,
) -> Result<(), BootPublicationReceiptPromotionError> {
    if state.head().committed() != pair.committed {
        return Err(BootPublicationReceiptPromotionError::CommittedPredecessorMismatch {
            expected: pair.committed,
            actual: state.head().committed(),
        });
    }
    let pending = state
        .head()
        .pending()
        .ok_or(BootPublicationReceiptPromotionError::MissingPending)?;
    if pending.transition_id() != receipt.body().transition_id() {
        return Err(BootPublicationReceiptPromotionError::PendingTransitionMismatch {
            expected: receipt.body().transition_id().clone(),
            actual: pending.transition_id().clone(),
        });
    }
    if pending.fingerprint() != receipt.fingerprint() {
        return Err(BootPublicationReceiptPromotionError::PendingFingerprintMismatch {
            expected: receipt.fingerprint(),
            actual: pending.fingerprint(),
        });
    }
    if state.pending() != Some(receipt) {
        return Err(BootPublicationReceiptPromotionError::PendingBodyMismatch);
    }
    Ok(())
}

fn is_exact_pending(
    state: &BootPublicationReceiptState,
    receipt: &CanonicalBootPublicationReceipt,
) -> bool {
    let pair = receipt_pair(receipt);
    state.head().committed() == pair.committed
        && state
            .head()
            .pending()
            .is_some_and(|pending| {
                pending.transition_id() == receipt.body().transition_id()
                    && pending.fingerprint() == receipt.fingerprint()
            })
        && state.pending() == Some(receipt)
}

fn is_exact_promoted(
    connection: &mut SqliteConnection,
    state: &BootPublicationReceiptState,
    receipt: &CanonicalBootPublicationReceipt,
) -> Result<bool, BootPublicationReceiptStateError> {
    let exact_head = state.head().committed() == Some(receipt.fingerprint())
        && state.head().pending().is_none()
        && state.pending().is_none()
        && state.committed() == Some(receipt);
    if !exact_head {
        return Ok(false);
    }
    if let Some(predecessor) = receipt.body().committed_predecessor() {
        load_required_receipt(
            connection,
            ReceiptReference::CommittedPredecessor,
            predecessor,
        )?;
    }
    Ok(true)
}

fn classify_durable_state(
    database: &Database,
    receipt: &CanonicalBootPublicationReceipt,
) -> Result<Option<BootPublicationReceiptPromotionDurableState>, BootPublicationReceiptStateError> {
    database.conn.exec(|connection| {
        connection.transaction(|connection| {
            let state = load_receipt_state(connection)?;
            if is_exact_promoted(connection, &state, receipt)? {
                Ok(Some(BootPublicationReceiptPromotionDurableState::Promoted))
            } else if is_exact_pending(&state, receipt) {
                Ok(Some(BootPublicationReceiptPromotionDurableState::Pending))
            } else {
                Ok(None)
            }
        })
    })
}

fn commit_report_error(
    database: &Database,
    receipt: &CanonicalBootPublicationReceipt,
    source: DatabaseError,
) -> Result<BootPublicationReceiptPromotionOutcome, BootPublicationReceiptPromotionError> {
    if let Err(cleanup) = reset_transaction_after_report(database) {
        return Err(BootPublicationReceiptPromotionError::CommitReportCleanup {
            report: source,
            cleanup,
        });
    }
    match classify_durable_state(database, receipt) {
        Ok(Some(durable)) => Err(BootPublicationReceiptPromotionError::CommitReport {
            durable,
            source,
        }),
        Ok(None) => Err(BootPublicationReceiptPromotionError::CommitReportMismatch { source }),
        Err(reconciliation) => Err(
            BootPublicationReceiptPromotionError::CommitReportAndReconciliation {
                commit: source,
                reconciliation,
            },
        ),
    }
}

/// Restore Diesel and SQLite to the same clean transaction boundary before
/// reading durable state. SQLite may retain the write transaction when COMMIT
/// fails, and Diesel retains its corresponding transaction depth. Cleanup must
/// therefore use the transaction manager rather than raw transaction SQL.
fn reset_transaction_after_report(database: &Database) -> Result<(), DatabaseError> {
    database.conn.exec(|connection| {
        match AnsiTransactionManager::rollback_transaction(connection) {
            Ok(()) | Err(diesel::result::Error::NotInTransaction) => {}
            Err(source) => return Err(DatabaseError::from(source)),
        }
        connection.transaction::<(), DatabaseError, _>(|_| Ok(()))
    })
}

/// Fail-closed error from exact receipt promotion.
#[derive(Debug, Error)]
pub(crate) enum BootPublicationReceiptPromotionError {
    #[error("strictly load canonical boot-publication receipt state")]
    State(#[from] BootPublicationReceiptStateError),
    #[error("the receipt head has no pending publication to promote")]
    MissingPending,
    #[error("the pending receipt committed predecessor differs from the exact head")]
    CommittedPredecessorMismatch {
        expected: Option<BootPublicationReceiptFingerprint>,
        actual: Option<BootPublicationReceiptFingerprint>,
    },
    #[error("the pending receipt belongs to transition {actual}, expected {expected}")]
    PendingTransitionMismatch {
        expected: TransitionId,
        actual: TransitionId,
    },
    #[error("the pending receipt fingerprint differs from the exact canonical receipt")]
    PendingFingerprintMismatch {
        expected: BootPublicationReceiptFingerprint,
        actual: BootPublicationReceiptFingerprint,
    },
    #[error("the pending canonical receipt body differs from the supplied exact body")]
    PendingBodyMismatch,
    #[error("the conditional receipt-head promotion changed {changed} rows instead of exactly one")]
    HeadUpdateRowMismatch { changed: usize },
    #[error("the in-transaction promoted receipt state failed exact terminal revalidation")]
    TerminalRevalidationMismatch,
    #[error("receipt promotion committed but post-commit evidence classified {durable:?}")]
    PostCommitDurableState {
        durable: BootPublicationReceiptPromotionDurableState,
    },
    #[error("receipt promotion committed but post-commit evidence matched neither exact pending nor promoted state")]
    PostCommitMismatch,
    #[error("strictly reload receipt state after the promotion transaction committed")]
    PostCommitState(#[source] BootPublicationReceiptStateError),
    #[error("receipt-promotion transaction reported failure after its body succeeded; durable state is {durable:?}")]
    CommitReport {
        durable: BootPublicationReceiptPromotionDurableState,
        #[source]
        source: DatabaseError,
    },
    #[error("receipt-promotion transaction reported failure and durable state matched neither exact pending nor promoted state")]
    CommitReportMismatch {
        #[source]
        source: DatabaseError,
    },
    #[error("receipt-promotion transaction reported failure and a clean SQLite transaction boundary could not be restored")]
    CommitReportCleanup {
        report: DatabaseError,
        #[source]
        cleanup: DatabaseError,
    },
    #[error("receipt-promotion transaction reported failure and strict durable reconciliation also failed")]
    CommitReportAndReconciliation {
        commit: DatabaseError,
        #[source]
        reconciliation: BootPublicationReceiptStateError,
    },
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

impl From<diesel::result::Error> for BootPublicationReceiptPromotionError {
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
fn arm_after_commit_before_return(
    callback: impl FnOnce() -> Result<(), DatabaseError> + 'static,
) {
    AFTER_COMMIT_BEFORE_RETURN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(callback)).is_none());
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
#[path = "promotion/tests.rs"]
mod tests;
