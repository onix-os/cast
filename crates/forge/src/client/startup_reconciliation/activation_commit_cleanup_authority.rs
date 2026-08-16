//! Sealed admission for activation commit cleanup, which has no namespace effect.
//!
//! Serves both operations that make a *candidate* state live: `NewState` and
//! `ActivateArchived`. Neither rotates a staging wrapper — the candidate ends up
//! in `/usr` and any predecessor in its own state slot — so there is nothing to
//! reconcile here. ActiveReblit is the contrasting case: its candidate and
//! previous are the same state, so it reaches `CommitCleanupComplete` by
//! exchanging the staging wrapper it activated through.
//!
//! The mandatory `CommitDecided -> CommitCleanupComplete` phase must still be
//! traversed, so this authority proves the record and database are exact and
//! then advances, holding no namespace evidence at all.
//!
//! Deliberately independent of `active_reblit_commit_cleanup_authority`: that
//! type's durable evidence is wrapper-shaped (retained exchange parents, a
//! post-exchange projection) with no counterpart here, and it is verified by the
//! crash matrix. See `plans/future_impl.md` §1.1b and §1.2.

use crate::{
    Installation, db,
    transition_journal::{Operation, Phase, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::super::active_state_snapshot::ActiveStateReservation;
use super::{
    DatabaseEvidence, InspectionError,
    activation_namespace::{ActivationTerminalNamespaceInspection, ActivationTerminalNamespaceProof},
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
pub(in crate::client) enum ActivationTerminalStep {
    /// `BootSyncStarted -> BootSyncComplete`. The only receipt-bound step: its
    /// successor must carry the exact pair the source record already binds, so
    /// it uses the typed successor rather than a generic advance.
    BootSyncStarted,
    /// `BootSyncComplete -> CommitDecided`, once boot publication has finished.
    BootSyncComplete,
    /// `CommitDecided -> CommitCleanupComplete`. NewState has no cleanup effect.
    CommitCleanup,
    /// `CommitCleanupComplete -> Complete`.
    CleanupComplete,
    /// Terminal `Complete`: the record is deleted rather than advanced, so this
    /// step has no successor phase.
    Finalize,
}

impl ActivationTerminalStep {
    const fn source(self) -> Phase {
        match self {
            Self::BootSyncStarted => Phase::BootSyncStarted,
            Self::BootSyncComplete => Phase::BootSyncComplete,
            Self::CommitCleanup => Phase::CommitDecided,
            Self::CleanupComplete => Phase::CommitCleanupComplete,
            Self::Finalize => Phase::Complete,
        }
    }

    /// The phase a record must already be at for this step to apply.
    pub(in crate::client) const fn source_phase(self) -> Phase {
        self.source()
    }

    /// Whether this step advances the record. `Finalize` deletes it instead.
    pub(in crate::client) const fn advances(self) -> bool {
        !matches!(self, Self::Finalize)
    }

    /// Whether the successor must be built from the record's bound receipt pair
    /// rather than a generic forward advance.
    pub(in crate::client) const fn is_receipt_bound(self) -> bool {
        matches!(self, Self::BootSyncStarted)
    }

    pub(in crate::client) const fn successor_phase(self) -> Phase {
        self.successor()
    }

    const fn successor(self) -> Phase {
        match self {
            Self::BootSyncStarted => Phase::BootSyncComplete,
            Self::BootSyncComplete => Phase::CommitDecided,
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
pub(in crate::client) enum ActivationCommitCleanupAdmission<'reservation> {
    /// The record is not a NewState commit-cleanup source.
    NotApplicable,
    /// The record is a NewState commit-cleanup source but its evidence is not
    /// currently exact; the caller must not advance. The reason is carried
    /// because this admission is reached deep inside a live transition, where
    /// "deferred" alone is not diagnosable.
    Deferred(ActivationCommitCleanupDeferral),
    /// Exact evidence; the caller may advance the record.
    Ready(ActivationCommitCleanupAuthority<'reservation>),
}

/// Why exact evidence could not be established.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) enum ActivationCommitCleanupDeferral {
    /// The terminal namespace did not match the record's expected layout at
    /// admission.
    NamespaceAtBegin,
    /// The namespace changed between the bracketing captures.
    NamespaceAtFinish,
    /// The bracketing database captures disagreed.
    DatabaseUnstable,
}

/// Exact `NewState + CommitDecided` evidence. Holds no namespace member: there
/// is no cleanup effect to reconcile.
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) struct ActivationCommitCleanupAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: ActivationTerminalNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    step: ActivationTerminalStep,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
impl<'reservation> ActivationCommitCleanupAuthority<'reservation> {
    /// Capture is read-only and performs no mutation.
    pub(in crate::client) fn capture(
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
        step: ActivationTerminalStep,
    ) -> Result<ActivationCommitCleanupAdmission<'reservation>, ActivationCommitCleanupAuthorityError> {
        if !exact_activation_terminal_source(record, step) {
            return Ok(ActivationCommitCleanupAdmission::NotApplicable);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding = journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        require_binding(installation, journal, &journal_record_binding, record)?;

        // Two bracketing database captures must agree, so a concurrent writer
        // cannot slip a different row between admission and use.
        // The namespace must already be the terminal shape the record implies:
        // candidate live, predecessor archived. Assessed through the shared
        // policy, which is record-driven (`plans/future_impl.md` §1.1c).
        let namespace_inspection = match ActivationTerminalNamespaceInspection::begin(
            installation,
            journal,
            &journal_record_binding,
            record,
        ) {
            Ok(inspection) => inspection,
            Err(_) => {
                return Ok(ActivationCommitCleanupAdmission::Deferred(
                    ActivationCommitCleanupDeferral::NamespaceAtBegin,
                ));
            }
        };

        let database = inspect_database(record, state_db, initial_in_flight)?;
        let in_flight_after = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
        let database_after = inspect_database(record, state_db, in_flight_after)?;
        if database != database_after {
            return Ok(ActivationCommitCleanupAdmission::Deferred(
                ActivationCommitCleanupDeferral::DatabaseUnstable,
            ));
        }

        let namespace = match namespace_inspection.finish(installation, journal, &journal_record_binding, record) {
            Ok(namespace) => namespace,
            Err(_) => {
                return Ok(ActivationCommitCleanupAdmission::Deferred(
                    ActivationCommitCleanupDeferral::NamespaceAtFinish,
                ));
            }
        };

        installation.revalidate_mutable_namespace()?;
        require_binding(installation, journal, &journal_record_binding, record)?;
        Ok(ActivationCommitCleanupAdmission::Ready(Self {
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
    ) -> Result<(), ActivationCommitCleanupAuthorityError> {
        require_binding(&self.installation, journal, &self.journal_record_binding, &self.record)?;
        self.installation.revalidate_mutable_namespace()?;
        if !exact_activation_terminal_source(&self.record, self.step) {
            return Err(ActivationCommitCleanupAuthorityError::SourceContract);
        }
        let in_flight = self
            .state_db
            .audit_in_flight_transition()
            .map_err(InspectionError::from)?;
        let database = inspect_database(&self.record, &self.state_db, in_flight)?;
        if database != self.database {
            return Err(ActivationCommitCleanupAuthorityError::DatabaseChanged);
        }
        self.namespace
            .revalidate(&self.installation, &self.record)
            .map_err(|source| ActivationCommitCleanupAuthorityError::Namespace(Box::new(source)))?;
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
        (
            TransitionJournalRecordBinding,
            ActivationCommitCleanupPostAdvanceAuthority<'reservation>,
        ),
        ActivationCommitCleanupAuthorityError,
    > {
        self.revalidate(journal)?;
        if successor.phase != self.step.successor()
            || successor.transition_id != self.record.transition_id
            || successor.generation != self.record.generation.saturating_add(1)
        {
            return Err(ActivationCommitCleanupAuthorityError::UnexpectedSuccessor);
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
            ActivationCommitCleanupPostAdvanceAuthority {
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
pub(in crate::client) struct ActivationCommitCleanupPostAdvanceAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    completed_record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: ActivationTerminalNamespaceProof,
    step: ActivationTerminalStep,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
impl ActivationCommitCleanupPostAdvanceAuthority<'_> {
    /// Prove the published successor is the exact record this authority
    /// advanced to, bound to the same journal store.
    pub(in crate::client) fn revalidate_successor_same_store(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
    ) -> Result<(), ActivationCommitCleanupAuthorityError> {
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
    ) -> Result<(), ActivationCommitCleanupAuthorityError> {
        self.revalidate_successor(journal, successor_binding, successor, SuccessorBindingMode::Reopened)
    }

    fn revalidate_successor(
        &self,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        successor: &TransitionRecord,
        mode: SuccessorBindingMode,
    ) -> Result<(), ActivationCommitCleanupAuthorityError> {
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
            return Err(ActivationCommitCleanupAuthorityError::SuccessorRecordBindingChanged);
        }
        if successor.phase != self.step.successor()
            || successor.transition_id != self.completed_record.transition_id
            || successor.generation != self.completed_record.generation.saturating_add(1)
        {
            return Err(ActivationCommitCleanupAuthorityError::UnexpectedSuccessor);
        }
        self.installation.revalidate_mutable_namespace()?;
        let in_flight = self
            .state_db
            .audit_in_flight_transition()
            .map_err(InspectionError::from)?;
        let database = inspect_database(successor, &self.state_db, in_flight)?;
        if database != self.database {
            return Err(ActivationCommitCleanupAuthorityError::DatabaseChanged);
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
impl<'reservation> ActivationCommitCleanupAuthority<'reservation> {
    /// Delete the terminal record, ending the transition.
    ///
    /// Only valid for [`ActivationTerminalStep::Finalize`]: every other step
    /// advances instead. The returned authority proves the deletion landed and
    /// that the surrounding evidence is unchanged.
    pub(in crate::client) fn attempt_record_bound_delete(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        (
            Result<(), crate::transition_journal::TransitionJournalRecordDeleteError>,
            ActivationTerminalAfterDeleteAuthority<'reservation>,
        ),
        ActivationCommitCleanupAuthorityError,
    > {
        if self.step.advances() {
            return Err(ActivationCommitCleanupAuthorityError::UnexpectedSuccessor);
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
            ActivationTerminalAfterDeleteAuthority {
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
pub(in crate::client) struct ActivationTerminalAfterDeleteAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: ActivationTerminalNamespaceProof,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
impl ActivationTerminalAfterDeleteAuthority<'_> {
    /// Prove the record is gone and the surrounding evidence still holds.
    pub(in crate::client) fn revalidate_after_journal_delete(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<(), ActivationCommitCleanupAuthorityError> {
        if journal.load()?.is_some() {
            return Err(ActivationCommitCleanupAuthorityError::RecordStillPresent);
        }
        self.installation.revalidate_mutable_namespace()?;
        let in_flight = self
            .state_db
            .audit_in_flight_transition()
            .map_err(InspectionError::from)?;
        let database = inspect_database(&self.record, &self.state_db, in_flight)?;
        if database != self.database {
            return Err(ActivationCommitCleanupAuthorityError::DatabaseChanged);
        }
        self.namespace
            .revalidate(&self.installation, &self.record)
            .map_err(|source| ActivationCommitCleanupAuthorityError::Namespace(Box::new(source)))?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn require_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), ActivationCommitCleanupAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(ActivationCommitCleanupAuthorityError::JournalRecordBindingMismatch);
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if !journal.has_record_binding(cast, binding, record)? {
        return Err(ActivationCommitCleanupAuthorityError::JournalRecordBindingMismatch);
    }
    Ok(())
}

/// A NewState transition at the exact terminal phase `step` advances from.
/// `archive_previous` with a distinct candidate is what distinguishes it from
/// every ActiveReblit source.
fn exact_activation_terminal_source(record: &TransitionRecord, step: ActivationTerminalStep) -> bool {
    // ActiveReblit repairs a state in place and keeps its own terminal
    // authority, whose evidence is built around the staging-wrapper exchange it
    // performs. The other two operations leave the same terminal shape — a
    // candidate distinct from its predecessor, with no cleanup effect to
    // reconcile — so one authority serves both (`plans/future_impl.md` §1.2a).
    if !matches!(record.operation, Operation::NewState | Operation::ActivateArchived) {
        return false;
    }
    if record.phase != step.source() || record.rollback.is_some() || record.candidate.id.is_none() {
        return false;
    }
    // The predecessor fields must agree with what the options declare.
    // Replacing or activating over an active state archives a distinct
    // predecessor; a first install has none at all.
    if record.options.archive_previous {
        record.previous.id.is_some() && record.candidate.id != record.previous.id
    } else {
        // Only a fresh install legitimately has nothing preceding it; there is
        // always something to archive when activating an existing state.
        record.operation == Operation::NewState && record.previous.id.is_none()
    }
}

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // consumed by the coordinated NewState route (Slice 5)
pub(in crate::client) enum ActivationCommitCleanupAuthorityError {
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
        assert!(
            commit_decided.options.archive_previous,
            "ActiveState previous implies archiving"
        );

        let mut cleanup_complete = commit_decided.clone();
        cleanup_complete.phase = Phase::CommitCleanupComplete;

        // Each step admits exactly the phase it advances from, and no other.
        assert!(exact_activation_terminal_source(
            &commit_decided,
            ActivationTerminalStep::CommitCleanup
        ));
        assert!(!exact_activation_terminal_source(
            &commit_decided,
            ActivationTerminalStep::CleanupComplete
        ));
        assert!(exact_activation_terminal_source(
            &cleanup_complete,
            ActivationTerminalStep::CleanupComplete
        ));
        assert!(!exact_activation_terminal_source(
            &cleanup_complete,
            ActivationTerminalStep::CommitCleanup
        ));

        assert_eq!(
            ActivationTerminalStep::CommitCleanup.successor(),
            Phase::CommitCleanupComplete
        );
        assert_eq!(ActivationTerminalStep::CleanupComplete.successor(), Phase::Complete);

        // The boot tail rejoins the shared chain at `BootSyncComplete`.
        let mut boot_sync_complete = commit_decided.clone();
        boot_sync_complete.phase = Phase::BootSyncComplete;
        assert!(exact_activation_terminal_source(
            &boot_sync_complete,
            ActivationTerminalStep::BootSyncComplete
        ));
        assert!(!exact_activation_terminal_source(
            &boot_sync_complete,
            ActivationTerminalStep::CommitCleanup
        ));
        assert_eq!(
            ActivationTerminalStep::BootSyncComplete.successor(),
            Phase::CommitDecided
        );

        // Entering `BootSyncComplete` is the one receipt-bound edge.
        let mut boot_sync_started = commit_decided.clone();
        boot_sync_started.phase = Phase::BootSyncStarted;
        assert!(exact_activation_terminal_source(
            &boot_sync_started,
            ActivationTerminalStep::BootSyncStarted
        ));
        assert_eq!(
            ActivationTerminalStep::BootSyncStarted.successor(),
            Phase::BootSyncComplete
        );
        assert!(ActivationTerminalStep::BootSyncStarted.is_receipt_bound());
        for step in [
            ActivationTerminalStep::BootSyncComplete,
            ActivationTerminalStep::CommitCleanup,
            ActivationTerminalStep::CleanupComplete,
            ActivationTerminalStep::Finalize,
        ] {
            assert!(!step.is_receipt_bound(), "{step:?} must not be receipt-bound");
        }

        // Only `Finalize` deletes instead of advancing.
        assert!(ActivationTerminalStep::BootSyncStarted.advances());
        assert!(ActivationTerminalStep::BootSyncComplete.advances());
        assert!(ActivationTerminalStep::CommitCleanup.advances());
        assert!(ActivationTerminalStep::CleanupComplete.advances());
        assert!(!ActivationTerminalStep::Finalize.advances());
    }

    #[test]
    fn no_activereblit_shaped_record_is_ever_an_exact_source() {
        let exact = new_state_commit_decided();
        let step = ActivationTerminalStep::CommitCleanup;

        // Every ActiveReblit-shaped record belongs to the other authority.
        let mut wrong_operation = exact.clone();
        wrong_operation.operation = Operation::ActiveReblit;
        assert!(!exact_activation_terminal_source(&wrong_operation, step));

        // ActivateArchived leaves the same terminal shape and is served here.
        let mut activate_archived = exact.clone();
        activate_archived.operation = Operation::ActivateArchived;
        assert!(exact_activation_terminal_source(&activate_archived, step));

        // ...but it always archives something, so the first-install shape is
        // never valid for it.
        let mut archived_without_previous = activate_archived.clone();
        archived_without_previous.options.archive_previous = false;
        archived_without_previous.previous.id = None;
        assert!(!exact_activation_terminal_source(&archived_without_previous, step));

        // A non-archiving NewState is the first-install shape: legitimate, but
        // only when it also has no predecessor.
        let mut no_archive = exact.clone();
        no_archive.options.archive_previous = false;
        assert!(
            !exact_activation_terminal_source(&no_archive, step),
            "archive_previous=false with a predecessor is contradictory",
        );
        no_archive.previous.id = None;
        assert!(
            exact_activation_terminal_source(&no_archive, step),
            "a first install has no predecessor and archives nothing",
        );

        // ...and an archiving record must actually name its predecessor.
        let mut archiving_without_previous = exact.clone();
        archiving_without_previous.previous.id = None;
        assert!(!exact_activation_terminal_source(&archiving_without_previous, step));

        // Candidate == previous is the in-place repair shape, never NewState.
        let mut same_state = exact.clone();
        same_state.previous.id = same_state.candidate.id;
        assert!(!exact_activation_terminal_source(&same_state, step));

        let mut missing_candidate = exact.clone();
        missing_candidate.candidate.id = None;
        assert!(!exact_activation_terminal_source(&missing_candidate, step));
    }
}
