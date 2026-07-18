use std::{fs, os::unix::fs::MetadataExt as _, path::PathBuf};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{self, UsrRollbackActivateArchivedCompleteRouteSeal},
        startup_reconciliation::{
            UsrRollbackActivateArchivedCompleteRouteAdmission, UsrRollbackActivateArchivedCompleteRouteAuthority,
            UsrRollbackActivateArchivedCompleteRouteAuthorityError,
        },
    },
    db,
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord, decode},
};

pub(super) use super::super::candidate_test_support::CandidateSource;
use super::super::{
    candidate_test_support::{CandidateLayout, CandidatePreserveFixture, archived_slot_path},
    test_fixture::{DatabaseSnapshot, NamespaceEntry, OperationKind},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Epoch {
    Current,
    Historical,
}

impl Epoch {
    pub(super) const ALL: [Self; 2] = [Self::Current, Self::Historical];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateOutcome {
    Applied,
    AlreadySatisfied,
}

impl CandidateOutcome {
    pub(super) const ALL: [Self; 2] = [Self::Applied, Self::AlreadySatisfied];

    pub(super) fn outcome(self) -> RollbackActionOutcome {
        match self {
            Self::Applied => RollbackActionOutcome::Applied,
            Self::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        }
    }
}

pub(super) struct RouteFixture {
    pub(super) fixture: CandidatePreserveFixture,
    pub(super) source: TransitionRecord,
}

impl RouteFixture {
    pub(super) fn new(
        epoch: Epoch,
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        let fixture = match epoch {
            Epoch::Current => {
                CandidatePreserveFixture::new(OperationKind::Archived, source, usr_outcome, CandidateLayout::Preserved)
            }
            Epoch::Historical => CandidatePreserveFixture::historical(
                OperationKind::Archived,
                source,
                usr_outcome,
                CandidateLayout::Preserved,
            ),
        };
        let source = fixture
            .candidate_intent
            .rollback_successor(Some(candidate_outcome.outcome()))
            .unwrap();
        assert_eq!(source.phase, Phase::CandidatePreserved);
        let journal = fixture.open_journal();
        journal.advance(&fixture.candidate_intent, &source).unwrap();
        drop(journal);
        assert_eq!(fixture.fixture.canonical_record(), source);
        let route = Self { fixture, source };
        route.assert_exact_database_pair();
        route.assert_exact_archived_topology();
        route
    }

    pub(super) fn open_journal(&self) -> TransitionJournalStore {
        self.fixture.open_journal()
    }

    pub(super) fn canonical_record(&self) -> TransitionRecord {
        self.fixture.fixture.canonical_record()
    }

    pub(super) fn canonical_bytes(&self) -> Vec<u8> {
        self.fixture.fixture.canonical_bytes()
    }

    pub(super) fn database_snapshot(&self) -> DatabaseSnapshot {
        self.fixture.fixture.database_snapshot()
    }

    pub(super) fn namespace_snapshot(&self) -> Vec<NamespaceEntry> {
        self.fixture.fixture.namespace_snapshot()
    }

    pub(super) fn expected_successor(&self) -> TransitionRecord {
        let successor = self.source.rollback_successor(None).unwrap();
        assert_eq!(successor.phase, Phase::RollbackComplete);
        successor
    }

    pub(super) fn capture<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> Result<
        UsrRollbackActivateArchivedCompleteRouteAdmission<'reservation>,
        UsrRollbackActivateArchivedCompleteRouteAuthorityError,
    > {
        capture_record(self, journal, reservation, &self.source)
    }

    pub(super) fn capture_ready<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackActivateArchivedCompleteRouteAuthority<'reservation> {
        match self.capture(journal, reservation).unwrap() {
            UsrRollbackActivateArchivedCompleteRouteAdmission::Ready(authority) => authority,
            _ => panic!("exact preserved ActivateArchived evidence did not admit completion routing"),
        }
    }

    pub(super) fn archived_wrapper_path(&self) -> PathBuf {
        self.fixture
            .fixture
            .installation
            .root
            .join(".cast/root")
            .join(self.fixture.fixture.candidate_state.to_string())
    }

    pub(super) fn archived_slot_path(&self) -> PathBuf {
        archived_slot_path(&self.fixture.fixture, &self.source)
    }

    pub(super) fn transition_quarantine_path(&self) -> PathBuf {
        self.fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join(self.source.quarantine_name.as_str())
    }

    pub(super) fn assert_exact_database_pair(&self) {
        let database = &self.fixture.fixture.database;
        let candidate = self.fixture.fixture.candidate_state;
        let previous = self.fixture.fixture.previous_state;
        assert_ne!(candidate, previous);
        assert_eq!(database.all().unwrap().len(), 2);
        assert_eq!(database.get(candidate).unwrap().id, candidate);
        assert_eq!(database.get(previous).unwrap().id, previous);
        assert_eq!(database.audit_in_flight_transition().unwrap(), None);
        assert_eq!(
            database
                .transition_ownership(candidate, &self.source.transition_id)
                .unwrap(),
            db::state::TransitionOwnership::Cleared
        );
        assert_eq!(
            database
                .transition_ownership(previous, &self.source.transition_id)
                .unwrap(),
            db::state::TransitionOwnership::Cleared
        );
        assert!(database.metadata_provenance(candidate).unwrap().is_some());
    }

    pub(super) fn assert_exact_archived_topology(&self) {
        let wrapper = self.archived_wrapper_path();
        let slot = self.archived_slot_path();
        let root = &self.fixture.fixture.installation.root;
        assert!(wrapper.join("usr").is_dir());
        assert!(slot.is_file());
        assert_eq!(fs::symlink_metadata(slot).unwrap().nlink(), 2);
        assert!(
            fs::read_dir(self.fixture.fixture.installation.staging_dir())
                .unwrap()
                .next()
                .is_none()
        );
        assert!(!self.transition_quarantine_path().exists());
        assert_eq!(
            fs::read_to_string(root.join("usr/.stateID")).unwrap(),
            self.fixture.fixture.previous_state.to_string()
        );
    }
}

pub(super) fn capture_record<'reservation>(
    fixture: &RouteFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> Result<
    UsrRollbackActivateArchivedCompleteRouteAdmission<'reservation>,
    UsrRollbackActivateArchivedCompleteRouteAuthorityError,
> {
    let seal = UsrRollbackActivateArchivedCompleteRouteSeal::new_for_test();
    UsrRollbackActivateArchivedCompleteRouteAuthority::capture(
        &seal,
        &fixture.fixture.fixture.installation,
        journal,
        &fixture.fixture.fixture.database,
        reservation,
        record,
    )
}

pub(super) fn assert_pending_phase(error: &startup_gate::Error, expected: Phase) {
    match error {
        startup_gate::Error::RecoveryPending(pending) => assert_eq!(pending.phase(), expected),
        other => panic!("expected {expected:?} recovery-pending result, got {other:?}"),
    }
}

pub(super) fn canonical_record_from_root(root: &std::path::Path) -> TransitionRecord {
    decode(&fs::read(root.join(".cast/journal/state-transition")).unwrap()).unwrap()
}
