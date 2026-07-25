//! Persist the NewState `CommitDecided -> CommitCleanupComplete` advance.
//!
//! NewState has no cleanup effect to reconcile — it never rotates a staging
//! wrapper — so this only proves the record, advances it, and then proves the
//! advance actually landed. The crash-safety reasoning it shares with
//! ActiveReblit lives in [`super::reopened_advance`]; only the error vocabulary
//! is local. See `plans/future_impl.md` §1.1b.

use crate::{
    Installation,
    client::startup_reconciliation::{
        NewStateCommitCleanupAuthority, NewStateCommitCleanupAuthorityError, NewStateTerminalStep,
    },
    transition_journal::{CodecError, Phase, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    canonical_journal_reopen::try_reopen_canonical_journal,
    reopened_advance::{ReopenedDurableRecord, classify_reopened_record},
};

/// Which record the journal durably holds when persistence could not complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum DurableNewStateCommitCleanupRecord {
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
pub(in crate::client) fn persist_new_state_terminal_advance_retaining_binding(
    journal: TransitionJournalStore,
    authority: NewStateCommitCleanupAuthority<'_>,
    step: NewStateTerminalStep,
) -> Result<
    (TransitionJournalStore, TransitionRecord, TransitionJournalRecordBinding),
    NewStateCommitCleanupPersistenceError,
> {
    authority
        .revalidate(&journal)
        .map_err(NewStateCommitCleanupPersistenceError::Authority)?;

    let source_record = authority.record().clone();
    let successor = match source_record.forward_successor(None) {
        Ok(successor) if successor.phase == step.successor_phase() => successor,
        Ok(successor) => {
            return Err(NewStateCommitCleanupPersistenceError::UnexpectedSuccessor { phase: successor.phase });
        }
        Err(source) => return Err(NewStateCommitCleanupPersistenceError::RouteConstruction { source }),
    };

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
                return Err(NewStateCommitCleanupPersistenceError::Advance { durable, source });
            }
        };

    drop(journal);
    let (reopened, actual) = try_reopen_canonical_journal(&installation)
        .map_err(|source| NewStateCommitCleanupPersistenceError::Reopen { source })?;

    if let Err(source) = same_store {
        let durable = durable_from(classify_reopened_record(actual.as_ref(), &source_record, &successor));
        drop(successor_binding);
        drop(post_advance_authority);
        drop(reopened);
        return Err(NewStateCommitCleanupPersistenceError::PostAdvanceValidation { durable, source });
    }

    match classify_reopened_record(actual.as_ref(), &source_record, &successor) {
        ReopenedDurableRecord::Successor => {}
        verdict => {
            let durable = durable_from(verdict);
            drop(successor_binding);
            drop(post_advance_authority);
            drop(reopened);
            return Err(NewStateCommitCleanupPersistenceError::AdvanceNotDurable { durable });
        }
    }

    // The reopened store has a different identity, so the successor must be
    // re-proven against it and then rebound to it.
    post_advance_authority
        .revalidate_successor_reopened(&reopened, &successor_binding, &successor)
        .map_err(NewStateCommitCleanupPersistenceError::Authority)?;
    let fresh_binding = recapture_successor_binding(&installation, &reopened, &successor)?;
    post_advance_authority
        .revalidate_successor_same_store(&reopened, &fresh_binding, &successor)
        .map_err(NewStateCommitCleanupPersistenceError::Authority)?;
    drop(successor_binding);
    Ok((reopened, successor, fresh_binding))
}

/// Reopen the canonical journal purely to learn which record is durable after a
/// failure. Used on paths that have already lost their journal handle; a reopen
/// failure leaves durability genuinely unknown.
fn durable_after_reopen(
    installation: &Installation,
    source_record: &TransitionRecord,
    successor: &TransitionRecord,
) -> Option<DurableNewStateCommitCleanupRecord> {
    match try_reopen_canonical_journal(installation) {
        Ok((reopened, actual)) => {
            let durable = durable_from(classify_reopened_record(actual.as_ref(), source_record, successor));
            drop(reopened);
            durable
        }
        Err(_) => None,
    }
}

fn durable_from(verdict: ReopenedDurableRecord) -> Option<DurableNewStateCommitCleanupRecord> {
    match verdict {
        ReopenedDurableRecord::Source => Some(DurableNewStateCommitCleanupRecord::CommitDecided),
        ReopenedDurableRecord::Successor => Some(DurableNewStateCommitCleanupRecord::CommitCleanupComplete),
        ReopenedDurableRecord::Neither => None,
    }
}

fn recapture_successor_binding(
    installation: &Installation,
    reopened: &TransitionJournalStore,
    successor: &TransitionRecord,
) -> Result<TransitionJournalRecordBinding, NewStateCommitCleanupPersistenceError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(NewStateCommitCleanupPersistenceError::Installation)?;
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(NewStateCommitCleanupPersistenceError::Installation)?;
    let fresh_binding = reopened
        .record_binding(cast, successor)
        .map_err(|source| NewStateCommitCleanupPersistenceError::FreshSuccessorBinding { source })?;
    installation
        .revalidate_mutable_namespace()
        .map_err(NewStateCommitCleanupPersistenceError::Installation)?;
    Ok(fresh_binding)
}

/// Every variant that can follow the advance reports `durable`: which record the
/// journal is known to hold. `None` means the reopened journal held neither the
/// source nor the successor, so durability is genuinely unknown.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) enum NewStateCommitCleanupPersistenceError {
    #[error("exact NewState cleanup authority")]
    Authority(#[source] NewStateCommitCleanupAuthorityError),
    #[error("the cleanup successor is phase {phase:?}, not commit-cleanup-complete")]
    UnexpectedSuccessor { phase: Phase },
    #[error("construct the NewState cleanup successor")]
    RouteConstruction {
        #[source]
        source: CodecError,
    },
    #[error("advance the NewState cleanup record (durable: {durable:?})")]
    Advance {
        durable: Option<DurableNewStateCommitCleanupRecord>,
        #[source]
        source: NewStateCommitCleanupAuthorityError,
    },
    #[error("the NewState cleanup advance is not durable (durable: {durable:?})")]
    AdvanceNotDurable {
        durable: Option<DurableNewStateCommitCleanupRecord>,
    },
    #[error("validate the advanced NewState cleanup record (durable: {durable:?})")]
    PostAdvanceValidation {
        durable: Option<DurableNewStateCommitCleanupRecord>,
        #[source]
        source: NewStateCommitCleanupAuthorityError,
    },
    #[error("reopen the canonical journal after the NewState cleanup advance")]
    Reopen {
        #[source]
        source: super::canonical_journal_reopen::CanonicalJournalReopenError,
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
pub(in crate::client) fn finalize_new_state_complete(
    journal: TransitionJournalStore,
    authority: NewStateCommitCleanupAuthority<'_>,
) -> Result<TransitionJournalStore, NewStateCommitCleanupPersistenceError> {
    let source = authority.record();
    if source.phase != Phase::Complete {
        return Err(NewStateCommitCleanupPersistenceError::UnexpectedSuccessor { phase: source.phase });
    }

    let (delete, after_delete) = authority
        .attempt_record_bound_delete(&journal)
        .map_err(NewStateCommitCleanupPersistenceError::Authority)?;

    match delete {
        Ok(()) => {
            after_delete
                .revalidate_after_journal_delete(&journal)
                .map_err(NewStateCommitCleanupPersistenceError::Authority)?;
            Ok(journal)
        }
        Err(source) => {
            // Verify the surrounding evidence regardless, so a failed delete is
            // never confused with a corrupted transition.
            let verification = after_delete.revalidate_after_journal_delete(&journal);
            Err(NewStateCommitCleanupPersistenceError::TerminalDelete {
                source: Box::new(source),
                verified: verification.is_ok(),
            })
        }
    }
}
