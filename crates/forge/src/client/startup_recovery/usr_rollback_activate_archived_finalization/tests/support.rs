use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackActivateArchivedFinalizationSeal,
        startup_reconciliation::{
            UsrRollbackActivateArchivedFinalizationAdmission, UsrRollbackActivateArchivedFinalizationAuthority,
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
            OperationKind::Archived,
            CandidateSource::RootLinksComplete,
            RollbackActionOutcome::Applied,
            CandidateLayout::Preserved,
        );
        let preserved = fixture
            .candidate_intent
            .rollback_successor(Some(RollbackActionOutcome::Applied))
            .unwrap();
        assert_eq!(preserved.phase, Phase::CandidatePreserved);
        let terminal = preserved.rollback_successor(None).unwrap();
        assert_eq!(terminal.phase, Phase::RollbackComplete);
        assert_eq!(terminal.generation, 12);
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
    ) -> UsrRollbackActivateArchivedFinalizationAuthority<'reservation> {
        let seal = UsrRollbackActivateArchivedFinalizationSeal::new_for_test();
        let admission = UsrRollbackActivateArchivedFinalizationAuthority::capture(
            &seal,
            &self.fixture.fixture.installation,
            journal,
            &self.fixture.fixture.database,
            reservation,
            &self.terminal,
        )
        .unwrap();
        let UsrRollbackActivateArchivedFinalizationAdmission::Ready(authority) = admission else {
            panic!("exact generation-12 RootLinks ActivateArchived terminal did not admit finalization");
        };
        authority
    }
}
