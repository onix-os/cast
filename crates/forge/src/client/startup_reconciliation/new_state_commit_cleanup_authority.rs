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
use super::{
    DatabaseEvidence, InspectionError,
    activation_namespace::{NewStateTerminalNamespaceInspection, NewStateTerminalNamespaceProof},
    inspect_database,
};

/// Which terminal record advance an authority is admitting.
///
/// Both steps have identical evidence requirements — record gate, database,
/// terminal namespace layout, record binding — and differ only in the phase they
/// advance from and to. Parameterizing avoids two near-identical authorities
/// (`plans/future_impl.md` §1.1c).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) enum NewStateTerminalStep {
    /// `CommitDecided -> CommitCleanupComplete`. NewState has no cleanup effect.
    CommitCleanup,
    /// `CommitCleanupComplete -> Complete`.
    CleanupComplete,
    /// Terminal `Complete`: the record is deleted rather than advanced, so this
    /// step has no successor phase.
    Finalize,
}

impl NewStateTerminalStep {
    const fn source(self) -> Phase {
        match self {
            Self::CommitCleanup => Phase::CommitDecided,
            Self::CleanupComplete => Phase::CommitCleanupComplete,
            Self::Finalize => Phase::Complete,
        }
    }

    /// Whether this step advances the record. `Finalize` deletes it instead.
    pub(in crate::client) const fn advances(self) -> bool {
        !matches!(self, Self::Finalize)
    }

    pub(in crate::client) const fn successor_phase(self) -> Phase {
        self.successor()
    }

    const fn successor(self) -> Phase {
        match self {
            Self::CommitCleanup => Phase::CommitCleanupComplete,
            // `Finalize` never advances; `advances()` gates every caller, and
            // reporting `Complete` keeps the accessor total.
            Self::CleanupComplete | Self::Finalize => Phase::Complete,
        }
    }
}

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
    namespace: NewStateTerminalNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    step: NewStateTerminalStep,
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
        step: NewStateTerminalStep,
    ) -> Result<NewStateCommitCleanupAdmission<'reservation>, NewStateCommitCleanupAuthorityError> {
        if !exact_new_state_terminal_source(record, step) {
            return Ok(NewStateCommitCleanupAdmission::NotApplicable);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding = journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        require_binding(installation, journal, &journal_record_binding, record)?;

        // Two bracketing database captures must agree, so a concurrent writer
        // cannot slip a different row between admission and use.
        // The namespace must already be the terminal shape the record implies:
        // candidate live, predecessor archived. Assessed through the shared
        // policy, which is record-driven (`plans/future_impl.md` §1.1c).
        let namespace_inspection =
            match NewStateTerminalNamespaceInspection::begin(installation, journal, &journal_record_binding, record) {
                Ok(inspection) => inspection,
                Err(_) => return Ok(NewStateCommitCleanupAdmission::Deferred),
            };

        let database = inspect_database(record, state_db, initial_in_flight)?;
        let in_flight_after = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
        let database_after = inspect_database(record, state_db, in_flight_after)?;
        if database != database_after {
            return Ok(NewStateCommitCleanupAdmission::Deferred);
        }

        let namespace =
            match namespace_inspection.finish(installation, journal, &journal_record_binding, record) {
                Ok(namespace) => namespace,
                Err(_) => return Ok(NewStateCommitCleanupAdmission::Deferred),
            };

        installation.revalidate_mutable_namespace()?;
        require_binding(installation, journal, &journal_record_binding, record)?;
        Ok(NewStateCommitCleanupAdmission::Ready(Self {
            installation: installation.clone(),
            state_db: state_db.clone(),
            record: record.clone(),
            database,
            namespace,
            journal_record_binding,
            step,
            _active_state_reservation: active_state_reservation,
        }))
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.record
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.installation
    }

    /// Repeat the exact admission evidence. Callers must invoke this
    /// immediately before persisting the successor.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), NewStateCommitCleanupAuthorityError> {
        require_binding(&self.installation, journal, &self.journal_record_binding, &self.record)?;
        self.installation.revalidate_mutable_namespace()?;
        if !exact_new_state_terminal_source(&self.record, self.step) {
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
        self.namespace
            .revalidate(&self.installation, &self.record)
            .map_err(|source| NewStateCommitCleanupAuthorityError::Namespace(Box::new(source)))?;
        self.installation.revalidate_mutable_namespace()?;
        require_binding(&self.installation, journal, &self.journal_record_binding, &self.record)
    }
    /// Advance the record and its binding together to `CommitCleanupComplete`.
    ///
    /// There is no effect to reconcile first: this authority *is* the durable
    /// one, because NewState performs no cleanup exchange. The caller must have
    /// derived `successor` from `forward_successor(None)` on this record.
    pub(in crate::client) fn advance_record_binding(
        self,
        journal: &TransitionJournalStore,
        successor: &TransitionRecord,
    ) -> Result<
        (TransitionJournalRecordBinding, NewStateCommitCleanupPostAdvanceAuthority<'reservation>),
        NewStateCommitCleanupAuthorityError,
    > {
        self.revalidate(journal)?;
        if successor.phase != self.step.successor()
            || successor.transition_id != self.record.transition_id
            || successor.generation != self.record.generation.saturating_add(1)
        {
            return Err(NewStateCommitCleanupAuthorityError::UnexpectedSuccessor);
        }

        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            step,
            _active_state_reservation,
        } = self;
        let cast = installation.retained_mutable_cast_directory()?;
        let successor_binding = journal.advance_record_binding(cast, journal_record_binding, successor)?;
        Ok((
            successor_binding,
            NewStateCommitCleanupPostAdvanceAuthority {
                installation,
                state_db,
                completed_record: record,
                database,
                namespace,
                step,
                _active_state_reservation,
            },
        ))
    }
}

/// Retained evidence after the cleanup record has been advanced.
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) struct NewStateCommitCleanupPostAdvanceAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    completed_record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: NewStateTerminalNamespaceProof,
    step: NewStateTerminalStep,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
impl NewStateCommitCleanupPostAdvanceAuthority<'_> {
    /// Prove the published successor is the exact record this authority
    /// advanced to, bound to the same journal store.
    pub(in crate::client) fn revalidate_successor_same_store(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
    ) -> Result<(), NewStateCommitCleanupAuthorityError> {
        self.revalidate_successor(journal, successor_binding, successor, SuccessorBindingMode::SameStore)
    }

    /// Same proof against a journal that has been closed and reopened, where the
    /// original store identity no longer applies and only the reopened record
    /// binding can authenticate the successor.
    pub(in crate::client) fn revalidate_successor_reopened(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
    ) -> Result<(), NewStateCommitCleanupAuthorityError> {
        self.revalidate_successor(journal, successor_binding, successor, SuccessorBindingMode::Reopened)
    }

    fn revalidate_successor(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
        mode: SuccessorBindingMode,
    ) -> Result<(), NewStateCommitCleanupAuthorityError> {
        let cast = self.installation.retained_mutable_cast_directory()?;
        let exact = match mode {
            SuccessorBindingMode::SameStore => {
                journal.has_record_store_binding(successor_binding)
                    && journal.has_record_binding(cast, successor_binding, successor)?
            }
            SuccessorBindingMode::Reopened => {
                journal.has_reopened_record_binding(cast, successor_binding, successor)?
            }
        };
        if !exact {
            return Err(NewStateCommitCleanupAuthorityError::SuccessorRecordBindingChanged);
        }
        if successor.phase != self.step.successor()
            || successor.transition_id != self.completed_record.transition_id
            || successor.generation != self.completed_record.generation.saturating_add(1)
        {
            return Err(NewStateCommitCleanupAuthorityError::UnexpectedSuccessor);
        }
        self.installation.revalidate_mutable_namespace()?;
        let in_flight = self
            .state_db
            .audit_in_flight_transition()
            .map_err(InspectionError::from)?;
        let database = inspect_database(successor, &self.state_db, in_flight)?;
        if database != self.database {
            return Err(NewStateCommitCleanupAuthorityError::DatabaseChanged);
        }
        Ok(())
    }

    pub(in crate::client) fn completed_record(&self) -> &TransitionRecord {
        &self.completed_record
    }
}

#[derive(Clone, Copy)]
enum SuccessorBindingMode {
    SameStore,
    Reopened,
}

#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
impl<'reservation> NewStateCommitCleanupAuthority<'reservation> {
    /// Delete the terminal record, ending the transition.
    ///
    /// Only valid for [`NewStateTerminalStep::Finalize`]: every other step
    /// advances instead. The returned authority proves the deletion landed and
    /// that the surrounding evidence is unchanged.
    pub(in crate::client) fn attempt_record_bound_delete(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        (
            Result<(), crate::transition_journal::TransitionJournalRecordDeleteError>,
            NewStateTerminalAfterDeleteAuthority<'reservation>,
        ),
        NewStateCommitCleanupAuthorityError,
    > {
        if self.step.advances() {
            return Err(NewStateCommitCleanupAuthorityError::UnexpectedSuccessor);
        }
        self.revalidate(journal)?;
        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            step: _,
            _active_state_reservation,
        } = self;
        let cast = installation.retained_mutable_cast_directory()?;
        let delete = journal.delete_record_binding(cast, journal_record_binding, &record);
        Ok((
            delete,
            NewStateTerminalAfterDeleteAuthority {
                installation,
                state_db,
                record,
                database,
                namespace,
                _active_state_reservation,
            },
        ))
    }
}

/// Evidence retained across the terminal record deletion.
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) struct NewStateTerminalAfterDeleteAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: NewStateTerminalNamespaceProof,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
impl NewStateTerminalAfterDeleteAuthority<'_> {
    /// Prove the record is gone and the surrounding evidence still holds.
    pub(in crate::client) fn revalidate_after_journal_delete(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<(), NewStateCommitCleanupAuthorityError> {
        if journal.load()?.is_some() {
            return Err(NewStateCommitCleanupAuthorityError::RecordStillPresent);
        }
        self.installation.revalidate_mutable_namespace()?;
        let in_flight = self
            .state_db
            .audit_in_flight_transition()
            .map_err(InspectionError::from)?;
        let database = inspect_database(&self.record, &self.state_db, in_flight)?;
        if database != self.database {
            return Err(NewStateCommitCleanupAuthorityError::DatabaseChanged);
        }
        self.namespace
            .revalidate(&self.installation, &self.record)
            .map_err(|source| NewStateCommitCleanupAuthorityError::Namespace(Box::new(source)))?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
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

/// A NewState transition at the exact terminal phase `step` advances from.
/// `archive_previous` with a distinct candidate is what distinguishes it from
/// every ActiveReblit source.
fn exact_new_state_terminal_source(record: &TransitionRecord, step: NewStateTerminalStep) -> bool {
    record.operation == Operation::NewState
        && record.phase == step.source()
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
    #[error("the published successor record binding changed")]
    SuccessorRecordBindingChanged,
    #[error("the successor is not the exact cleanup-complete record")]
    UnexpectedSuccessor,
    #[error("the state database changed during admission")]
    DatabaseChanged,
    #[error("the terminal record is still present after its bound deletion")]
    RecordStillPresent,
    #[error("terminal namespace")]
    Namespace(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error("installation: {0}")]
    Installation(#[from] crate::installation::Error),
    #[error("journal: {0}")]
    Journal(#[from] crate::transition_journal::StorageError),
    #[error("inspection: {0}")]
    Inspection(#[from] InspectionError),
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        state::TransitionId,
        transition_journal::{
            BootId, MountNamespaceIdentity, Previous, PreviousOrigin, QuarantineName, RuntimeEpoch,
            RuntimeTreeIdentity, TreeToken,
        },
    };

    /// A committed, archiving NewState record: the exact shape
    /// `active_reblit_commit_cleanup_authority` refuses on three separate counts.
    fn new_state_commit_decided() -> TransitionRecord {
        let mut record = TransitionRecord::preparing(
            TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            RuntimeEpoch {
                boot_id: BootId::parse("01234567-89ab-4cde-8f01-23456789abcd").unwrap(),
                mount_namespace: MountNamespaceIdentity { st_dev: 30, inode: 31 },
            },
            Operation::NewState,
            None,
            TreeToken::parse("a".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
            RuntimeTreeIdentity {
                st_dev: 10,
                inode: 10,
                mount_id: 12,
            },
            Previous {
                id: Some(41),
                tree_token: TreeToken::parse("b".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
                usr_runtime_identity: RuntimeTreeIdentity {
                    st_dev: 10,
                    inode: 20,
                    mount_id: 12,
                },
                origin: PreviousOrigin::ActiveState,
            },
            true,
            true,
            QuarantineName::parse("new-state-cleanup-test").unwrap(),
        )
        .unwrap();
        record.phase = Phase::CommitDecided;
        record.candidate.id = Some(42);
        record
    }

    #[test]
    fn each_terminal_step_admits_only_its_own_source_phase() {
        let commit_decided = new_state_commit_decided();
        assert!(commit_decided.options.archive_previous, "ActiveState previous implies archiving");

        let mut cleanup_complete = commit_decided.clone();
        cleanup_complete.phase = Phase::CommitCleanupComplete;

        // Each step admits exactly the phase it advances from, and no other.
        assert!(exact_new_state_terminal_source(
            &commit_decided,
            NewStateTerminalStep::CommitCleanup
        ));
        assert!(!exact_new_state_terminal_source(
            &commit_decided,
            NewStateTerminalStep::CleanupComplete
        ));
        assert!(exact_new_state_terminal_source(
            &cleanup_complete,
            NewStateTerminalStep::CleanupComplete
        ));
        assert!(!exact_new_state_terminal_source(
            &cleanup_complete,
            NewStateTerminalStep::CommitCleanup
        ));

        assert_eq!(NewStateTerminalStep::CommitCleanup.successor(), Phase::CommitCleanupComplete);
        assert_eq!(NewStateTerminalStep::CleanupComplete.successor(), Phase::Complete);
    }

    #[test]
    fn no_activereblit_shaped_record_is_ever_an_exact_source() {
        let exact = new_state_commit_decided();
        let step = NewStateTerminalStep::CommitCleanup;

        // Every ActiveReblit-shaped record belongs to the other authority.
        let mut wrong_operation = exact.clone();
        wrong_operation.operation = Operation::ActiveReblit;
        assert!(!exact_new_state_terminal_source(&wrong_operation, step));

        // A non-archiving NewState is the deferred no-previous case, not this one.
        let mut no_archive = exact.clone();
        no_archive.options.archive_previous = false;
        assert!(!exact_new_state_terminal_source(&no_archive, step));

        // Candidate == previous is the in-place repair shape, never NewState.
        let mut same_state = exact.clone();
        same_state.previous.id = same_state.candidate.id;
        assert!(!exact_new_state_terminal_source(&same_state, step));

        let mut missing_candidate = exact.clone();
        missing_candidate.candidate.id = None;
        assert!(!exact_new_state_terminal_source(&missing_candidate, step));
    }

}
