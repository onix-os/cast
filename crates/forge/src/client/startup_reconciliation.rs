//! Read-only classification of one durable transition found during startup.
//!
//! This module owns evidence; it does not execute recovery.  In particular,
//! opening known names is not treated as a complete activation-namespace
//! proof, and an unresolved state-slot marker hardlink is never authorized
//! here.

use std::{fmt, path::PathBuf};

use crate::{
    Installation, db,
    state::{self, TransitionId},
    transition_journal::{
        Operation, Phase, RecoveryDisposition, RuntimeEpoch, RuntimeEvidenceError, RuntimeTreeIdentity,
        TransitionJournalStore, TransitionRecord,
    },
    tree_marker::{RetainedTreeMarker, TreeMarkerError, TreeMarkerStore},
};

const MAX_KNOWN_TREE_LOCATIONS: usize = 5;

/// The installation/global-lock, journal, and state-database capabilities
/// retained while startup proves that no interrupted transition exists.
#[derive(Debug)]
pub(super) struct StartupRecoveryAuthority {
    _installation: Installation,
    journal: TransitionJournalStore,
    state_db: db::state::Database,
}

impl StartupRecoveryAuthority {
    pub(super) fn new(
        installation: &Installation,
        journal: TransitionJournalStore,
        state_db: &db::state::Database,
    ) -> Self {
        let retained = state_db.clone();
        debug_assert!(retained.same_instance(state_db));
        Self {
            _installation: installation.clone(),
            journal,
            state_db: retained,
        }
    }

    pub(super) fn journal(&self) -> &TransitionJournalStore {
        &self.journal
    }
}

/// Exact state-database evidence correlated with the decoded journal record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum DatabaseEvidence {
    AllocationNotObserved {
        previous: Option<ExistingStateEvidence>,
    },
    AllocationCommittedBehindJournal {
        state: state::Id,
        previous: Option<ExistingStateEvidence>,
    },
    CandidateOwnership {
        state: state::Id,
        ownership: db::state::TransitionOwnership,
        previous: Option<ExistingStateEvidence>,
    },
    ExistingCandidate {
        candidate: ExistingStateEvidence,
        previous: Option<ExistingStateEvidence>,
    },
    Conflict(DatabaseConflict),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExistingStateEvidence {
    state: state::Id,
    ownership: db::state::TransitionOwnership,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum DatabaseInspectionStability {
    Stable,
    Changed { after: DatabaseEvidence },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum DatabaseConflict {
    UnexpectedForExistingCandidate {
        state: state::Id,
        transition: TransitionId,
    },
    UnexpectedBeforeAllocationIntent {
        state: state::Id,
    },
    ForeignTransition {
        state: state::Id,
        transition: TransitionId,
    },
    CandidateStateMismatch {
        expected: state::Id,
        actual: state::Id,
    },
    InconsistentAuditOwnership {
        state: state::Id,
        audit_present: bool,
        ownership: db::state::TransitionOwnership,
    },
}

/// The fixed, bounded names which can be authenticated without inventing a
/// new recovery namespace API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum KnownTreeRole {
    Live,
    Staging,
    CandidateState(state::Id),
    PreviousState(state::Id),
    Quarantine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // retained for structured diagnostic classification
pub(super) enum DurableTreeRole {
    Candidate,
    Previous,
    Foreign,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // retained for structured diagnostic classification
pub(super) enum RuntimeTreeRole {
    Candidate,
    Previous,
    Foreign,
    NotComparable,
    Unavailable,
}

#[derive(Clone, Debug)]
pub(super) struct KnownTreeLocation {
    path: PathBuf,
    roles: Vec<KnownTreeRole>,
}

#[derive(Debug)]
#[allow(dead_code)] // every field preserves the immutable diagnostic snapshot
pub(super) struct RetainedTreeEvidence {
    location: KnownTreeLocation,
    store: TreeMarkerStore,
    marker: RetainedTreeMarker,
    runtime: Result<RuntimeTreeIdentity, RuntimeEvidenceError>,
}

impl RetainedTreeEvidence {
    #[allow(dead_code)] // consumed by fail-closed blocker classification
    fn durable_role(&self, record: &TransitionRecord) -> DurableTreeRole {
        if self.marker.token() == &record.candidate.tree_token {
            DurableTreeRole::Candidate
        } else if self.marker.token() == &record.previous.tree_token {
            DurableTreeRole::Previous
        } else {
            DurableTreeRole::Foreign
        }
    }

    #[allow(dead_code)] // consumed by fail-closed blocker classification
    fn runtime_role(&self, record: &TransitionRecord, epoch: &RuntimeEpochEvidence) -> RuntimeTreeRole {
        if epoch.comparability(record) != RuntimeEpochComparability::Current {
            return RuntimeTreeRole::NotComparable;
        }
        let Ok(runtime) = &self.runtime else {
            return RuntimeTreeRole::Unavailable;
        };
        if *runtime == record.candidate.usr_runtime_identity {
            RuntimeTreeRole::Candidate
        } else if *runtime == record.previous.usr_runtime_identity {
            RuntimeTreeRole::Previous
        } else {
            RuntimeTreeRole::Foreign
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // variants retain exact rejected/absent diagnostic evidence
pub(super) enum KnownTreeEvidence {
    Retained(RetainedTreeEvidence),
    Unresolved {
        location: KnownTreeLocation,
        retained: Option<RetainedTreeEvidence>,
        reason: UnresolvedTreeReason,
    },
}

#[derive(Debug)]
#[allow(dead_code)] // preserves typed reasons in the diagnostic snapshot
pub(super) enum UnresolvedTreeReason {
    Absent,
    Rejected(TreeMarkerError),
    StateSlotLinkUnauthenticated,
}

/// Runtime witnesses are comparable only when both authenticated epoch
/// captures agree with the journal's creation epoch.
#[derive(Debug)]
pub(super) struct RuntimeEpochEvidence {
    before: Result<RuntimeEpoch, RuntimeEvidenceError>,
    after: Result<RuntimeEpoch, RuntimeEvidenceError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RuntimeEpochComparability {
    Current,
    RecordedEpochChanged,
    ChangedDuringInspection,
    Unavailable,
}

impl RuntimeEpochEvidence {
    fn comparability(&self, record: &TransitionRecord) -> RuntimeEpochComparability {
        let (Ok(before), Ok(after)) = (&self.before, &self.after) else {
            return RuntimeEpochComparability::Unavailable;
        };
        if before != after {
            RuntimeEpochComparability::ChangedDuringInspection
        } else if before == &record.creation_epoch {
            RuntimeEpochComparability::Current
        } else {
            RuntimeEpochComparability::RecordedEpochChanged
        }
    }
}

/// Why this first read-only foundation still refuses to execute effects.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum RecoveryBlocker {
    DatabaseConflict,
    DatabaseChangedDuringInspection,
    RuntimeEpochUnavailable,
    RuntimeEpochChangedDuringInspection,
    RuntimeTreeEvidenceUnavailable,
    RuntimeTreeIdentityConflict,
    TreeEvidenceRejected,
    DurableTreeIdentityConflict,
    UnresolvedStateSlotLink,
    ExactNamespaceInventoryRequired,
    ManualBootRepair,
}

/// A pending transition owns the exact database capability and retained tree
/// evidence used by its read-only assessment. Inspection keeps the mutable
/// installation/global-lock and exclusive journal authority through the final
/// revalidation, then deliberately releases both before returning this
/// diagnostic. It therefore exposes and pins no recovery effects. A future
/// executor must run internally before the startup reservation is released and
/// reload the exact canonical journal generation immediately before each
/// conditional mutation.
#[derive(Debug)]
#[allow(dead_code)] // preserves the complete structured snapshot until the diagnostic is dropped
pub(super) struct PendingSystemTransition {
    state_db: db::state::Database,
    record: TransitionRecord,
    disposition: RecoveryDisposition,
    database: DatabaseEvidence,
    database_stability: DatabaseInspectionStability,
    epoch: RuntimeEpochEvidence,
    trees: Vec<KnownTreeEvidence>,
    blockers: Vec<RecoveryBlocker>,
}

impl PendingSystemTransition {
    pub(super) fn inspect(
        installation: &Installation,
        state_db: &db::state::Database,
        journal: TransitionJournalStore,
        record: TransitionRecord,
        in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<Self, InspectionError> {
        let authority = StartupRecoveryAuthority::new(installation, journal, state_db);
        let disposition = record.recovery_disposition();
        let database = inspect_database(&record, authority.state_db(), in_flight)?;

        installation.revalidate_mutable_namespace()?;
        let before = RuntimeEpoch::capture();
        let trees = inspect_known_trees(installation, &record);
        let after = RuntimeEpoch::capture();
        installation.revalidate_mutable_namespace()?;
        let epoch = RuntimeEpochEvidence { before, after };
        run_between_database_inspections();
        let in_flight_after = authority.state_db().audit_in_flight_transition()?;
        let database_after = inspect_database(&record, authority.state_db(), in_flight_after)?;
        installation.revalidate_mutable_namespace()?;
        let database_stability = if database == database_after {
            DatabaseInspectionStability::Stable
        } else {
            DatabaseInspectionStability::Changed { after: database_after }
        };

        let mut blockers = Vec::with_capacity(12);
        if !database_evidence_compatible(&record, &database) {
            blockers.push(RecoveryBlocker::DatabaseConflict);
        }
        if database_stability != DatabaseInspectionStability::Stable {
            blockers.push(RecoveryBlocker::DatabaseChangedDuringInspection);
        }
        match epoch.comparability(&record) {
            RuntimeEpochComparability::Unavailable => blockers.push(RecoveryBlocker::RuntimeEpochUnavailable),
            RuntimeEpochComparability::ChangedDuringInspection => {
                blockers.push(RecoveryBlocker::RuntimeEpochChangedDuringInspection);
            }
            RuntimeEpochComparability::Current | RuntimeEpochComparability::RecordedEpochChanged => {}
        }
        if trees.iter().any(|tree| {
            matches!(
                tree,
                KnownTreeEvidence::Unresolved {
                    reason: UnresolvedTreeReason::Rejected(_),
                    ..
                }
            )
        }) {
            blockers.push(RecoveryBlocker::TreeEvidenceRejected);
        }
        if trees.iter().any(|tree| {
            matches!(
                tree,
                KnownTreeEvidence::Unresolved {
                    reason: UnresolvedTreeReason::StateSlotLinkUnauthenticated,
                    ..
                }
            )
        }) {
            blockers.push(RecoveryBlocker::UnresolvedStateSlotLink);
        }
        assess_tree_roles(&record, &epoch, &trees, &mut blockers);
        // These fixed names do not prove absence of previous-slot parking or
        // copied markers elsewhere in the bounded activation namespace.
        blockers.push(RecoveryBlocker::ExactNamespaceInventoryRequired);
        if disposition == RecoveryDisposition::ManualBootRepair {
            blockers.push(RecoveryBlocker::ManualBootRepair);
        }
        blockers.sort_unstable();
        blockers.dedup();

        // Retain the exact database connection used for the snapshot, but
        // release the mutable installation/global-lock and exclusive journal
        // authority before returning a diagnostic.  The diagnostic exposes
        // no recovery effects, and keeping the journal here would permit a
        // coordinator -> journal / diagnostic -> coordinator ABBA deadlock.
        let state_db = authority.state_db().clone();
        debug_assert!(state_db.same_instance(authority.state_db()));
        drop(authority);

        Ok(Self {
            state_db,
            record,
            disposition,
            database,
            database_stability,
            epoch,
            trees,
            blockers,
        })
    }

    pub(super) fn transition_id(&self) -> &TransitionId {
        &self.record.transition_id
    }

    pub(super) fn phase(&self) -> Phase {
        self.record.phase
    }

    #[allow(dead_code)] // available to structured client diagnostics
    pub(super) fn disposition(&self) -> RecoveryDisposition {
        self.disposition
    }

    #[cfg(test)]
    pub(super) fn database_evidence(&self) -> &DatabaseEvidence {
        &self.database
    }

    #[cfg(test)]
    pub(super) fn database_stability(&self) -> &DatabaseInspectionStability {
        &self.database_stability
    }

    #[cfg(test)]
    pub(super) fn blockers(&self) -> &[RecoveryBlocker] {
        &self.blockers
    }

    #[cfg(test)]
    pub(super) fn retains_database(&self, database: &db::state::Database) -> bool {
        self.state_db.same_instance(database)
    }
}

impl fmt::Display for PendingSystemTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "state transition {} at {:?} requires {:?}; recovery effects remain blocked by {:?}",
            self.transition_id(),
            self.phase(),
            self.disposition,
            self.blockers,
        )
    }
}

impl std::error::Error for PendingSystemTransition {}

impl StartupRecoveryAuthority {
    fn state_db(&self) -> &db::state::Database {
        &self.state_db
    }
}

fn inspect_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
    in_flight: Option<db::state::InFlightTransition>,
) -> Result<DatabaseEvidence, InspectionError> {
    if record.operation != Operation::NewState {
        if let Some(row) = in_flight {
            return Ok(DatabaseEvidence::Conflict(
                DatabaseConflict::UnexpectedForExistingCandidate {
                    state: row.state_id,
                    transition: row.transition_id,
                },
            ));
        }
        let candidate = state::Id::from(
            record
                .candidate
                .id
                .expect("validated existing-candidate record has a state ID"),
        );
        let candidate = ExistingStateEvidence {
            state: candidate,
            ownership: state_db.transition_ownership(candidate, &record.transition_id)?,
        };
        let previous = inspect_previous_state(record, state_db, Some(candidate.state))?;
        return Ok(DatabaseEvidence::ExistingCandidate { candidate, previous });
    }

    let Some(candidate) = record.candidate.id.map(state::Id::from) else {
        let previous = inspect_previous_state(record, state_db, None)?;
        return Ok(match in_flight {
            None => DatabaseEvidence::AllocationNotObserved { previous },
            Some(row) if row.transition_id != record.transition_id => {
                DatabaseEvidence::Conflict(DatabaseConflict::ForeignTransition {
                    state: row.state_id,
                    transition: row.transition_id,
                })
            }
            Some(row) if record.phase != Phase::FreshStateAllocating => {
                DatabaseEvidence::Conflict(DatabaseConflict::UnexpectedBeforeAllocationIntent { state: row.state_id })
            }
            Some(row) => DatabaseEvidence::AllocationCommittedBehindJournal {
                state: row.state_id,
                previous,
            },
        });
    };

    if let Some(row) = in_flight.as_ref() {
        if row.transition_id != record.transition_id {
            return Ok(DatabaseEvidence::Conflict(DatabaseConflict::ForeignTransition {
                state: row.state_id,
                transition: row.transition_id.clone(),
            }));
        }
        if row.state_id != candidate {
            return Ok(DatabaseEvidence::Conflict(DatabaseConflict::CandidateStateMismatch {
                expected: candidate,
                actual: row.state_id,
            }));
        }
    }

    let audit_present = in_flight.is_some();
    let ownership = state_db.transition_ownership(candidate, &record.transition_id)?;
    let ownership_consistent = if audit_present {
        ownership == db::state::TransitionOwnership::Matching
    } else {
        matches!(
            ownership,
            db::state::TransitionOwnership::Cleared | db::state::TransitionOwnership::Missing
        )
    };
    if !ownership_consistent {
        return Ok(DatabaseEvidence::Conflict(
            DatabaseConflict::InconsistentAuditOwnership {
                state: candidate,
                audit_present,
                ownership,
            },
        ));
    }
    let previous = inspect_previous_state(record, state_db, Some(candidate))?;
    Ok(DatabaseEvidence::CandidateOwnership {
        state: candidate,
        ownership,
        previous,
    })
}

fn inspect_previous_state(
    record: &TransitionRecord,
    state_db: &db::state::Database,
    candidate: Option<state::Id>,
) -> Result<Option<ExistingStateEvidence>, InspectionError> {
    record
        .previous
        .id
        .map(state::Id::from)
        .filter(|previous| Some(*previous) != candidate)
        .map(|previous| {
            state_db
                .transition_ownership(previous, &record.transition_id)
                .map(|ownership| ExistingStateEvidence {
                    state: previous,
                    ownership,
                })
                .map_err(InspectionError::from)
        })
        .transpose()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FreshDatabaseExpectation {
    Matching,
    MatchingOrCleared,
    Cleared,
    MatchingOrMissing,
    Missing,
}

fn database_evidence_compatible(record: &TransitionRecord, evidence: &DatabaseEvidence) -> bool {
    match evidence {
        DatabaseEvidence::AllocationNotObserved { previous } => {
            previous_state_compatible(previous)
                && record.candidate.id.is_none()
                && (matches!(record.phase, Phase::Preparing | Phase::FreshStateAllocating)
                    || record.rollback.as_ref().is_some_and(|rollback| {
                        matches!(
                            rollback.fresh_db,
                            crate::transition_journal::RollbackAction::NotRequired
                                | crate::transition_journal::RollbackAction::AlreadySatisfied
                        )
                    }))
        }
        DatabaseEvidence::AllocationCommittedBehindJournal { previous, .. } => {
            previous_state_compatible(previous) && record.phase == Phase::FreshStateAllocating
        }
        DatabaseEvidence::CandidateOwnership {
            ownership, previous, ..
        } => {
            previous_state_compatible(previous)
                && match fresh_database_expectation(record) {
                    FreshDatabaseExpectation::Matching => *ownership == db::state::TransitionOwnership::Matching,
                    FreshDatabaseExpectation::MatchingOrCleared => matches!(
                        ownership,
                        db::state::TransitionOwnership::Matching | db::state::TransitionOwnership::Cleared
                    ),
                    FreshDatabaseExpectation::Cleared => *ownership == db::state::TransitionOwnership::Cleared,
                    FreshDatabaseExpectation::MatchingOrMissing => matches!(
                        ownership,
                        db::state::TransitionOwnership::Matching | db::state::TransitionOwnership::Missing
                    ),
                    FreshDatabaseExpectation::Missing => *ownership == db::state::TransitionOwnership::Missing,
                }
        }
        DatabaseEvidence::ExistingCandidate { candidate, previous } => {
            candidate.ownership == db::state::TransitionOwnership::Cleared
                && previous
                    .as_ref()
                    .is_none_or(|previous| previous.ownership == db::state::TransitionOwnership::Cleared)
        }
        DatabaseEvidence::Conflict(_) => false,
    }
}

fn previous_state_compatible(previous: &Option<ExistingStateEvidence>) -> bool {
    previous
        .as_ref()
        .is_none_or(|previous| previous.ownership == db::state::TransitionOwnership::Cleared)
}

fn fresh_database_expectation(record: &TransitionRecord) -> FreshDatabaseExpectation {
    if record.rollback.is_none() {
        return match record.phase {
            Phase::CommitDecided => FreshDatabaseExpectation::MatchingOrCleared,
            Phase::CommitCleanupComplete | Phase::Complete => FreshDatabaseExpectation::Cleared,
            _ => FreshDatabaseExpectation::Matching,
        };
    }

    let rollback = record.rollback.as_ref().expect("checked rollback record");
    match rollback.fresh_db {
        crate::transition_journal::RollbackAction::Pending if record.phase == Phase::FreshDbInvalidationIntent => {
            FreshDatabaseExpectation::MatchingOrMissing
        }
        crate::transition_journal::RollbackAction::Pending => FreshDatabaseExpectation::Matching,
        crate::transition_journal::RollbackAction::Applied
        | crate::transition_journal::RollbackAction::AlreadySatisfied => FreshDatabaseExpectation::Missing,
        crate::transition_journal::RollbackAction::NotRequired => FreshDatabaseExpectation::Matching,
    }
}

fn inspect_known_trees(installation: &Installation, record: &TransitionRecord) -> Vec<KnownTreeEvidence> {
    let mut locations = known_tree_locations(installation, record);
    debug_assert!(locations.len() <= MAX_KNOWN_TREE_LOCATIONS);
    locations.drain(..).map(inspect_known_tree).collect()
}

fn inspect_known_tree(location: KnownTreeLocation) -> KnownTreeEvidence {
    let store = match TreeMarkerStore::open_path(location.path.clone()) {
        Err(TreeMarkerError::Io { source, .. }) if source.raw_os_error() == Some(nix::libc::ENOENT) => {
            return unresolved_tree(location, None, UnresolvedTreeReason::Absent);
        }
        Err(source) => return unresolved_tree(location, None, UnresolvedTreeReason::Rejected(source)),
        Ok(store) => store,
    };
    let marker = match store.read_for_transition_recovery() {
        Ok(marker) => marker,
        Err(source) => return unresolved_tree(location, None, UnresolvedTreeReason::Rejected(source)),
    };
    let runtime = RuntimeTreeIdentity::capture_directory(store.retained_directory());
    let reopened = match TreeMarkerStore::open_path(location.path.clone()) {
        Ok(reopened) => reopened,
        Err(source) => {
            let retained = RetainedTreeEvidence {
                location: location.clone(),
                store,
                marker,
                runtime,
            };
            return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
        }
    };
    if let Err(source) = store.require_same_directory(&reopened) {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store,
            marker,
            runtime,
        };
        return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
    }
    let runtime = reconcile_runtime(
        runtime,
        RuntimeTreeIdentity::capture_directory(reopened.retained_directory()),
    );
    if marker.needs_slot_link_authorization() {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store: reopened,
            marker,
            runtime,
        };
        return unresolved_tree(
            location,
            Some(retained),
            UnresolvedTreeReason::StateSlotLinkUnauthenticated,
        );
    }

    let named_marker = match marker.read_named_for_transition(&reopened) {
        Ok(named_marker) => named_marker,
        Err(source) => {
            let retained = RetainedTreeEvidence {
                location: location.clone(),
                store,
                marker,
                runtime,
            };
            return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
        }
    };
    if let Err(source) = marker.require_same_marker(&named_marker) {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store,
            marker,
            runtime,
        };
        return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
    }
    if let Err(source) = named_marker.revalidate(&reopened) {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store,
            marker,
            runtime,
        };
        return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
    }

    run_before_final_tree_reopen();
    let final_store = match TreeMarkerStore::open_path(location.path.clone()) {
        Ok(final_store) => final_store,
        Err(source) => {
            let retained = RetainedTreeEvidence {
                location: location.clone(),
                store: reopened,
                marker: named_marker,
                runtime,
            };
            return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
        }
    };
    if let Err(source) = reopened.require_same_directory(&final_store) {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store: reopened,
            marker: named_marker,
            runtime,
        };
        return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
    }
    let runtime = reconcile_runtime(
        runtime,
        RuntimeTreeIdentity::capture_directory(final_store.retained_directory()),
    );
    let final_marker = match named_marker.read_named_for_transition(&final_store) {
        Ok(final_marker) => final_marker,
        Err(source) => {
            let retained = RetainedTreeEvidence {
                location: location.clone(),
                store: reopened,
                marker: named_marker,
                runtime,
            };
            return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
        }
    };
    if let Err(source) = named_marker.require_same_marker(&final_marker) {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store: reopened,
            marker: named_marker,
            runtime,
        };
        return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
    }
    if let Err(source) = final_marker.revalidate(&final_store) {
        let retained = RetainedTreeEvidence {
            location: location.clone(),
            store: reopened,
            marker: named_marker,
            runtime,
        };
        return unresolved_tree(location, Some(retained), UnresolvedTreeReason::Rejected(source));
    }
    KnownTreeEvidence::Retained(RetainedTreeEvidence {
        location,
        store: final_store,
        marker: final_marker,
        runtime,
    })
}

fn reconcile_runtime(
    before: Result<RuntimeTreeIdentity, RuntimeEvidenceError>,
    after: Result<RuntimeTreeIdentity, RuntimeEvidenceError>,
) -> Result<RuntimeTreeIdentity, RuntimeEvidenceError> {
    match (before, after) {
        (Ok(before), Ok(after)) if before == after => Ok(after),
        (Ok(_), Ok(_)) => Err(RuntimeEvidenceError::TreeChanged),
        (Err(source), _) | (_, Err(source)) => Err(source),
    }
}

fn unresolved_tree(
    location: KnownTreeLocation,
    retained: Option<RetainedTreeEvidence>,
    reason: UnresolvedTreeReason,
) -> KnownTreeEvidence {
    KnownTreeEvidence::Unresolved {
        location,
        retained,
        reason,
    }
}

fn assess_tree_roles(
    record: &TransitionRecord,
    epoch: &RuntimeEpochEvidence,
    trees: &[KnownTreeEvidence],
    blockers: &mut Vec<RecoveryBlocker>,
) {
    let mut candidate_count = 0usize;
    let mut previous_count = 0usize;
    for retained in trees.iter().filter_map(retained_tree) {
        let durable = retained.durable_role(record);
        match durable {
            DurableTreeRole::Candidate => candidate_count += 1,
            DurableTreeRole::Previous => previous_count += 1,
            DurableTreeRole::Foreign => blockers.push(RecoveryBlocker::DurableTreeIdentityConflict),
        }
        if epoch.comparability(record) != RuntimeEpochComparability::Current {
            continue;
        }
        match (durable, retained.runtime_role(record, epoch)) {
            (_, RuntimeTreeRole::Unavailable) => blockers.push(RecoveryBlocker::RuntimeTreeEvidenceUnavailable),
            (DurableTreeRole::Candidate, RuntimeTreeRole::Candidate)
            | (DurableTreeRole::Previous, RuntimeTreeRole::Previous) => {}
            (_, RuntimeTreeRole::NotComparable) => {
                blockers.push(RecoveryBlocker::RuntimeEpochChangedDuringInspection);
            }
            _ => blockers.push(RecoveryBlocker::RuntimeTreeIdentityConflict),
        }
    }
    if candidate_count > 1 || previous_count > 1 {
        blockers.push(RecoveryBlocker::DurableTreeIdentityConflict);
    }
}

fn retained_tree(tree: &KnownTreeEvidence) -> Option<&RetainedTreeEvidence> {
    match tree {
        KnownTreeEvidence::Retained(retained) => Some(retained),
        KnownTreeEvidence::Unresolved { retained, .. } => retained.as_ref(),
    }
}

fn known_tree_locations(installation: &Installation, record: &TransitionRecord) -> Vec<KnownTreeLocation> {
    let mut locations = Vec::with_capacity(MAX_KNOWN_TREE_LOCATIONS);
    add_location(&mut locations, installation.root.join("usr"), KnownTreeRole::Live);
    add_location(&mut locations, installation.staging_path("usr"), KnownTreeRole::Staging);
    if let Some(candidate) = record.candidate.id.map(state::Id::from) {
        add_location(
            &mut locations,
            installation.root_path(i32::from(candidate).to_string()).join("usr"),
            KnownTreeRole::CandidateState(candidate),
        );
    }
    if let Some(previous) = record.previous.id.map(state::Id::from) {
        add_location(
            &mut locations,
            installation.root_path(i32::from(previous).to_string()).join("usr"),
            KnownTreeRole::PreviousState(previous),
        );
    }
    add_location(
        &mut locations,
        installation
            .state_quarantine_dir()
            .join(record.quarantine_name.as_str())
            .join("usr"),
        KnownTreeRole::Quarantine,
    );
    locations
}

fn add_location(locations: &mut Vec<KnownTreeLocation>, path: PathBuf, role: KnownTreeRole) {
    if let Some(existing) = locations.iter_mut().find(|existing| existing.path == path) {
        existing.roles.push(role);
    } else {
        locations.push(KnownTreeLocation {
            path,
            roles: vec![role],
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum InspectionError {
    #[error("inspect exact state-transition database ownership")]
    Database(#[from] db::state::TransitionEvidenceError),
    #[error("revalidate retained mutable installation namespace around recovery evidence inspection")]
    Installation(#[from] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_DATABASE_INSPECTIONS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_FINAL_TREE_REOPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_between_database_inspections(hook: impl FnOnce() + 'static) {
    BETWEEN_DATABASE_INSPECTIONS.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_between_database_inspections() {
    BETWEEN_DATABASE_INSPECTIONS.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_between_database_inspections() {}

#[cfg(test)]
fn arm_before_final_tree_reopen(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_TREE_REOPEN.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_final_tree_reopen() {
    BEFORE_FINAL_TREE_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_final_tree_reopen() {}

#[cfg(test)]
mod tests;
