use std::path::Path;

use crate::{
    Installation, db,
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackCompleteRouteSeal,
        startup_reconciliation::{
            UsrRollbackCompleteRouteAdmission, UsrRollbackCompleteRouteAuthority,
            UsrRollbackCompleteRouteAuthorityError, UsrRollbackFreshDbInvalidationApplyReconciliation,
            fresh_db_invalidation_removal_call_count,
        },
        startup_recovery::{
            UsrRollbackFreshDbInvalidationEffectSeal, persist_usr_rollback_fresh_db_invalidation_and_reopen,
        },
    },
    installation::DatabaseKind,
    test_support::private_installation_tempdir,
    transition_journal::{Phase, RollbackAction, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

use super::super::invalidation_test_support::{
    CandidateOutcome, CandidateSource, FreshDbInvalidationFixture, FreshRowLayout,
};

pub(super) use super::super::{
    invalidation_test_support::{
        canonical_journal, capture_record as capture_invalidation_record, create_private_directory,
        transition_quarantine_path,
    },
    test_fixture::{DatabaseSnapshot, NamespaceEntry},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FreshDbOutcome {
    Applied,
    AlreadySatisfied,
}

impl FreshDbOutcome {
    pub(super) const ALL: [Self; 2] = [Self::Applied, Self::AlreadySatisfied];

    pub(super) fn outcome(self) -> RollbackActionOutcome {
        match self {
            Self::Applied => RollbackActionOutcome::Applied,
            Self::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        }
    }

    pub(super) fn action(self) -> RollbackAction {
        match self {
            Self::Applied => RollbackAction::Applied,
            Self::AlreadySatisfied => RollbackAction::AlreadySatisfied,
        }
    }

    pub(super) fn expected_removals(self) -> usize {
        usize::from(self == Self::Applied)
    }
}

pub(super) struct RouteFixture {
    pub(super) fixture: FreshDbInvalidationFixture,
    pub(super) source: TransitionRecord,
    pub(super) origin: FreshDbOutcome,
}

impl RouteFixture {
    pub(super) fn new(
        origin: FreshDbOutcome,
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        Self::build(origin, false, source, usr_outcome, candidate_outcome)
    }

    pub(super) fn historical(
        origin: FreshDbOutcome,
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        Self::build(origin, true, source, usr_outcome, candidate_outcome)
    }

    fn build(
        origin: FreshDbOutcome,
        historical: bool,
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        let row = match origin {
            FreshDbOutcome::Applied => FreshRowLayout::Present,
            FreshDbOutcome::AlreadySatisfied => FreshRowLayout::JointlyAbsent,
        };
        let fixture = if historical {
            FreshDbInvalidationFixture::historical(source, usr_outcome, candidate_outcome, row)
        } else {
            FreshDbInvalidationFixture::new(source, usr_outcome, candidate_outcome, row)
        };
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
        let authority = match origin {
            FreshDbOutcome::Applied => {
                let authority = fixture.capture_apply(&journal, &reservation);
                let UsrRollbackFreshDbInvalidationApplyReconciliation::Applied(authority) =
                    authority.reconcile(&seal, &journal).unwrap()
                else {
                    panic!("exact fresh row did not reconcile as Applied");
                };
                authority
            }
            FreshDbOutcome::AlreadySatisfied => fixture
                .capture_finish(&journal, &reservation)
                .reconcile(&seal, &journal)
                .unwrap(),
        };
        let expected = fixture.record.rollback_successor(Some(origin.outcome())).unwrap();
        let (reopened, actual) = persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap();
        drop(reopened);
        drop(reservation);
        assert_eq!(actual, expected);
        assert_eq!(actual.phase, Phase::FreshDbInvalidated);
        assert_eq!(fresh_db_invalidation_removal_call_count(), origin.expected_removals());
        fixture.assert_exact_joint_absence();
        Self {
            fixture,
            source: actual,
            origin,
        }
    }

    /// Build an intentionally inexact source: its journal claims invalidation,
    /// but the exact fresh row remains present.
    pub(super) fn with_present_fresh_row(
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        let fixture = FreshDbInvalidationFixture::new(source, usr_outcome, candidate_outcome, FreshRowLayout::Present);
        let source = fixture
            .record
            .rollback_successor(Some(RollbackActionOutcome::Applied))
            .unwrap();
        let journal = fixture.open_journal();
        journal.advance(&fixture.record, &source).unwrap();
        drop(journal);
        fixture.assert_exact_present();
        assert_eq!(fixture.canonical_record(), source);
        Self {
            fixture,
            source,
            origin: FreshDbOutcome::Applied,
        }
    }

    pub(super) fn open_journal(&self) -> TransitionJournalStore {
        self.fixture.open_journal()
    }

    pub(super) fn canonical_record(&self) -> TransitionRecord {
        self.fixture.canonical_record()
    }

    pub(super) fn canonical_bytes(&self) -> Vec<u8> {
        self.fixture.canonical_bytes()
    }

    pub(super) fn database_snapshot(&self) -> DatabaseSnapshot {
        self.fixture.fixture.fixture.database_snapshot()
    }

    pub(super) fn namespace_snapshot(&self) -> Vec<NamespaceEntry> {
        self.fixture.namespace_snapshot()
    }

    pub(super) fn expected_successor(&self) -> TransitionRecord {
        let successor = self.source.rollback_successor(None).unwrap();
        assert_eq!(successor.phase, Phase::RollbackComplete);
        successor
    }

    pub(super) fn capture<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> Result<UsrRollbackCompleteRouteAdmission<'reservation>, UsrRollbackCompleteRouteAuthorityError> {
        capture_record(&self.fixture, journal, reservation, &self.source)
    }

    pub(super) fn capture_ready<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackCompleteRouteAuthority<'reservation> {
        match self.capture(journal, reservation).unwrap() {
            UsrRollbackCompleteRouteAdmission::Ready(authority) => authority,
            _ => panic!("exact FreshDbInvalidated joint-absence evidence did not admit completion"),
        }
    }

    pub(super) fn assert_no_second_removal(&self) {
        self.fixture.assert_exact_joint_absence();
        assert_eq!(
            fresh_db_invalidation_removal_call_count(),
            self.origin.expected_removals()
        );
    }
}

pub(super) fn capture_record<'reservation>(
    fixture: &FreshDbInvalidationFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> Result<UsrRollbackCompleteRouteAdmission<'reservation>, UsrRollbackCompleteRouteAuthorityError> {
    let seal = UsrRollbackCompleteRouteSeal::new_for_test();
    UsrRollbackCompleteRouteAuthority::capture(
        &seal,
        &fixture.fixture.fixture.installation,
        journal,
        &fixture.fixture.fixture.database,
        reservation,
        record,
    )
}

pub(super) use super::super::invalidation_test_support::{
    CandidateOutcome as CandidateResult, CandidateSource as Source,
};

const OS_RELEASE: &[u8] = b"NAME=Rollback Completion Test\nID=rollback-completion-test\n";
const SYSTEM_MODEL: &[u8] = b"let system = { hostname = \"rollback-completion-test\" } in system\n";

pub(super) fn install_persistent_joint_absence_database(fixture: &mut RouteFixture) {
    let database = open_state_database(&fixture.fixture.fixture.fixture.installation);
    let previous = database.add(&[], Some("rollback previous"), None).unwrap().id;
    let candidate = database
        .add_with_transition(
            &fixture.source.transition_id,
            &[],
            Some("rollback fresh candidate"),
            None,
        )
        .unwrap()
        .id;
    assert_eq!(previous, fixture.fixture.fixture.fixture.previous_state);
    assert_eq!(candidate, fixture.fixture.fixture.fixture.candidate_state);
    let provenance = db::state::MetadataProvenance::from_outputs(OS_RELEASE, SYSTEM_MODEL);
    database
        .insert_fresh_metadata_provenance_if_transition_matches(
            candidate,
            &fixture.source.transition_id,
            &provenance,
        )
        .unwrap();
    let observation = database
        .inspect_exact_fresh_transition(candidate, &fixture.source.transition_id)
        .unwrap();
    let db::state::ExactFreshTransitionObservation::Present(preimage) = observation else {
        panic!("persistent completion-route fixture expected one exact fresh preimage");
    };
    database.remove_exact_fresh_transition(preimage).unwrap();
    let old = std::mem::replace(&mut fixture.fixture.fixture.fixture.database, database);
    drop(old);
    fixture.fixture.assert_exact_joint_absence();
}

pub(super) fn release_route_handles(mut fixture: RouteFixture) -> tempfile::TempDir {
    let retained = std::mem::replace(
        &mut fixture.fixture.fixture.fixture._temporary,
        private_installation_tempdir(),
    );
    drop(fixture);
    retained
}

pub(super) struct FreshCompleteRouteHandles {
    pub(super) installation: Installation,
    pub(super) database: db::state::Database,
    pub(super) journal: TransitionJournalStore,
    pub(super) record: TransitionRecord,
}

impl FreshCompleteRouteHandles {
    pub(super) fn open(root: &Path) -> Self {
        let installation = Installation::open(root, None).unwrap();
        let database = open_state_database(&installation);
        let journal = TransitionJournalStore::open_retained(installation.root_directory(), root).unwrap();
        let record = journal
            .load()
            .unwrap()
            .expect("fresh-handle reopen requires one durable completion-route record");
        Self {
            installation,
            database,
            journal,
            record,
        }
    }

    pub(super) fn capture<'reservation>(
        &self,
        reservation: &'reservation ActiveStateReservation,
    ) -> Result<UsrRollbackCompleteRouteAdmission<'reservation>, UsrRollbackCompleteRouteAuthorityError> {
        let seal = UsrRollbackCompleteRouteSeal::new_for_test();
        UsrRollbackCompleteRouteAuthority::capture(
            &seal,
            &self.installation,
            &self.journal,
            &self.database,
            reservation,
            &self.record,
        )
    }

    pub(super) fn capture_ready<'reservation>(
        &self,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackCompleteRouteAuthority<'reservation> {
        match self.capture(reservation).unwrap() {
            UsrRollbackCompleteRouteAdmission::Ready(authority) => authority,
            _ => panic!("fresh exact FreshDbInvalidated joint absence did not admit completion"),
        }
    }
}

fn open_state_database(installation: &Installation) -> db::state::Database {
    let location = installation.mutable_database_location(DatabaseKind::State).unwrap();
    let (url, anchor) = location.parts();
    let database = db::state::Database::new_anchored(url, anchor).unwrap();
    location.revalidate().unwrap();
    installation.revalidate_mutable_namespace().unwrap();
    database
}
