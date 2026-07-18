use std::os::unix::fs::symlink;

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

const ROOT_ABI: [(&str, &str); 5] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib32", "usr/lib32"),
    ("lib64", "usr/lib"),
];

pub(super) struct FinalizationFixture {
    pub(super) fixture: CandidatePreserveFixture,
    pub(super) preterminal: TransitionRecord,
    pub(super) terminal: TransitionRecord,
}

impl FinalizationFixture {
    pub(super) fn new() -> Self {
        let fixture = CandidatePreserveFixture::new(
            OperationKind::Archived,
            CandidateSource::Intent,
            RollbackActionOutcome::Applied,
            CandidateLayout::Preserved,
        );
        for (name, target) in ROOT_ABI {
            symlink(target, fixture.fixture.installation.root.join(name)).unwrap();
        }
        let preterminal = fixture
            .candidate_intent
            .rollback_successor(Some(RollbackActionOutcome::Applied))
            .unwrap();
        assert_eq!(preterminal.phase, Phase::CandidatePreserved);
        let terminal = preterminal.rollback_successor(None).unwrap();
        assert_eq!(terminal.phase, Phase::RollbackComplete);
        let journal = fixture.open_journal();
        journal.advance(&fixture.candidate_intent, &preterminal).unwrap();
        journal.advance(&preterminal, &terminal).unwrap();
        drop(journal);
        Self {
            fixture,
            preterminal,
            terminal,
        }
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
            panic!("exact terminal ActivateArchived evidence did not admit finalization");
        };
        authority
    }
}
