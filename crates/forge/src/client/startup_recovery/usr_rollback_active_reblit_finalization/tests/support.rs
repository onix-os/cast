use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackActiveReblitFinalizationSeal,
        startup_reconciliation::{
            UsrRollbackActiveReblitFinalizationAdmission, UsrRollbackActiveReblitFinalizationAuthority,
        },
    },
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

use super::{
    candidate_test_support::{CandidateLayout, CandidatePreserveFixture, CandidateSource},
    test_fixture::OperationKind,
};

pub(super) struct FinalizationFixture {
    pub(super) fixture: CandidatePreserveFixture,
    pub(super) terminal: TransitionRecord,
}

impl FinalizationFixture {
    pub(super) fn new() -> Self {
        let fixture = CandidatePreserveFixture::new(
            OperationKind::ActiveReblit,
            CandidateSource::RootLinksComplete,
            RollbackActionOutcome::Applied,
            CandidateLayout::Preserved,
        )
        .with_active_reblit_wrapper_index(13);
        let preserved = fixture
            .candidate_intent
            .rollback_successor(Some(RollbackActionOutcome::Applied))
            .unwrap();
        assert_eq!(preserved.phase, Phase::CandidatePreserved);
        let terminal = preserved.rollback_successor(None).unwrap();
        assert_eq!(terminal.phase, Phase::RollbackComplete);
        assert_eq!(terminal.generation, 14);
        let journal = fixture.open_journal();
        journal.advance(&fixture.candidate_intent, &preserved).unwrap();
        journal.advance(&preserved, &terminal).unwrap();
        drop(journal);
        Self { fixture, terminal }
    }

    pub(super) fn open_journal(&self) -> TransitionJournalStore {
        self.fixture.open_journal()
    }

    pub(super) fn capture_ready<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackActiveReblitFinalizationAuthority<'reservation> {
        let seal = UsrRollbackActiveReblitFinalizationSeal::new_for_test();
        let admission = UsrRollbackActiveReblitFinalizationAuthority::capture(
            &seal,
            &self.fixture.fixture.installation,
            journal,
            &self.fixture.fixture.database,
            reservation,
            &self.terminal,
        )
        .unwrap();
        let UsrRollbackActiveReblitFinalizationAdmission::Ready(authority) = admission else {
            panic!("exact generation-14 RootLinks ActiveReblit terminal did not admit finalization");
        };
        authority
    }
}
