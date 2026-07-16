use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackReverseSeal,
        startup_reconciliation::{UsrRollbackReverseAdmission, UsrRollbackReverseAuthority},
    },
    transition_journal::{
        InitialRollbackAction, Phase, RollbackActionOutcome, RollbackObservations, TransitionJournalStore,
        TransitionRecord, encode,
    },
};

use super::test_fixture::{DatabaseSnapshot, Fixture, NamespaceEntry, OperationKind, SourceCase};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReverseLayout {
    Post,
    Pre,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EffectOperationKind {
    NewState,
    Archived,
    ActiveReblit,
}

impl EffectOperationKind {
    pub(super) const ALL: [Self; 3] = [Self::NewState, Self::Archived, Self::ActiveReblit];

    fn fixture_kind(self) -> OperationKind {
        match self {
            Self::NewState => OperationKind::NewState,
            Self::Archived => OperationKind::Archived,
            Self::ActiveReblit => OperationKind::ActiveReblit,
        }
    }
}

pub(super) struct ReverseFixture {
    pub(super) fixture: Fixture,
    pub(super) reverse_intent: TransitionRecord,
    pub(super) record: TransitionRecord,
    initial_database: DatabaseSnapshot,
}

impl ReverseFixture {
    pub(super) fn new(kind: OperationKind, layout: ReverseLayout) -> Self {
        Self::from_source(kind, SourceCase::ExchangedPost, layout)
    }

    pub(super) fn from_source(kind: OperationKind, source: SourceCase, layout: ReverseLayout) -> Self {
        assert!(matches!(source, SourceCase::IntentPost | SourceCase::ExchangedPost));
        Self::build(Fixture::new(kind, source), kind, layout, false)
    }

    pub(super) fn historical(kind: OperationKind, layout: ReverseLayout) -> Self {
        Self::build(
            Fixture::historical(kind, SourceCase::ExchangedPost),
            kind,
            layout,
            false,
        )
    }

    pub(super) fn restored(kind: OperationKind) -> Self {
        Self::build(
            Fixture::new(kind, SourceCase::ExchangedPost),
            kind,
            ReverseLayout::Pre,
            true,
        )
    }

    pub(super) fn for_effect(kind: EffectOperationKind, layout: ReverseLayout) -> Self {
        Self::new(kind.fixture_kind(), layout)
    }

    fn build(fixture: Fixture, kind: OperationKind, layout: ReverseLayout, restored: bool) -> Self {
        let decision = fixture
            .source
            .rollback_decision(RollbackObservations {
                allocated_candidate_id: None,
                previous_archive: None,
                usr_exchange: Some(InitialRollbackAction::Pending),
                candidate: InitialRollbackAction::Pending,
                fresh_db: (kind == OperationKind::NewState).then_some(InitialRollbackAction::Pending),
            })
            .unwrap();
        let reverse_intent = decision.rollback_successor(None).unwrap();
        assert_eq!(reverse_intent.phase, Phase::ReverseExchangeIntent);

        let journal =
            TransitionJournalStore::open_retained(fixture.installation.root_directory(), &fixture.installation.root)
                .unwrap();
        journal.advance(&fixture.source, &decision).unwrap();
        journal.advance(&decision, &reverse_intent).unwrap();

        if layout == ReverseLayout::Pre {
            exchange_usr_layout(&fixture.installation.root);
        }
        let record = if restored {
            let restored = reverse_intent
                .rollback_successor(Some(RollbackActionOutcome::Applied))
                .unwrap();
            assert_eq!(restored.phase, Phase::UsrRestored);
            journal.advance(&reverse_intent, &restored).unwrap();
            restored
        } else {
            reverse_intent.clone()
        };
        drop(journal);
        assert_eq!(fixture.canonical_record(), record);
        let initial_database = fixture.database_snapshot();
        Self {
            fixture,
            reverse_intent,
            record,
            initial_database,
        }
    }

    pub(super) fn open_journal(&self) -> TransitionJournalStore {
        TransitionJournalStore::open_retained(
            self.fixture.installation.root_directory(),
            &self.fixture.installation.root,
        )
        .unwrap()
    }

    pub(super) fn capture<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackReverseAdmission<'reservation> {
        capture_record(&self.fixture, journal, reservation, &self.record)
    }

    pub(super) fn evidence_snapshots(&self) -> (Vec<u8>, DatabaseSnapshot, Vec<NamespaceEntry>) {
        (
            self.fixture.canonical_bytes(),
            self.fixture.database_snapshot(),
            self.fixture.namespace_snapshot(),
        )
    }

    pub(super) fn assert_evidence_unchanged(&self, expected: &(Vec<u8>, DatabaseSnapshot, Vec<NamespaceEntry>)) {
        assert_eq!(self.fixture.canonical_bytes(), expected.0);
        assert_eq!(self.fixture.database_snapshot(), expected.1);
        assert_eq!(self.fixture.namespace_snapshot(), expected.2);
    }

    pub(super) fn assert_non_namespace_unchanged(&self) {
        assert_eq!(self.fixture.canonical_record(), self.record);
        assert_eq!(self.fixture.database_snapshot(), self.initial_database);
    }

    pub(super) fn namespace_change_hook(&self, name: String) -> impl FnOnce() + 'static {
        let inserted = self.fixture.installation.state_quarantine_dir().join(name);
        move || super::test_fixture::create_private_directory(&inserted)
    }

    pub(super) fn candidate_transition_clear_hook(&self) -> impl FnOnce() + 'static {
        let database = self.fixture.database.clone();
        let candidate = self.fixture.candidate_state;
        let transition = self.reverse_intent.transition_id.clone();
        move || {
            database.clear_transition_if_matches(candidate, &transition).unwrap();
        }
    }

    pub(super) fn journal_change_hook(&self) -> impl FnOnce() + 'static {
        let canonical = super::test_fixture::canonical_journal(&self.fixture.installation.root);
        let changed = self
            .record
            .rollback_successor(Some(RollbackActionOutcome::Applied))
            .unwrap();
        let bytes = encode(&changed).unwrap();
        move || fs::write(canonical, bytes).unwrap()
    }

    pub(super) fn durability_parent_identities(&self) -> ((u64, u64), (u64, u64)) {
        self.fixture.durability_parent_identities()
    }
}

pub(super) fn capture_record<'reservation>(
    fixture: &Fixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> UsrRollbackReverseAdmission<'reservation> {
    let seal = UsrRollbackReverseSeal::new_for_test();
    let in_flight = fixture.database.audit_in_flight_transition().unwrap();
    UsrRollbackReverseAuthority::capture(
        &seal,
        &fixture.installation,
        journal,
        &fixture.database,
        reservation,
        record,
        in_flight,
    )
    .unwrap()
}

fn exchange_usr_layout(root: &std::path::Path) {
    let live = root.join("usr");
    let staging = root.join(".cast/root/staging/usr");
    let parked = root.join(".cast/root/.rollback-reverse-fixture");
    fs::rename(&live, &parked).unwrap();
    fs::rename(&staging, &live).unwrap();
    fs::rename(&parked, &staging).unwrap();
}
