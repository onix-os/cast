use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{self, UsrRollbackResumeRouteSeal},
        startup_reconciliation::{UsrRollbackResumeRouteAdmission, UsrRollbackResumeRouteAuthority},
    },
    transition_journal::{
        InitialRollbackAction, Phase, RollbackActionOutcome, RollbackObservations, TransitionJournalStore,
        TransitionRecord,
    },
};

use super::fixture::{Fixture, OperationKind, SourceCase, exchange_usr_layout};

pub(super) struct RouteFixture {
    pub(super) fixture: Fixture,
    pub(super) source: TransitionRecord,
}

impl RouteFixture {
    pub(super) fn new(kind: OperationKind, source: SourceCase) -> Self {
        Self::from_fixture(Fixture::new(kind, source), kind, source)
    }

    pub(super) fn historical(kind: OperationKind, source: SourceCase) -> Self {
        Self::from_fixture(Fixture::historical(kind, source), kind, source)
    }

    fn from_fixture(fixture: Fixture, kind: OperationKind, source: SourceCase) -> Self {
        assert!(
            !matches!(source, SourceCase::ExchangedPre | SourceCase::RootLinksCompletePre),
            "a pre-exchange source requiring a pending reverse exchange cannot form a route fixture"
        );
        let usr_exchange = match source {
            SourceCase::IntentPre => InitialRollbackAction::AlreadySatisfied,
            SourceCase::IntentPost | SourceCase::ExchangedPost | SourceCase::RootLinksCompletePost => {
                InitialRollbackAction::Pending
            }
            SourceCase::ExchangedPre | SourceCase::RootLinksCompletePre => unreachable!(),
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
        Self {
            fixture,
            source: decision,
        }
    }

    pub(super) fn usr_restored(kind: OperationKind, source: SourceCase, outcome: RollbackActionOutcome) -> Self {
        assert!(
            matches!(source, SourceCase::IntentPost | SourceCase::ExchangedPost),
            "UsrRestored fixture requires a pending reverse-exchange route"
        );
        let mut fixture = Self::new(kind, source);
        let reverse_intent = fixture.source.rollback_successor(None).unwrap();
        assert_eq!(reverse_intent.phase, Phase::ReverseExchangeIntent);
        exchange_usr_layout(&fixture.fixture.installation.root);
        let restored = reverse_intent.rollback_successor(Some(outcome)).unwrap();
        assert_eq!(restored.phase, Phase::UsrRestored);
        let journal = fixture.open_journal();
        journal.advance(&fixture.source, &reverse_intent).unwrap();
        journal.advance(&reverse_intent, &restored).unwrap();
        drop(journal);
        fixture.source = restored;
        assert_eq!(fixture.canonical_record(), fixture.source);
        fixture
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
        self.source.rollback_successor(None).unwrap()
    }

    pub(super) fn expected_phase(&self) -> Phase {
        self.expected_route().phase
    }

    pub(super) fn assert_exact_route(&self, actual: &TransitionRecord) {
        let expected = self.expected_route();
        assert_eq!(actual, &expected);
        assert_eq!(actual.generation, self.source.generation + 1);
        assert_eq!(actual.rollback, self.source.rollback);
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
            &self.source,
            in_flight,
        )
        .unwrap()
        {
            UsrRollbackResumeRouteAdmission::Ready(authority) => authority,
            _ => panic!("exact RollbackDecided evidence did not admit routing authority"),
        }
    }
}
