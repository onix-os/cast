//! Stable database and active-selection authority for replacement repair.

use crate::{
    Installation, db, state,
    transition_journal::{TransitionJournalStore, TransitionRecord},
};

use super::super::active_state_snapshot::{ActiveStateReservation, ActiveStateSnapshot};
use super::super::startup_gate::ActiveReblitReplacementMutationSeal;
use super::{
    DatabaseEvidence, InspectionError, database_ownership_evidence_compatible, inspect_database,
    metadata_provenance_evidence_compatible,
};

/// Lazy, unforgeable provider retained by the production normalizer. Merely
/// constructing it observes and mutates nothing; restrictive residue is the
/// only path which consumes the startup gate's initial database observation.
pub(crate) struct ActiveReblitReplacementMutationAuthorityProvider<'authority> {
    installation: &'authority Installation,
    journal: &'authority TransitionJournalStore,
    state_db: &'authority db::state::Database,
    active_state_reservation: &'authority ActiveStateReservation,
    record: &'authority TransitionRecord,
    initial_in_flight: Option<Option<db::state::InFlightTransition>>,
}

impl<'authority> ActiveReblitReplacementMutationAuthorityProvider<'authority> {
    pub(in crate::client) fn new(
        _startup_gate_seal: &ActiveReblitReplacementMutationSeal,
        installation: &'authority Installation,
        journal: &'authority TransitionJournalStore,
        state_db: &'authority db::state::Database,
        active_state_reservation: &'authority ActiveStateReservation,
        record: &'authority TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Self {
        Self::from_context(
            installation,
            journal,
            state_db,
            active_state_reservation,
            record,
            initial_in_flight,
        )
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test(
        installation: &'authority Installation,
        journal: &'authority TransitionJournalStore,
        state_db: &'authority db::state::Database,
        active_state_reservation: &'authority ActiveStateReservation,
        record: &'authority TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Self {
        Self::from_context(
            installation,
            journal,
            state_db,
            active_state_reservation,
            record,
            initial_in_flight,
        )
    }

    fn from_context(
        installation: &'authority Installation,
        journal: &'authority TransitionJournalStore,
        state_db: &'authority db::state::Database,
        active_state_reservation: &'authority ActiveStateReservation,
        record: &'authority TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Self {
        Self {
            installation,
            journal,
            state_db,
            active_state_reservation,
            record,
            initial_in_flight: Some(initial_in_flight),
        }
    }

    pub(crate) fn recovery_context(
        &self,
    ) -> (
        &'authority Installation,
        &'authority TransitionJournalStore,
        &'authority TransitionRecord,
    ) {
        (self.installation, self.journal, self.record)
    }

    #[cfg(test)]
    pub(crate) fn require_exact_context(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        record: &TransitionRecord,
    ) -> Result<(), StartupMutationAuthorityError> {
        let installation_matches = std::ptr::eq(self.installation, installation);
        let journal_matches = std::ptr::eq(self.journal, journal);
        let record_matches = std::ptr::eq(self.record, record);
        if installation_matches && journal_matches && record_matches {
            Ok(())
        } else {
            Err(StartupMutationAuthorityErrorKind::ContextMismatch {
                installation_matches,
                journal_matches,
                record_matches,
            }
            .into())
        }
    }

    pub(crate) fn prepare(
        &mut self,
    ) -> Result<ActiveReblitReplacementMutationAuthority<'authority>, StartupMutationAuthorityError> {
        let initial_in_flight = self
            .initial_in_flight
            .take()
            .expect("startup replacement mutation authority is prepared only once");
        ActiveReblitReplacementMutationAuthority::capture(
            self.installation,
            self.state_db,
            self.active_state_reservation,
            self.record,
            initial_in_flight,
        )
    }
}

/// Exact non-mutating database and live-selection evidence retained across
/// the narrowly-scoped CandidatePrepared replacement-mode repair.
pub(crate) struct ActiveReblitReplacementMutationAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    active_state: ActiveStateSnapshot,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> ActiveReblitReplacementMutationAuthority<'reservation> {
    fn capture(
        installation: &Installation,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<Self, StartupMutationAuthorityError> {
        installation.revalidate_mutable_namespace()?;
        let database = compatible_database_evidence(record, state_db, initial_in_flight)?;
        let active_state = active_state_reservation
            .capture_for_startup_recovery(installation)
            .map_err(|source| StartupMutationAuthorityErrorKind::ActiveState {
                source: Box::new(source),
            })
            .map_err(StartupMutationAuthorityError::from)?;
        require_expected_active_state(record, &active_state)?;
        let database_after = exact_compatible_database_evidence(record, state_db)?;
        require_exact_database_evidence(&database, database_after)?;
        active_state
            .revalidate(installation)
            .map_err(|source| StartupMutationAuthorityErrorKind::ActiveState {
                source: Box::new(source),
            })
            .map_err(StartupMutationAuthorityError::from)?;
        installation.revalidate_mutable_namespace()?;
        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        Ok(Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database,
            active_state,
            _active_state_reservation: active_state_reservation,
        })
    }

    /// Revalidate a database-active-database sandwich against the originally
    /// captured evidence before, after, and at final mutation evidence.
    pub(crate) fn revalidate(&self) -> Result<(), StartupMutationAuthorityError> {
        self.installation.revalidate_mutable_namespace()?;
        let database_before = exact_compatible_database_evidence(&self.record, &self.state_db)?;
        require_exact_database_evidence(&self.database, database_before)?;
        require_expected_active_state(&self.record, &self.active_state)?;
        self.active_state
            .revalidate(&self.installation)
            .map_err(|source| StartupMutationAuthorityErrorKind::ActiveState {
                source: Box::new(source),
            })
            .map_err(StartupMutationAuthorityError::from)?;
        let database_after = exact_compatible_database_evidence(&self.record, &self.state_db)?;
        require_exact_database_evidence(&self.database, database_after)?;
        self.active_state
            .revalidate(&self.installation)
            .map_err(|source| StartupMutationAuthorityErrorKind::ActiveState {
                source: Box::new(source),
            })
            .map_err(StartupMutationAuthorityError::from)?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn exact_compatible_database_evidence(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseEvidence, StartupMutationAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    compatible_database_evidence(record, state_db, in_flight)
}

fn compatible_database_evidence(
    record: &TransitionRecord,
    state_db: &db::state::Database,
    in_flight: Option<db::state::InFlightTransition>,
) -> Result<DatabaseEvidence, StartupMutationAuthorityError> {
    let evidence = inspect_database(record, state_db, in_flight)?;
    if database_ownership_evidence_compatible(record, &evidence)
        && metadata_provenance_evidence_compatible(record, &evidence)
    {
        Ok(evidence)
    } else {
        Err(StartupMutationAuthorityErrorKind::DatabaseIncompatible {
            evidence: Box::new(evidence),
        }
        .into())
    }
}

fn require_exact_database_evidence(
    expected: &DatabaseEvidence,
    actual: DatabaseEvidence,
) -> Result<(), StartupMutationAuthorityError> {
    if *expected == actual {
        Ok(())
    } else {
        Err(StartupMutationAuthorityErrorKind::DatabaseChanged {
            expected: Box::new(expected.clone()),
            actual: Box::new(actual),
        }
        .into())
    }
}

fn require_expected_active_state(
    record: &TransitionRecord,
    active_state: &ActiveStateSnapshot,
) -> Result<(), StartupMutationAuthorityError> {
    let expected = record
        .previous
        .id
        .map(state::Id::from)
        .expect("validated active-reblit recovery record has a previous state ID");
    let actual = active_state.active();
    if actual == Some(expected) {
        Ok(())
    } else {
        Err(StartupMutationAuthorityErrorKind::ActiveSelectionMismatch { expected, actual }.into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(crate) struct StartupMutationAuthorityError(#[from] StartupMutationAuthorityErrorKind);

impl From<InspectionError> for StartupMutationAuthorityError {
    fn from(source: InspectionError) -> Self {
        StartupMutationAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<crate::installation::Error> for StartupMutationAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        StartupMutationAuthorityErrorKind::Installation(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum StartupMutationAuthorityErrorKind {
    #[cfg(test)]
    #[error(
        "startup replacement mutation context mismatch (installation={installation_matches}, journal={journal_matches}, record={record_matches})"
    )]
    ContextMismatch {
        installation_matches: bool,
        journal_matches: bool,
        record_matches: bool,
    },
    #[error("inspect stable startup mutation evidence")]
    Inspection(#[source] InspectionError),
    #[error("revalidate retained mutable installation namespace around startup mutation authority")]
    Installation(#[source] crate::installation::Error),
    #[error("startup mutation database evidence is incompatible with the persisted transition phase: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("startup mutation database evidence changed from {expected:?} to {actual:?}")]
    DatabaseChanged {
        expected: Box<DatabaseEvidence>,
        actual: Box<DatabaseEvidence>,
    },
    #[error("prove exact live active-state selection for startup mutation")]
    ActiveState {
        #[source]
        source: Box<super::super::Error>,
    },
    #[error("startup mutation requires active state {expected}, found {actual:?}")]
    ActiveSelectionMismatch {
        expected: state::Id,
        actual: Option<state::Id>,
    },
}
