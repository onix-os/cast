//! Persist the activation `CommitDecided -> CommitCleanupComplete` advance.
//!
//! Serves `NewState` and `ActivateArchived`. Neither has a cleanup effect to
//! reconcile — neither rotates a staging wrapper — so this only proves the
//! record, advances it, and then proves the advance actually landed. The
//! crash-safety reasoning it shares with ActiveReblit lives in
//! [`super::reopened_advance`]; only the error vocabulary is local. See
//! `plans/future_impl.md` §1.1b and §1.2.

use crate::{
    Installation,
    client::startup_reconciliation::{
        ActivationCommitCleanupAuthority, ActivationCommitCleanupAuthorityError, ActivationTerminalStep,
    },
    transition_journal::{CodecError, Phase, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    canonical_journal_reopen::try_reopen_canonical_journal,
    reopened_advance::{ReopenedDurableRecord, classify_reopened_record},
};

/// Which record the journal durably holds when persistence could not complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableActivationCommitCleanupRecord {
    /// The advance never landed; the transition is still at `CommitDecided`.
    CommitDecided,
    /// The advance landed; the transition is at `CommitCleanupComplete`.
    CommitCleanupComplete,
}

/// Advance a NewState cleanup record and return the reopened journal bound to
/// its successor.
///
/// The journal is closed and reopened deliberately: a reported failure does not
/// prove the write was lost, so the reopened record is the only evidence of what
/// is durable. Every error therefore reports which record survives, so a caller
/// can never mistake a landed advance for a lost one.
// Forward scaffolding: consumed once Slice 5 wires `apply_new_state_candidate`
// live; the coordinated NewState route reaches `Complete` through here.
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) fn persist_activation_terminal_advance_retaining_binding(
    journal: TransitionJournalStore,
    authority: ActivationCommitCleanupAuthority<'_>,
    step: ActivationTerminalStep,
) -> Result<
    (TransitionJournalStore, TransitionRecord, TransitionJournalRecordBinding),
    ActivationCommitCleanupPersistenceError,
> {
    authority
        .revalidate(&journal)
        .map_err(ActivationCommitCleanupPersistenceError::Authority)?;

    let source_record = authority.record().clone();
    let successor = derive_successor(&source_record, step)?;

    let installation = authority.installation().clone();

    // The advance is the last operation permitted on this handle. Whether it
    // reports success or failure, only the reopened journal proves what is
    // durable, so every path below closes and reopens before concluding.
    let (successor_binding, post_advance_authority, same_store) =
        match authority.advance_record_binding(&journal, &successor) {
            Ok((successor_binding, post_advance_authority)) => {
                let same_store =
                    post_advance_authority.revalidate_successor_same_store(&journal, &successor_binding, &successor);
                (successor_binding, post_advance_authority, same_store)
            }
            Err(source) => {
                drop(journal);
                let durable = durable_after_reopen(&installation, &source_record, &successor);
                return Err(ActivationCommitCleanupPersistenceError::Advance { durable, source });
            }
        };

    drop(journal);
    let (reopened, actual) = try_reopen_canonical_journal(&installation).map_err(|source| {
        ActivationCommitCleanupPersistenceError::Reopen {
            source: Box::new(source),
        }
    })?;

    if let Err(source) = same_store {
        let durable = durable_from(classify_reopened_record(actual.as_ref(), &source_record, &successor));
        drop(successor_binding);
        drop(post_advance_authority);
        drop(reopened);
        return Err(ActivationCommitCleanupPersistenceError::PostAdvanceValidation { durable, source });
    }

    match classify_reopened_record(actual.as_ref(), &source_record, &successor) {
        ReopenedDurableRecord::Successor => {}
        verdict => {
            let durable = durable_from(verdict);
            drop(successor_binding);
            drop(post_advance_authority);
            drop(reopened);
            return Err(ActivationCommitCleanupPersistenceError::AdvanceNotDurable { durable });
        }
    }

    // The reopened store has a different identity, so the successor must be
    // re-proven against it and then rebound to it.
    post_advance_authority
        .revalidate_successor_reopened(&reopened, &successor_binding, &successor)
        .map_err(ActivationCommitCleanupPersistenceError::Authority)?;
    let fresh_binding = recapture_successor_binding(&installation, &reopened, &successor)?;
    post_advance_authority
        .revalidate_successor_same_store(&reopened, &fresh_binding, &successor)
        .map_err(ActivationCommitCleanupPersistenceError::Authority)?;
    drop(successor_binding);
    Ok((reopened, successor, fresh_binding))
}

/// Build the successor this step advances to.
///
/// Entering `BootSyncComplete` is the one receipt-bound edge in the chain: its
/// successor must carry the exact pair the source record already binds, so it
/// goes through the typed constructor. Every other step is a generic forward
/// advance derived from the record's own options.
fn derive_successor(
    source_record: &TransitionRecord,
    step: ActivationTerminalStep,
) -> Result<TransitionRecord, ActivationCommitCleanupPersistenceError> {
    let built = if step.is_receipt_bound() {
        let pair = match source_record.boot_publication_receipt_correlation() {
            Ok(Some(pair)) => pair,
            Ok(None) => return Err(ActivationCommitCleanupPersistenceError::MissingReceiptCorrelation),
            Err(source) => return Err(ActivationCommitCleanupPersistenceError::RouteConstruction { source }),
        };
        source_record.boot_sync_complete_successor(pair)
    } else {
        source_record.forward_successor(None)
    };
    match built {
        Ok(successor) if successor.phase == step.successor_phase() => Ok(successor),
        Ok(successor) => Err(ActivationCommitCleanupPersistenceError::UnexpectedSuccessor { phase: successor.phase }),
        Err(source) => Err(ActivationCommitCleanupPersistenceError::RouteConstruction { source }),
    }
}

/// Reopen the canonical journal purely to learn which record is durable after a
/// failure. Used on paths that have already lost their journal handle; a reopen
/// failure leaves durability genuinely unknown.
fn durable_after_reopen(
    installation: &Installation,
    source_record: &TransitionRecord,
    successor: &TransitionRecord,
) -> Option<DurableActivationCommitCleanupRecord> {
    match try_reopen_canonical_journal(installation) {
        Ok((reopened, actual)) => {
            let durable = durable_from(classify_reopened_record(actual.as_ref(), source_record, successor));
            drop(reopened);
            durable
        }
        Err(_) => None,
    }
}

fn durable_from(verdict: ReopenedDurableRecord) -> Option<DurableActivationCommitCleanupRecord> {
    match verdict {
        ReopenedDurableRecord::Source => Some(DurableActivationCommitCleanupRecord::CommitDecided),
        ReopenedDurableRecord::Successor => Some(DurableActivationCommitCleanupRecord::CommitCleanupComplete),
        ReopenedDurableRecord::Neither => None,
    }
}

fn recapture_successor_binding(
    installation: &Installation,
    reopened: &TransitionJournalStore,
    successor: &TransitionRecord,
) -> Result<TransitionJournalRecordBinding, ActivationCommitCleanupPersistenceError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(ActivationCommitCleanupPersistenceError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(ActivationCommitCleanupPersistenceError::Installation)?;
    let fresh_binding = reopened
        .record_binding(cast, successor)
        .map_err(|source| ActivationCommitCleanupPersistenceError::FreshSuccessorBinding { source })?;
    installation
        .revalidate_mutable_namespace()
        .map_err(ActivationCommitCleanupPersistenceError::Installation)?;
    Ok(fresh_binding)
}

/// Every variant that can follow the advance reports `durable`: which record the
/// journal is known to hold. `None` means the reopened journal held neither the
/// source nor the successor, so durability is genuinely unknown.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) enum ActivationCommitCleanupPersistenceError {
    #[error("exact NewState cleanup authority")]
    Authority(#[source] ActivationCommitCleanupAuthorityError),
    #[error("the cleanup successor is phase {phase:?}, not commit-cleanup-complete")]
    UnexpectedSuccessor { phase: Phase },
    #[error("the receipt-bound step requires a bound boot-publication receipt pair")]
    MissingReceiptCorrelation,
    #[error("construct the NewState cleanup successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("advance the NewState cleanup record (durable: {durable:?})")]
    Advance {
        durable: Option<DurableActivationCommitCleanupRecord>,
        #[source]
        source: ActivationCommitCleanupAuthorityError,
    },
    #[error("the NewState cleanup advance is not durable (durable: {durable:?})")]
    AdvanceNotDurable {
        durable: Option<DurableActivationCommitCleanupRecord>,
    },
    #[error("validate the advanced NewState cleanup record (durable: {durable:?})")]
    PostAdvanceValidation {
        durable: Option<DurableActivationCommitCleanupRecord>,
        #[source]
        source: ActivationCommitCleanupAuthorityError,
    },
    #[error("reopen the canonical journal after the NewState cleanup advance")]
    Reopen {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("rebind the NewState cleanup successor in the reopened journal")]
    FreshSuccessorBinding {
        #[source]
        source: crate::transition_journal::StorageError,
    },
    #[error("delete the terminal NewState record (surrounding evidence verified: {verified})")]
    TerminalDelete {
        #[source]
        source: Box<crate::transition_journal::TransitionJournalRecordDeleteError>,
        verified: bool,
    },
    #[error("audit the in-flight transition")]
    InFlight(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("clear the candidate row's in-flight transition marker")]
    ClearInFlight(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("the {step} terminal step was deferred: {reason}")]
    AdmissionDeferred { step: String, reason: String },
    #[error("the {step} terminal step was not applicable")]
    AdmissionNotApplicable { step: String },
    #[error("installation")]
    Installation(#[source] crate::installation::Error),
}

/// End a NewState transition by deleting its terminal `Complete` record.
///
/// Deletion is the last durable act of the transition. A delete that reports
/// `Absent` is still a failure to report, because it means something else
/// removed the record — but the surrounding evidence is verified either way, so
/// the caller can tell "finished" from "someone else interfered".
// Forward scaffolding: consumed once Slice 5 wires `apply_new_state_candidate` live.
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) fn finalize_activation_complete(
    journal: TransitionJournalStore,
    authority: ActivationCommitCleanupAuthority<'_>,
) -> Result<TransitionJournalStore, ActivationCommitCleanupPersistenceError> {
    let source = authority.record();
    if source.phase != Phase::Complete {
        return Err(ActivationCommitCleanupPersistenceError::UnexpectedSuccessor { phase: source.phase });
    }

    let (delete, after_delete) = authority
        .attempt_record_bound_delete(&journal)
        .map_err(ActivationCommitCleanupPersistenceError::Authority)?;

    match delete {
        Ok(()) => {
            after_delete
                .revalidate_after_journal_delete(&journal)
                .map_err(ActivationCommitCleanupPersistenceError::Authority)?;
            Ok(journal)
        }
        Err(source) => {
            // Verify the surrounding evidence regardless, so a failed delete is
            // never confused with a corrupted transition.
            let verification = after_delete.revalidate_after_journal_delete(&journal);
            Err(ActivationCommitCleanupPersistenceError::TerminalDelete {
                source: Box::new(source),
                verified: verification.is_ok(),
            })
        }
    }
}

/// Drive a committed NewState transition from `CommitDecided` to a deleted
/// terminal record.
///
/// A transition that stops at `CommitDecided` is not finished: the next startup
/// finds a live journal record and reports `RecoveryPending`. This walks the
/// remaining chain — cleanup, cleanup-complete, then terminal deletion — so the
/// transition actually ends (`plans/future_impl.md` §1.1b/§1.1c).
#[allow(dead_code)] // consumed by the coordinated NewState route
pub(in crate::client) fn finish_activation_after_commit(
    mut journal: TransitionJournalStore,
    state_db: &crate::db::state::Database,
    installation: &Installation,
    mut record: TransitionRecord,
    reservation: &crate::client::active_state_snapshot::ActiveStateReservation,
) -> Result<TransitionJournalStore, ActivationCommitCleanupPersistenceError> {
    for step in [
        ActivationTerminalStep::CommitCleanup,
        ActivationTerminalStep::CleanupComplete,
    ] {
        // Resumption: a record recovered mid-tail has already crossed the
        // earlier steps, so skip the ones it is past rather than refusing it.
        if record.phase != step.source_phase() {
            continue;
        }
        let in_flight = state_db
            .audit_in_flight_transition()
            .map_err(|source| ActivationCommitCleanupPersistenceError::InFlight(Box::new(source)))?;
        let authority = match ActivationCommitCleanupAuthority::capture(
            installation,
            &journal,
            state_db,
            reservation,
            &record,
            in_flight,
            step,
        )
        .map_err(ActivationCommitCleanupPersistenceError::Authority)?
        {
            crate::client::startup_reconciliation::ActivationCommitCleanupAdmission::Ready(authority) => authority,
            crate::client::startup_reconciliation::ActivationCommitCleanupAdmission::Deferred(reason) => {
                return Err(ActivationCommitCleanupPersistenceError::AdmissionDeferred {
                    step: format!("{step:?}"),
                    reason: format!("{reason:?}"),
                });
            }
            crate::client::startup_reconciliation::ActivationCommitCleanupAdmission::NotApplicable => {
                return Err(ActivationCommitCleanupPersistenceError::AdmissionNotApplicable {
                    step: format!("{step:?}"),
                });
            }
        };
        let (next_journal, next_record, binding) =
            persist_activation_terminal_advance_retaining_binding(journal, authority, step)?;
        drop(binding);
        journal = next_journal;
        record = next_record;
    }

    // Clear the candidate row's in-flight marker before the terminal delete.
    //
    // The coordinator allocates the row with `add_with_transition`, which stamps
    // `state.transition_id`. Archiving the predecessor is what normally clears
    // it (`db/state/exact_archived_removal.rs`), so a transition with no
    // predecessor to archive never reaches that path — the marker would outlive
    // the transition and the next startup's `audit_in_flight_transition` would
    // report `OrphanTransitionRow`, making a successful install look like an
    // interrupted one.
    //
    // The ordering is the crash-safety property. Clearing here happens while the
    // record is still live at `Complete`, so a crash between the clear and the
    // delete leaves a record whose ownership reads `Cleared` with no in-flight
    // row — a combination `inspect_database` already admits — and the next
    // startup finishes the delete. Deleting first would invert that: a marked
    // row with no record left to recover it from.
    //
    // Guarded because `clear_transition_if_matches` requires exactly one row to
    // change. When a predecessor was archived the marker is already gone, and an
    // unguarded clear would fail a transition that is in fact correct.
    let pending = state_db
        .audit_in_flight_transition()
        .map_err(|source| ActivationCommitCleanupPersistenceError::InFlight(Box::new(source)))?;
    if let Some(row) = pending.as_ref()
        && row.transition_id == record.transition_id
    {
        state_db
            .clear_transition_if_matches(row.state_id, &record.transition_id)
            .map_err(|source| ActivationCommitCleanupPersistenceError::ClearInFlight(Box::new(source)))?;
    }

    let in_flight = state_db
        .audit_in_flight_transition()
        .map_err(|source| ActivationCommitCleanupPersistenceError::InFlight(Box::new(source)))?;
    let authority = match ActivationCommitCleanupAuthority::capture(
        installation,
        &journal,
        state_db,
        reservation,
        &record,
        in_flight,
        ActivationTerminalStep::Finalize,
    )
    .map_err(ActivationCommitCleanupPersistenceError::Authority)?
    {
        crate::client::startup_reconciliation::ActivationCommitCleanupAdmission::Ready(authority) => authority,
        crate::client::startup_reconciliation::ActivationCommitCleanupAdmission::Deferred(reason) => {
            return Err(ActivationCommitCleanupPersistenceError::AdmissionDeferred {
                step: "Finalize".to_owned(),
                reason: format!("{reason:?}"),
            });
        }
        crate::client::startup_reconciliation::ActivationCommitCleanupAdmission::NotApplicable => {
            return Err(ActivationCommitCleanupPersistenceError::AdmissionNotApplicable {
                step: "Finalize".to_owned(),
            });
        }
    };
    finalize_activation_complete(journal, authority)
}
