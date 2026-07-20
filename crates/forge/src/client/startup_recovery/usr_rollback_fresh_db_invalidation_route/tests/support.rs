use std::path::Path;

use crate::{
    Installation, db,
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackFreshDbInvalidationRouteSeal,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationRouteAdmission, UsrRollbackFreshDbInvalidationRouteAuthority,
            UsrRollbackFreshDbInvalidationRouteAuthorityError,
        },
    },
    installation::DatabaseKind,
    test_support::private_installation_tempdir,
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

const OS_RELEASE: &[u8] = b"NAME=Rollback Decision Test\nID=rollback-decision-test\n";
const SYSTEM_MODEL: &[u8] = b"let system = { hostname = \"rollback-decision-test\" } in system\n";

use super::super::{
    candidate_test_support::{CandidateLayout, CandidatePreserveFixture},
    test_fixture::{DatabaseSnapshot, NamespaceEntry, OperationKind},
};

pub(super) use super::super::candidate_test_support::{CandidateSource, transition_quarantine_path};
pub(super) use super::super::test_fixture::{canonical_journal, create_private_directory};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateOutcome {
    Applied,
    AlreadySatisfied,
}

impl CandidateOutcome {
    pub(super) const ALL: [Self; 2] = [Self::Applied, Self::AlreadySatisfied];

    pub(super) fn journal_outcome(self) -> RollbackActionOutcome {
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
    pub(super) fn at_epoch(
        historical: bool,
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        if historical {
            Self::historical(source, usr_outcome, candidate_outcome)
        } else {
            Self::new(source, usr_outcome, candidate_outcome)
        }
    }

    pub(super) fn new(
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        Self::from_candidate_fixture(
            CandidatePreserveFixture::new(OperationKind::NewState, source, usr_outcome, CandidateLayout::Preserved),
            candidate_outcome,
        )
    }

    pub(super) fn historical(
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        Self::from_candidate_fixture(
            CandidatePreserveFixture::historical(
                OperationKind::NewState,
                source,
                usr_outcome,
                CandidateLayout::Preserved,
            ),
            candidate_outcome,
        )
    }

    fn from_candidate_fixture(fixture: CandidatePreserveFixture, candidate_outcome: CandidateOutcome) -> Self {
        let source = fixture
            .candidate_intent
            .rollback_successor(Some(candidate_outcome.journal_outcome()))
            .expect("preserved candidate fixture must admit CandidatePreserved");
        assert_eq!(source.phase, Phase::CandidatePreserved);
        let journal = fixture.open_journal();
        journal.advance(&fixture.candidate_intent, &source).unwrap();
        drop(journal);
        assert_eq!(fixture.fixture.canonical_record(), source);
        Self { fixture, source }
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
        assert_eq!(successor.phase, Phase::FreshDbInvalidationIntent);
        successor
    }

    pub(super) fn capture<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> Result<
        UsrRollbackFreshDbInvalidationRouteAdmission<'reservation>,
        UsrRollbackFreshDbInvalidationRouteAuthorityError,
    > {
        capture_record(&self.fixture, journal, reservation, &self.source)
    }

    pub(super) fn capture_ready<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackFreshDbInvalidationRouteAuthority<'reservation> {
        match self.capture(journal, reservation).unwrap() {
            UsrRollbackFreshDbInvalidationRouteAdmission::Ready(authority) => authority,
            _ => panic!("exact CandidatePreserved evidence did not admit the route"),
        }
    }

    pub(super) fn install_persistent_database(&mut self) {
        let database = open_state_database(&self.fixture.fixture.installation);
        let previous = database.add(&[], Some("rollback previous"), None).unwrap().id;
        let candidate = database
            .add_with_transition(
                &self.source.transition_id,
                &[],
                Some("rollback fresh candidate"),
                None,
            )
            .unwrap()
            .id;
        assert_eq!(previous, self.fixture.fixture.previous_state);
        assert_eq!(candidate, self.fixture.fixture.candidate_state);
        let provenance = db::state::MetadataProvenance::from_outputs(OS_RELEASE, SYSTEM_MODEL);
        database
            .insert_fresh_metadata_provenance_if_transition_matches(
                candidate,
                &self.source.transition_id,
                &provenance,
            )
            .unwrap();
        let old = std::mem::replace(&mut self.fixture.fixture.database, database);
        drop(old);
    }

    pub(super) fn release_handles(mut self) -> tempfile::TempDir {
        let retained = std::mem::replace(
            &mut self.fixture.fixture._temporary,
            private_installation_tempdir(),
        );
        drop(self);
        retained
    }
}

pub(super) struct FreshRouteHandles {
    pub(super) installation: Installation,
    pub(super) database: db::state::Database,
    pub(super) journal: TransitionJournalStore,
    pub(super) record: TransitionRecord,
}

impl FreshRouteHandles {
    pub(super) fn open(root: &Path) -> Self {
        let installation = Installation::open(root, None).unwrap();
        let database = open_state_database(&installation);
        let journal = TransitionJournalStore::open_retained(installation.root_directory(), root).unwrap();
        let record = journal
            .load()
            .unwrap()
            .expect("fresh-handle reopen requires a durable route record");
        Self {
            installation,
            database,
            journal,
            record,
        }
    }

    pub(super) fn capture<'reservation>(
        &self,
        reservation: &'reservation ActiveStateReservation,
    ) -> Result<
        UsrRollbackFreshDbInvalidationRouteAdmission<'reservation>,
        UsrRollbackFreshDbInvalidationRouteAuthorityError,
    > {
        capture_parts(
            &self.installation,
            &self.database,
            &self.journal,
            reservation,
            &self.record,
        )
    }

    pub(super) fn capture_ready<'reservation>(
        &self,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackFreshDbInvalidationRouteAuthority<'reservation> {
        match self.capture(reservation).unwrap() {
            UsrRollbackFreshDbInvalidationRouteAdmission::Ready(authority) => authority,
            _ => panic!("fresh exact CandidatePreserved handles did not admit the route"),
        }
    }
}

pub(super) fn capture_record<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> Result<UsrRollbackFreshDbInvalidationRouteAdmission<'reservation>, UsrRollbackFreshDbInvalidationRouteAuthorityError>
{
    capture_parts(
        &fixture.fixture.installation,
        &fixture.fixture.database,
        journal,
        reservation,
        record,
    )
}

fn capture_parts<'reservation>(
    installation: &Installation,
    database: &db::state::Database,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> Result<UsrRollbackFreshDbInvalidationRouteAdmission<'reservation>, UsrRollbackFreshDbInvalidationRouteAuthorityError>
{
    let seal = UsrRollbackFreshDbInvalidationRouteSeal::new_for_test();
    let initial_in_flight = database.audit_in_flight_transition().unwrap();
    UsrRollbackFreshDbInvalidationRouteAuthority::capture(
        &seal,
        installation,
        journal,
        database,
        reservation,
        record,
        initial_in_flight,
    )
}

fn open_state_database(installation: &Installation) -> db::state::Database {
    let location = installation.mutable_database_location(DatabaseKind::State).unwrap();
    let (url, anchor) = location.parts();
    let database = db::state::Database::new_anchored(url, anchor).unwrap();
    location.revalidate().unwrap();
    installation.revalidate_mutable_namespace().unwrap();
    database
}
