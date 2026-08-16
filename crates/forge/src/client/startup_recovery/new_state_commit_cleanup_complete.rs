//! Persist the journal-only NewState route from `CommitDecided` to
//! `CommitCleanupComplete`.
//!
//! A first install has no predecessor tree, so this edge performs no namespace
//! effect: it consumes the exact predecessor binding through one conditional
//! advance, destroys the old lock-bearing store, reopens canonically, and
//! revalidates the published successor and its binding. Admissibility is
//! re-checked here so this boundary cannot be reached with a record that still
//! owes a predecessor-wrapper reclaim.

use thiserror::Error;

use crate::{
    Installation, installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::startup_reconciliation::{
    new_state_boot_tail_edge_is_admissible, new_state_boot_tail_terminal_is_admissible,
};
use super::canonical_journal_reopen::{CanonicalJournalReopenError, try_reopen_canonical_journal};

/// Advance the exact first-install cleanup edge and reopen canonically.
pub(in crate::client) fn persist_new_state_boot_tail_edge_retaining_binding(
    installation: &Installation,
    journal: TransitionJournalStore,
    record: &TransitionRecord,
    record_binding: TransitionJournalRecordBinding,
    expected_successor: Phase,
) -> Result<
    (TransitionJournalStore, TransitionRecord, TransitionJournalRecordBinding),
    NewStateCommitCleanupPersistenceError,
> {
    if !new_state_boot_tail_edge_is_admissible(record, expected_successor) {
        drop(journal);
        return Err(NewStateCommitCleanupPersistenceError::NotAdmissible);
    }
    let successor = match record.forward_successor(None) {
        Ok(successor) if successor.phase == expected_successor => successor,
        Ok(successor) => {
            drop(journal);
            return Err(NewStateCommitCleanupPersistenceError::UnexpectedSuccessor { phase: successor.phase });
        }
        Err(source) => {
            drop(journal);
            return Err(NewStateCommitCleanupPersistenceError::RouteConstruction { source });
        }
    };

    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(NewStateCommitCleanupPersistenceError::Installation)?;
    let advance = journal.advance_record_binding(cast, record_binding, &successor);
    drop(journal);
    let successor_binding = advance.map_err(NewStateCommitCleanupPersistenceError::Advance)?;

    let reopened = try_reopen_canonical_journal(installation)?;
    let (reopened, actual) = reopened;
    if actual.as_ref() != Some(&successor) {
        drop(reopened);
        return Err(NewStateCommitCleanupPersistenceError::ReopenedRecordMismatch);
    }
    // The advance returns a binding carrying the old store's per-open identity,
    // which cannot outlive the reopen. The published record is compared exactly
    // above; this re-derives the binding against the reopened store so the
    // caller's next edge is bound to the store it will actually advance.
    drop(successor_binding);
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(NewStateCommitCleanupPersistenceError::Installation)?;
    let successor_binding = reopened
        .record_binding(cast, &successor)
        .map_err(NewStateCommitCleanupPersistenceError::Advance)?;
    Ok((reopened, successor, successor_binding))
}

#[derive(Debug, Error)]
pub(in crate::client) enum NewStateCommitCleanupPersistenceError {
    #[error("the retained CommitDecided record is not the exact first-install cleanup edge")]
    NotAdmissible,
    #[error("derive the sole legal CommitCleanupComplete successor")]
    RouteConstruction { source: CodecError },
    #[error("the derived successor is {phase:?}, not the expected boot-tail phase")]
    UnexpectedSuccessor { phase: Phase },
    #[error("advance the exact bound NewState CommitCleanupComplete record")]
    Advance(#[source] StorageError),
    #[error("revalidate retained installation across the NewState cleanup advance")]
    Installation(#[source] installation::Error),
    #[error("reopen the canonical journal after the NewState cleanup advance")]
    Reopen(#[from] CanonicalJournalReopenError),
    #[error("the reopened canonical journal is not the exact CommitCleanupComplete record")]
    ReopenedRecordMismatch,
    #[error("the published NewState CommitCleanupComplete successor lost its exact record binding")]
    SuccessorBindingChanged,
    #[error("delete the exact first-install terminal record")]
    TerminalDelete {
        source: crate::transition_journal::TransitionJournalRecordDeleteError,
    },
    #[error("the first-install terminal record survived its bound deletion")]
    TerminalRecordSurvived,
    #[error("audit the in-flight transition before the first-install terminal deletion")]
    InFlight { source: Box<crate::db::state::TransitionEvidenceError> },
    #[error("clear the in-flight transition before the first-install terminal deletion")]
    ClearInFlight { source: Box<crate::db::state::TransitionMutationError> },
}

/// Delete the exact first-install terminal record through its retained binding.
///
/// The reblit route proves a completed cleanup namespace before deleting. A
/// first install reclaimed nothing, so there is no such proof to make: the
/// record binding and the admissibility of the record are the whole contract.
pub(in crate::client) fn finalize_new_state_boot_tail_terminal(
    installation: &Installation,
    state_db: &crate::db::state::Database,
    journal: TransitionJournalStore,
    record: &TransitionRecord,
    record_binding: TransitionJournalRecordBinding,
) -> Result<TransitionJournalStore, NewStateCommitCleanupPersistenceError> {
    if !new_state_boot_tail_terminal_is_admissible(record) {
        drop(journal);
        return Err(NewStateCommitCleanupPersistenceError::NotAdmissible);
    }
    // Clear before deleting, never after. A crash between the two leaves a
    // record whose ownership reads cleared with no in-flight row, which the
    // next startup finishes. Deleting first would leave a marked row with no
    // record left to recover it from.
    let pending = state_db
        .audit_in_flight_transition()
        .map_err(|source| NewStateCommitCleanupPersistenceError::InFlight { source: Box::new(source) })?;
    if let Some(row) = pending.as_ref()
        && row.transition_id == record.transition_id
    {
        state_db
            .clear_transition_if_matches(row.state_id, &record.transition_id)
            .map_err(|source| NewStateCommitCleanupPersistenceError::ClearInFlight { source: Box::new(source) })?;
    }
    let cast = installation
        .retained_mutable_cast_directory()
        .map_err(NewStateCommitCleanupPersistenceError::Installation)?;
    journal
        .delete_record_binding(cast, record_binding, record)
        .map_err(|source| NewStateCommitCleanupPersistenceError::TerminalDelete { source })?;
    match journal.load() {
        Ok(None) => Ok(journal),
        Ok(Some(_)) => {
            drop(journal);
            Err(NewStateCommitCleanupPersistenceError::TerminalRecordSurvived)
        }
        Err(source) => {
            drop(journal);
            Err(NewStateCommitCleanupPersistenceError::Advance(source))
        }
    }
}
