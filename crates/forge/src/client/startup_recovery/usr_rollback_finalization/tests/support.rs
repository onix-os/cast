use crate::{
    Installation,
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackFinalizationSeal,
        startup_reconciliation::{
            UsrRollbackFinalizationAdmission, UsrRollbackFinalizationAuthority, UsrRollbackFinalizationAuthorityError,
        },
        startup_recovery::persist_usr_rollback_complete_route_and_reopen,
    },
    db,
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

pub(super) use super::super::invalidation_test_support::{
    CandidateOutcome as CandidateResult, CandidateSource as Source,
};
use super::route_support::RouteFixture;

pub(super) use super::route_support::{
    DatabaseSnapshot, FreshDbOutcome, NamespaceEntry, canonical_journal, transition_quarantine_path,
};

pub(super) struct FinalizationFixture {
    pub(super) route: RouteFixture,
    pub(super) source: TransitionRecord,
}

impl FinalizationFixture {
    pub(super) fn new(
        origin: FreshDbOutcome,
        source: Source,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateResult,
    ) -> Self {
        Self::build(origin, false, source, usr_outcome, candidate_outcome)
    }

    pub(super) fn historical(
        origin: FreshDbOutcome,
        source: Source,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateResult,
    ) -> Self {
        Self::build(origin, true, source, usr_outcome, candidate_outcome)
    }

    fn build(
        origin: FreshDbOutcome,
        historical: bool,
        source: Source,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateResult,
    ) -> Self {
        let route = if historical {
            RouteFixture::historical(origin, source, usr_outcome, candidate_outcome)
        } else {
            RouteFixture::new(origin, source, usr_outcome, candidate_outcome)
        };
        let journal = route.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = route.capture_ready(&journal, &reservation);
        let expected = route.expected_successor();
        let (reopened, actual) = persist_usr_rollback_complete_route_and_reopen(journal, authority).unwrap();
        drop(reopened);
        drop(reservation);
        assert_eq!(actual, expected);
        assert_eq!(actual.phase, Phase::RollbackComplete);
        route.assert_no_second_removal();
        Self { route, source: actual }
    }

    pub(super) fn open_journal(&self) -> TransitionJournalStore {
        self.route.open_journal()
    }

    pub(super) fn canonical_record(&self) -> TransitionRecord {
        self.route.canonical_record()
    }

    pub(super) fn database_snapshot(&self) -> DatabaseSnapshot {
        self.route.database_snapshot()
    }

    pub(super) fn namespace_snapshot(&self) -> Vec<NamespaceEntry> {
        self.route.namespace_snapshot()
    }

    pub(super) fn installation(&self) -> &Installation {
        &self.route.fixture.fixture.fixture.installation
    }

    pub(super) fn database(&self) -> &db::state::Database {
        &self.route.fixture.fixture.fixture.database
    }

    pub(super) fn previous_state(&self) -> crate::state::Id {
        self.route.fixture.fixture.fixture.previous_state
    }

    pub(super) fn preterminal_record(&self) -> &TransitionRecord {
        &self.route.source
    }

    pub(super) fn transition_target(&self) -> std::path::PathBuf {
        transition_quarantine_path(&self.route.fixture.fixture.fixture, &self.source)
    }

    pub(super) fn capture<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> Result<UsrRollbackFinalizationAdmission<'reservation>, UsrRollbackFinalizationAuthorityError> {
        let seal = UsrRollbackFinalizationSeal::new_for_test();
        UsrRollbackFinalizationAuthority::capture(
            &seal,
            self.installation(),
            journal,
            self.database(),
            reservation,
            &self.source,
        )
    }

    pub(super) fn capture_ready<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackFinalizationAuthority<'reservation> {
        match self.capture(journal, reservation).unwrap() {
            UsrRollbackFinalizationAdmission::Ready(authority) => authority,
            UsrRollbackFinalizationAdmission::NotApplicable | UsrRollbackFinalizationAdmission::Deferred => {
                panic!("exact NewState RollbackComplete evidence did not admit finalization")
            }
        }
    }

    pub(super) fn assert_no_second_removal(&self) {
        self.route.assert_no_second_removal();
    }
}
