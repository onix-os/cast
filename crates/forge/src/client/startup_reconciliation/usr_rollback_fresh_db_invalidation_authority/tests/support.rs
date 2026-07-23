use std::fs;

use crate::{
    State,
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackFreshDbInvalidationSeal,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationAdmission, UsrRollbackFreshDbInvalidationApplyAuthority,
            UsrRollbackFreshDbInvalidationAuthority, UsrRollbackFreshDbInvalidationAuthorityError,
            UsrRollbackFreshDbInvalidationFinishAuthority,
        },
    },
    db,
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord, encode},
};

use super::{
    candidate_test_support::{CandidateLayout, CandidatePreserveFixture},
    test_fixture::{NamespaceEntry, OperationKind},
};

pub(super) use super::candidate_test_support::{CandidateSource, transition_quarantine_path};
pub(super) use super::test_fixture::{canonical_journal, create_private_directory};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateOutcome {
    Applied,
    AlreadySatisfied,
}

impl CandidateOutcome {
    pub(super) const ALL: [Self; 2] = [Self::Applied, Self::AlreadySatisfied];

    fn journal_outcome(self) -> RollbackActionOutcome {
        match self {
            Self::Applied => RollbackActionOutcome::Applied,
            Self::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FreshRowLayout {
    Present,
    JointlyAbsent,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct PreviousDatabaseEvidence {
    state: State,
    ownership: db::state::TransitionOwnership,
    provenance: Option<db::state::MetadataProvenance>,
}

pub(super) struct FreshDbInvalidationFixture {
    pub(super) fixture: CandidatePreserveFixture,
    pub(super) candidate_preserved: TransitionRecord,
    pub(super) record: TransitionRecord,
}

impl FreshDbInvalidationFixture {
    pub(super) fn new(
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
        row: FreshRowLayout,
    ) -> Self {
        Self::build(
            CandidatePreserveFixture::new(OperationKind::NewState, source, usr_outcome, CandidateLayout::Preserved),
            candidate_outcome,
            row,
        )
    }

    pub(super) fn historical(
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
        row: FreshRowLayout,
    ) -> Self {
        Self::build(
            CandidatePreserveFixture::historical(
                OperationKind::NewState,
                source,
                usr_outcome,
                CandidateLayout::Preserved,
            ),
            candidate_outcome,
            row,
        )
    }

    fn build(fixture: CandidatePreserveFixture, candidate_outcome: CandidateOutcome, row: FreshRowLayout) -> Self {
        let candidate_preserved = fixture
            .candidate_intent
            .rollback_successor(Some(candidate_outcome.journal_outcome()))
            .expect("preserved candidate fixture must admit CandidatePreserved");
        assert_eq!(candidate_preserved.phase, Phase::CandidatePreserved);
        let record = candidate_preserved
            .rollback_successor(None)
            .expect("CandidatePreserved must route to FreshDbInvalidationIntent");
        assert_eq!(record.phase, Phase::FreshDbInvalidationIntent);

        let journal = fixture.open_journal();
        journal
            .advance(&fixture.candidate_intent, &candidate_preserved)
            .unwrap();
        journal.advance(&candidate_preserved, &record).unwrap();
        drop(journal);

        let built = Self {
            fixture,
            candidate_preserved,
            record,
        };
        if row == FreshRowLayout::JointlyAbsent {
            built.remove_fresh_row_for_fixture();
        }
        assert_eq!(built.canonical_record(), built.record);
        built
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

    pub(super) fn namespace_snapshot(&self) -> Vec<NamespaceEntry> {
        self.fixture.fixture.namespace_snapshot()
    }

    pub(super) fn previous_database_evidence(&self) -> PreviousDatabaseEvidence {
        let fixture = &self.fixture.fixture;
        PreviousDatabaseEvidence {
            state: fixture.database.get(fixture.previous_state).unwrap(),
            ownership: fixture
                .database
                .transition_ownership(fixture.previous_state, &self.record.transition_id)
                .unwrap(),
            provenance: fixture.database.metadata_provenance(fixture.previous_state).unwrap(),
        }
    }

    pub(super) fn assert_journal_namespace_and_previous_unchanged(
        &self,
        canonical: &[u8],
        namespace: &[NamespaceEntry],
        previous: &PreviousDatabaseEvidence,
    ) {
        assert_eq!(self.canonical_bytes(), canonical);
        assert_eq!(self.namespace_snapshot(), namespace);
        assert_eq!(&self.previous_database_evidence(), previous);
    }

    pub(super) fn assert_exact_present(&self) {
        assert!(matches!(
            self.fixture
                .fixture
                .database
                .inspect_exact_fresh_transition(self.fixture.fixture.candidate_state, &self.record.transition_id,),
            Ok(db::state::ExactFreshTransitionObservation::Present(_))
        ));
    }

    pub(super) fn assert_exact_joint_absence(&self) {
        assert!(matches!(
            self.fixture
                .fixture
                .database
                .inspect_exact_fresh_transition(self.fixture.fixture.candidate_state, &self.record.transition_id,),
            Ok(db::state::ExactFreshTransitionObservation::JointlyAbsent(_))
        ));
    }

    pub(super) fn capture<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> Result<UsrRollbackFreshDbInvalidationAdmission<'reservation>, UsrRollbackFreshDbInvalidationAuthorityError>
    {
        capture_record(&self.fixture, journal, reservation, &self.record)
    }

    pub(super) fn capture_apply<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackFreshDbInvalidationApplyAuthority<'reservation> {
        match self.capture(journal, reservation).unwrap() {
            UsrRollbackFreshDbInvalidationAdmission::Apply(authority) => authority,
            _ => panic!("exact present FreshDbInvalidationIntent evidence did not admit Apply"),
        }
    }

    pub(super) fn capture_finish<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackFreshDbInvalidationFinishAuthority<'reservation> {
        match self.capture(journal, reservation).unwrap() {
            UsrRollbackFreshDbInvalidationAdmission::Finish(authority) => authority,
            _ => panic!("bound joint absence at FreshDbInvalidationIntent did not admit Finish"),
        }
    }

    pub(super) fn overwrite_canonical(&self, record: &TransitionRecord) {
        fs::write(
            canonical_journal(&self.fixture.fixture.installation.root),
            encode(record).unwrap(),
        )
        .unwrap();
    }

    pub(super) fn journal_change_hook(&self) -> impl FnOnce() + 'static {
        let canonical = canonical_journal(&self.fixture.fixture.installation.root);
        let changed = self
            .record
            .rollback_successor(Some(RollbackActionOutcome::Applied))
            .unwrap();
        let bytes = encode(&changed).unwrap();
        move || fs::write(canonical, bytes).unwrap()
    }

    pub(super) fn transition_clear_hook(&self) -> impl FnOnce() + 'static {
        let database = self.fixture.fixture.database.clone();
        let candidate = self.fixture.fixture.candidate_state;
        let transition = self.record.transition_id.clone();
        move || {
            database.clear_transition_if_matches(candidate, &transition).unwrap();
        }
    }

    pub(super) fn provenance_delete_hook(&self) -> impl FnOnce() + 'static {
        let database = self.fixture.fixture.database.clone();
        let candidate = self.fixture.fixture.candidate_state;
        move || {
            database.delete_metadata_provenance_for_test(candidate).unwrap();
        }
    }

    fn remove_fresh_row_for_fixture(&self) {
        let observation = self
            .fixture
            .fixture
            .database
            .inspect_exact_fresh_transition(self.fixture.fixture.candidate_state, &self.record.transition_id)
            .unwrap();
        let db::state::ExactFreshTransitionObservation::Present(preimage) = observation else {
            panic!("fresh-row fixture expected one complete preimage");
        };
        self.fixture
            .fixture
            .database
            .remove_exact_fresh_transition(preimage)
            .unwrap();
        self.assert_exact_joint_absence();
    }
}

pub(super) fn capture_record<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> Result<UsrRollbackFreshDbInvalidationAdmission<'reservation>, UsrRollbackFreshDbInvalidationAuthorityError> {
    let seal = UsrRollbackFreshDbInvalidationSeal::new_for_test();
    UsrRollbackFreshDbInvalidationAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        journal,
        &fixture.fixture.database,
        reservation,
        record,
    )
}
