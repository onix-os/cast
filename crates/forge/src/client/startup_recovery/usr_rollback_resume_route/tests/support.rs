use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{self, UsrRollbackResumeRouteSeal},
        startup_reconciliation::{UsrRollbackResumeRouteAdmission, UsrRollbackResumeRouteAuthority},
    },
    transition_journal::{
        InitialRollbackAction, Phase, RollbackObservations, TransitionJournalStore, TransitionRecord,
    },
};

use super::fixture::{Fixture, OperationKind, SourceCase};

pub(super) struct RouteFixture {
    pub(super) fixture: Fixture,
    pub(super) decision: TransitionRecord,
}

impl RouteFixture {
    pub(super) fn new(kind: OperationKind, source: SourceCase) -> Self {
        Self::from_fixture(Fixture::new(kind, source), kind, source)
    }

    pub(super) fn historical(kind: OperationKind, source: SourceCase) -> Self {
        Self::from_fixture(Fixture::historical(kind, source), kind, source)
    }

    fn from_fixture(fixture: Fixture, kind: OperationKind, source: SourceCase) -> Self {
        assert_ne!(
            source,
            SourceCase::ExchangedPre,
            "incompatible source cannot form a route fixture"
        );
        let usr_exchange = match source {
            SourceCase::IntentPre => InitialRollbackAction::AlreadySatisfied,
            SourceCase::IntentPost | SourceCase::ExchangedPost => InitialRollbackAction::Pending,
            SourceCase::ExchangedPre => unreachable!(),
        };
        let decision = fixture
            .source
            .rollback_decision(RollbackObservations {
                allocated_candidate_id: None,
                previous_archive: None,
                usr_exchange: Some(usr_exchange),
                candidate: InitialRollbackAction::Pending,
                fresh_db: (kind == OperationKind::NewState).then_some(InitialRollbackAction::Pending),
            })
            .unwrap();
        let journal =
            TransitionJournalStore::open_retained(fixture.installation.root_directory(), &fixture.installation.root)
                .unwrap();
        journal.advance(&fixture.source, &decision).unwrap();
        drop(journal);
        assert_eq!(fixture.canonical_record(), decision);
        Self { fixture, decision }
    }

    pub(super) fn enter(&self) -> startup_gate::Error {
        self.fixture.enter()
    }

    pub(super) fn open_journal(&self) -> TransitionJournalStore {
        TransitionJournalStore::open_retained(
            self.fixture.installation.root_directory(),
            &self.fixture.installation.root,
        )
        .unwrap()
    }

    pub(super) fn canonical_record(&self) -> TransitionRecord {
        self.fixture.canonical_record()
    }

    pub(super) fn expected_route(&self) -> TransitionRecord {
        self.decision.rollback_successor(None).unwrap()
    }

    pub(super) fn expected_phase(&self) -> Phase {
        self.expected_route().phase
    }

    pub(super) fn assert_exact_route(&self, actual: &TransitionRecord) {
        let expected = self.expected_route();
        assert_eq!(actual, &expected);
        assert_eq!(actual.generation, self.decision.generation + 1);
        assert_eq!(actual.rollback, self.decision.rollback);
    }

    pub(super) fn capture_ready<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackResumeRouteAuthority<'reservation> {
        let seal = UsrRollbackResumeRouteSeal::new_for_test();
        let in_flight = self.fixture.database.audit_in_flight_transition().unwrap();
        match UsrRollbackResumeRouteAuthority::capture(
            &seal,
            &self.fixture.installation,
            journal,
            &self.fixture.database,
            reservation,
            &self.decision,
            in_flight,
        )
        .unwrap()
        {
            UsrRollbackResumeRouteAdmission::Ready(authority) => authority,
            _ => panic!("exact RollbackDecided evidence did not admit routing authority"),
        }
    }
}
