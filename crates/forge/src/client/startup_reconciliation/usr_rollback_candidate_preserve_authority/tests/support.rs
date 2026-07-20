use std::{fs, os::unix::fs::PermissionsExt as _, path::PathBuf};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackCandidatePreserveSeal,
        startup_reconciliation::{UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveAuthority},
    },
    transition_journal::{
        InitialRollbackAction, Phase, RollbackActionOutcome, RollbackObservations, TransitionJournalStore,
        TransitionRecord, encode,
    },
};

use super::test_fixture::{BootSyncStartedLayout, DatabaseSnapshot, Fixture, NamespaceEntry, OperationKind, SourceCase};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateSource {
    Intent,
    Exchanged,
    RootLinksComplete,
}

impl CandidateSource {
    pub(super) const ALL: [Self; 2] = [Self::Intent, Self::Exchanged];
    pub(super) const THROUGH_CANDIDATE_PRESERVED: [Self; 3] = [
        Self::Intent,
        Self::Exchanged,
        Self::RootLinksComplete,
    ];

    fn fixture_source(self) -> SourceCase {
        match self {
            Self::Intent => SourceCase::IntentPost,
            Self::Exchanged => SourceCase::ExchangedPost,
            Self::RootLinksComplete => SourceCase::RootLinksCompletePost,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateLayout {
    Staged,
    Preserved,
}

pub(super) struct CandidatePreserveFixture {
    pub(super) fixture: Fixture,
    pub(super) candidate_intent: TransitionRecord,
    initial_database: DatabaseSnapshot,
}

impl CandidatePreserveFixture {
    pub(super) fn new(
        kind: OperationKind,
        source: CandidateSource,
        usr_reverse_outcome: RollbackActionOutcome,
        layout: CandidateLayout,
    ) -> Self {
        Self::build(
            Fixture::new(kind, source.fixture_source()),
            kind,
            usr_reverse_outcome,
            layout,
        )
    }

    pub(super) fn historical(
        kind: OperationKind,
        source: CandidateSource,
        usr_reverse_outcome: RollbackActionOutcome,
        layout: CandidateLayout,
    ) -> Self {
        Self::build(
            Fixture::historical(kind, source.fixture_source()),
            kind,
            usr_reverse_outcome,
            layout,
        )
    }

    pub(super) fn active_reblit_boot_sync_started(
        historical: bool,
        usr_reverse_outcome: RollbackActionOutcome,
        layout: CandidateLayout,
    ) -> Self {
        let fixture = Fixture::active_reblit_boot_sync_started(BootSyncStartedLayout::Post, historical);
        super::test_fixture::install_root_abi(&fixture.installation.root);
        Self::build(
            fixture,
            OperationKind::ActiveReblit,
            usr_reverse_outcome,
            layout,
        )
    }

    fn build(
        fixture: Fixture,
        kind: OperationKind,
        usr_reverse_outcome: RollbackActionOutcome,
        layout: CandidateLayout,
    ) -> Self {
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
        super::test_fixture::exchange_usr_layout(&fixture.installation.root);

        // This freezes the earlier `/usr` reverse result. Candidate
        // preservation itself remains entirely unexecuted in this fixture.
        let restored = reverse_intent.rollback_successor(Some(usr_reverse_outcome)).unwrap();
        assert_eq!(restored.phase, Phase::UsrRestored);
        journal.advance(&reverse_intent, &restored).unwrap();
        let candidate_intent = restored.rollback_successor(None).unwrap();
        assert_eq!(candidate_intent.phase, Phase::CandidatePreserveIntent);
        journal.advance(&restored, &candidate_intent).unwrap();
        drop(journal);

        if kind == OperationKind::Archived {
            create_archived_staged_topology(&fixture, &candidate_intent);
        }
        if layout == CandidateLayout::Preserved {
            synthesize_preserved_topology(&fixture, &candidate_intent);
        }
        assert_eq!(fixture.canonical_record(), candidate_intent);
        let initial_database = fixture.database_snapshot();
        Self {
            fixture,
            candidate_intent,
            initial_database,
        }
    }

    pub(super) fn with_new_state_empty_quarantine_prefix() -> Self {
        Self::new_state_empty_quarantine_prefix(CandidateSource::Exchanged, RollbackActionOutcome::Applied)
    }

    pub(super) fn new_state_empty_quarantine_prefix(
        source: CandidateSource,
        usr_reverse_outcome: RollbackActionOutcome,
    ) -> Self {
        Self::new_state_empty_quarantine_prefix_at_epoch(false, source, usr_reverse_outcome)
    }

    pub(super) fn new_state_empty_quarantine_prefix_at_epoch(
        historical: bool,
        source: CandidateSource,
        usr_reverse_outcome: RollbackActionOutcome,
    ) -> Self {
        let fixture = if historical {
            Self::historical(
                OperationKind::NewState,
                source,
                usr_reverse_outcome,
                CandidateLayout::Staged,
            )
        } else {
            Self::new(
                OperationKind::NewState,
                source,
                usr_reverse_outcome,
                CandidateLayout::Staged,
            )
        };
        create_quarantine_wrapper(&fixture.fixture, &fixture.candidate_intent);
        fixture
    }

    pub(super) fn new_state_target_residue(
        source: CandidateSource,
        usr_reverse_outcome: RollbackActionOutcome,
        mode: u32,
    ) -> Self {
        Self::new_state_target_residue_at_epoch(false, source, usr_reverse_outcome, mode)
    }

    pub(super) fn new_state_target_residue_at_epoch(
        historical: bool,
        source: CandidateSource,
        usr_reverse_outcome: RollbackActionOutcome,
        mode: u32,
    ) -> Self {
        let fixture = if historical {
            Self::historical(
                OperationKind::NewState,
                source,
                usr_reverse_outcome,
                CandidateLayout::Staged,
            )
        } else {
            Self::new(
                OperationKind::NewState,
                source,
                usr_reverse_outcome,
                CandidateLayout::Staged,
            )
        };
        let target = create_quarantine_wrapper(&fixture.fixture, &fixture.candidate_intent);
        fs::set_permissions(target, fs::Permissions::from_mode(mode)).unwrap();
        fixture
    }

    pub(super) fn with_active_reblit_wrapper_index(mut self, index: usize) -> Self {
        assert_eq!(self.fixture.kind, OperationKind::ActiveReblit);
        let current = self
            .fixture
            .active_reblit_reservation
            .take()
            .expect("active-reblit fixture reserves its replacement wrapper");
        let replacement = active_reblit_wrapper_path(&self.fixture, &self.candidate_intent, index);
        fs::rename(&current, &replacement).unwrap();
        self.fixture.active_reblit_reservation = Some(replacement);
        self
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
    ) -> UsrRollbackCandidatePreserveAdmission<'reservation> {
        capture_record(&self.fixture, journal, reservation, &self.candidate_intent)
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
        assert_eq!(self.fixture.canonical_record(), self.candidate_intent);
        assert_eq!(self.fixture.database_snapshot(), self.initial_database);
    }

    pub(super) fn namespace_change_hook(&self, name: String) -> impl FnOnce() + 'static {
        let inserted = self.fixture.installation.state_quarantine_dir().join(name);
        move || super::test_fixture::create_private_directory(&inserted)
    }

    pub(super) fn candidate_transition_clear_hook(&self) -> impl FnOnce() + 'static {
        let database = self.fixture.database.clone();
        let candidate = self.fixture.candidate_state;
        let transition = self.candidate_intent.transition_id.clone();
        move || {
            database.clear_transition_if_matches(candidate, &transition).unwrap();
        }
    }

    pub(super) fn journal_change_hook(&self) -> impl FnOnce() + 'static {
        let canonical = super::test_fixture::canonical_journal(&self.fixture.installation.root);
        let changed = self
            .candidate_intent
            .rollback_successor(Some(RollbackActionOutcome::Applied))
            .unwrap();
        let bytes = encode(&changed).unwrap();
        move || fs::write(canonical, bytes).unwrap()
    }
}

pub(super) fn capture_record<'reservation>(
    fixture: &Fixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> UsrRollbackCandidatePreserveAdmission<'reservation> {
    let seal = UsrRollbackCandidatePreserveSeal::new_for_test();
    let in_flight = fixture.database.audit_in_flight_transition().unwrap();
    UsrRollbackCandidatePreserveAuthority::capture(
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

/// Synthesize only the archived staged shape consumed by read-only admission.
/// Creating the slot after journal setup is fixture convenience, not claimed
/// production ordering, durability, or candidate-preservation behavior.
fn create_archived_staged_topology(fixture: &Fixture, record: &TransitionRecord) {
    let wrapper = archived_state_wrapper(fixture);
    super::test_fixture::create_private_directory(&wrapper);
    let marker = fixture.installation.staging_dir().join("usr/.cast-tree-id");
    fs::hard_link(marker, wrapper.join(slot_name(fixture, record))).unwrap();
}

/// Build only the final namespace shape consumed by the read-only proof.
/// This fixture is not an implementation or behavioral test of preservation.
fn synthesize_preserved_topology(fixture: &Fixture, record: &TransitionRecord) {
    match fixture.kind {
        OperationKind::NewState => {
            let destination = create_quarantine_wrapper(fixture, record);
            fs::rename(fixture.installation.staging_dir().join("usr"), destination.join("usr")).unwrap();
        }
        OperationKind::Archived => {
            let state = archived_state_wrapper(fixture);
            fs::rename(fixture.installation.staging_dir().join("usr"), state.join("usr")).unwrap();
        }
        OperationKind::ActiveReblit => {
            let destination = fixture
                .active_reblit_reservation
                .as_ref()
                .expect("active-reblit fixture reserves its replacement wrapper");
            let staging = fixture.installation.staging_dir();
            let temporary = fixture
                .installation
                .state_quarantine_dir()
                .join(".candidate-preserve-wrapper-exchange");
            fs::rename(destination, &temporary).unwrap();
            fs::rename(&staging, destination).unwrap();
            fs::rename(&temporary, &staging).unwrap();
        }
    }
}

fn create_quarantine_wrapper(fixture: &Fixture, record: &TransitionRecord) -> PathBuf {
    let destination = fixture
        .installation
        .state_quarantine_dir()
        .join(record.quarantine_name.as_str());
    super::test_fixture::create_private_directory(&destination);
    destination
}

fn archived_state_wrapper(fixture: &Fixture) -> PathBuf {
    fixture
        .installation
        .root
        .join(".cast/root")
        .join(fixture.candidate_state.to_string())
}

fn slot_name(fixture: &Fixture, record: &TransitionRecord) -> String {
    format!(
        ".cast-state-slot-{}-{}",
        fixture.candidate_state,
        record.candidate.tree_token.as_str()
    )
}

pub(super) fn active_reblit_wrapper_path(fixture: &Fixture, record: &TransitionRecord, index: usize) -> PathBuf {
    fixture.installation.state_quarantine_dir().join(format!(
        "replaced-active-reblit-wrapper-{}-{}-{index}",
        fixture.previous_state,
        record.previous.tree_token.as_str()
    ))
}

pub(super) fn reserved_active_reblit_wrapper_path(
    fixture: &CandidatePreserveFixture,
    layout: CandidateLayout,
) -> PathBuf {
    assert_eq!(fixture.fixture.kind, OperationKind::ActiveReblit);
    match layout {
        CandidateLayout::Staged => fixture
            .fixture
            .active_reblit_reservation
            .clone()
            .expect("active-reblit fixture reserves its replacement wrapper"),
        CandidateLayout::Preserved => fixture.fixture.installation.staging_dir(),
    }
}

pub(super) fn archived_slot_path(fixture: &Fixture, record: &TransitionRecord) -> PathBuf {
    archived_state_wrapper(fixture).join(slot_name(fixture, record))
}

pub(super) fn transition_quarantine_path(fixture: &Fixture, record: &TransitionRecord) -> PathBuf {
    fixture
        .installation
        .state_quarantine_dir()
        .join(record.quarantine_name.as_str())
}
