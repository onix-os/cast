//! Sealed admission for NewState commit cleanup, which has no namespace effect.
//!
//! ActiveReblit reaches `CommitCleanupComplete` by exchanging the staging
//! wrapper it activated through, because its candidate and previous are the
//! same state. NewState never rotates a wrapper — its candidate moved from
//! staging into `/usr` and its predecessor moved into its own state slot — so
//! there is nothing to reconcile here. The mandatory
//! `CommitDecided -> CommitCleanupComplete` phase must still be traversed, so
//! this authority proves the record and database are exact and then advances,
//! holding no namespace evidence at all.
//!
//! Deliberately independent of `active_reblit_commit_cleanup_authority`: that
//! type's durable evidence is wrapper-shaped (retained exchange parents, a
//! post-exchange projection) with no NewState counterpart, and it is verified
//! by the crash matrix. See `plans/future_impl.md` §1.1b.

use crate::{
    Installation, db,
    transition_journal::{
        Operation, Phase, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::super::active_state_snapshot::ActiveStateReservation;
use super::{DatabaseEvidence, InspectionError, inspect_database};

/// Exact result of read-only NewState commit-cleanup admission.
// Forward scaffolding: consumed by the coordinated NewState route once Slice 5
// wires `apply_new_state_candidate` live (`plans/future_impl.md` §1.1b).
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) enum NewStateCommitCleanupAdmission<'reservation> {
    /// The record is not a NewState commit-cleanup source.
    NotApplicable,
    /// The record is a NewState commit-cleanup source but its evidence is not
    /// currently exact; the caller must not advance.
    Deferred,
    /// Exact evidence; the caller may advance the record.
    Ready(NewStateCommitCleanupAuthority<'reservation>),
}

/// Exact `NewState + CommitDecided` evidence. Holds no namespace member: there
/// is no cleanup effect to reconcile.
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) struct NewStateCommitCleanupAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
impl<'reservation> NewStateCommitCleanupAuthority<'reservation> {
    /// Capture is read-only and performs no mutation.
    pub(in crate::client) fn capture(
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<NewStateCommitCleanupAdmission<'reservation>, NewStateCommitCleanupAuthorityError> {
        if !exact_new_state_commit_cleanup_source(record) {
            return Ok(NewStateCommitCleanupAdmission::NotApplicable);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding = journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        require_binding(installation, journal, &journal_record_binding, record)?;

        // Two bracketing database captures must agree, so a concurrent writer
        // cannot slip a different row between admission and use.
        let database = inspect_database(record, state_db, initial_in_flight)?;
        let in_flight_after = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
        let database_after = inspect_database(record, state_db, in_flight_after)?;
        if database != database_after {
            return Ok(NewStateCommitCleanupAdmission::Deferred);
        }

        installation.revalidate_mutable_namespace()?;
        require_binding(installation, journal, &journal_record_binding, record)?;
        Ok(NewStateCommitCleanupAdmission::Ready(Self {
            installation: installation.clone(),
            state_db: state_db.clone(),
            record: record.clone(),
            database,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        }))
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.record
    }

    /// Repeat the exact admission evidence. Callers must invoke this
    /// immediately before persisting the successor.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), NewStateCommitCleanupAuthorityError> {
        require_binding(&self.installation, journal, &self.journal_record_binding, &self.record)?;
        self.installation.revalidate_mutable_namespace()?;
        if !exact_new_state_commit_cleanup_source(&self.record) {
            return Err(NewStateCommitCleanupAuthorityError::SourceContract);
        }
        let in_flight = self
            .state_db
            .audit_in_flight_transition()
            .map_err(InspectionError::from)?;
        let database = inspect_database(&self.record, &self.state_db, in_flight)?;
        if database != self.database {
            return Err(NewStateCommitCleanupAuthorityError::DatabaseChanged);
        }
        self.installation.revalidate_mutable_namespace()?;
        require_binding(&self.installation, journal, &self.journal_record_binding, &self.record)
    }
}

fn require_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), NewStateCommitCleanupAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(NewStateCommitCleanupAuthorityError::JournalRecordBindingMismatch);
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if !journal.has_record_binding(cast, binding, record)? {
        return Err(NewStateCommitCleanupAuthorityError::JournalRecordBindingMismatch);
    }
    Ok(())
}

/// A NewState transition that has committed and now only needs to traverse the
/// mandatory cleanup phase. `archive_previous` with a distinct candidate is what
/// distinguishes it from every ActiveReblit source.
fn exact_new_state_commit_cleanup_source(record: &TransitionRecord) -> bool {
    record.operation == Operation::NewState
        && record.phase == Phase::CommitDecided
        && record.rollback.is_none()
        && record.options.archive_previous
        && record.candidate.id.is_some()
        && record.previous.id.is_some()
        && record.candidate.id != record.previous.id
}

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) enum NewStateCommitCleanupAuthorityError {
    #[error("the retained journal record binding no longer matches")]
    JournalRecordBindingMismatch,
    #[error("the record is no longer an exact NewState commit-cleanup source")]
    SourceContract,
    #[error("the state database changed during admission")]
    DatabaseChanged,
    #[error("installation: {0}")]
    Installation(#[from] crate::installation::Error),
    #[error("journal: {0}")]
    Journal(#[from] crate::transition_journal::StorageError),
    #[error("inspection: {0}")]
    Inspection(#[from] InspectionError),
}

// The admission gate is exercised against real records by the Slice 5 wiring,
// which owns the journal fixtures needed to build a NewState `CommitDecided`
// record; `TransitionRecord` has no builder reachable from this module.
