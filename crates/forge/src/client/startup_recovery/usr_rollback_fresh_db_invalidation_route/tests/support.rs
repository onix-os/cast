use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackFreshDbInvalidationRouteSeal,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationRouteAdmission, UsrRollbackFreshDbInvalidationRouteAuthority,
            UsrRollbackFreshDbInvalidationRouteAuthorityError,
        },
    },
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

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
}

pub(super) fn capture_record<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> Result<UsrRollbackFreshDbInvalidationRouteAdmission<'reservation>, UsrRollbackFreshDbInvalidationRouteAuthorityError>
{
    let seal = UsrRollbackFreshDbInvalidationRouteSeal::new_for_test();
    let initial_in_flight = fixture.fixture.database.audit_in_flight_transition().unwrap();
    UsrRollbackFreshDbInvalidationRouteAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        journal,
        &fixture.fixture.database,
        reservation,
        record,
        initial_in_flight,
    )
}
