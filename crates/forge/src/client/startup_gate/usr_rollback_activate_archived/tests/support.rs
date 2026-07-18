use std::{
    fs,
    os::unix::fs::{MetadataExt as _, symlink},
    path::{Path, PathBuf},
};

use crate::{
    Installation,
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{
            self, CleanSystemStartup, UsrRollbackActivateArchivedCompleteRouteSeal,
            UsrRollbackActivateArchivedFinalizationSeal,
        },
        startup_reconciliation::{
            UsrRollbackActivateArchivedCompleteRouteAdmission, UsrRollbackActivateArchivedCompleteRouteAuthority,
            UsrRollbackActivateArchivedCompleteRouteAuthorityError, UsrRollbackActivateArchivedFinalizationAdmission,
            UsrRollbackActivateArchivedFinalizationAuthority, archived_candidate_preserve_move_attempt_count,
            reset_archived_candidate_preserve_move_attempt_count,
        },
        startup_recovery::{
            DurableUsrRollbackActivateArchivedCompleteRouteRecord, DurableUsrRollbackArchivedCandidatePreserveRecord,
            UsrRollbackActivateArchivedCompleteRoutePersistenceError,
            UsrRollbackArchivedCandidatePreservePersistenceError, UsrRollbackCandidatePreserveDispatchError,
        },
    },
    db,
    installation::DatabaseKind,
    state::TransitionId,
    test_support::private_installation_tempdir,
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord, decode},
};

pub(super) use super::super::candidate_test_support::CandidateSource;
use super::super::{
    candidate_test_support::{CandidateLayout, CandidatePreserveFixture, archived_slot_path},
    test_fixture::{DatabaseSnapshot, NamespaceEntry, OperationKind},
};

const OS_RELEASE: &[u8] = b"NAME=Rollback Decision Test\nID=rollback-decision-test\n";
const SYSTEM_MODEL: &[u8] = b"let system = { hostname = \"rollback-decision-test\" } in system\n";
const ROOT_ABI: [(&str, &str); 5] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib32", "usr/lib32"),
    ("lib64", "usr/lib"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Epoch {
    Current,
    Historical,
}

impl Epoch {
    pub(super) const ALL: [Self; 2] = [Self::Current, Self::Historical];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateOutcome {
    Applied,
    AlreadySatisfied,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateOrigin {
    Applied,
    AlreadySatisfied,
}

impl CandidateOrigin {
    pub(super) const ALL: [Self; 2] = [Self::Applied, Self::AlreadySatisfied];

    pub(super) fn outcome(self) -> RollbackActionOutcome {
        match self {
            Self::Applied => RollbackActionOutcome::Applied,
            Self::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        }
    }

    fn layout(self) -> CandidateLayout {
        match self {
            Self::Applied => CandidateLayout::Staged,
            Self::AlreadySatisfied => CandidateLayout::Preserved,
        }
    }
}

pub(super) fn build_candidate(
    epoch: Epoch,
    source: CandidateSource,
    usr_outcome: RollbackActionOutcome,
    origin: CandidateOrigin,
) -> CandidatePreserveFixture {
    match epoch {
        Epoch::Current => CandidatePreserveFixture::new(OperationKind::Archived, source, usr_outcome, origin.layout()),
        Epoch::Historical => {
            CandidatePreserveFixture::historical(OperationKind::Archived, source, usr_outcome, origin.layout())
        }
    }
}

pub(super) fn expected_candidate_preserved(
    fixture: &CandidatePreserveFixture,
    origin: CandidateOrigin,
) -> TransitionRecord {
    let successor = fixture
        .candidate_intent
        .rollback_successor(Some(origin.outcome()))
        .unwrap();
    assert_eq!(successor.phase, Phase::CandidatePreserved);
    successor
}

pub(super) fn enter_candidate(fixture: &CandidatePreserveFixture) -> startup_gate::Error {
    let reservation = ActiveStateReservation::acquire().unwrap();
    match CleanSystemStartup::enter(&fixture.fixture.installation, &fixture.fixture.database, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted unresolved ActivateArchived evidence"),
        Err(error) => error,
    }
}

pub(super) fn reset_candidate_observers() {
    reset_archived_candidate_preserve_move_attempt_count();
}

pub(super) fn assert_persistence_advance(
    error: &startup_gate::Error,
    expected: DurableUsrRollbackArchivedCandidatePreserveRecord,
) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackActivateArchivedDispatch(
                super::super::Error::CandidatePreserveDispatch(
                    UsrRollbackCandidatePreserveDispatchError::ArchivedPersistence(
                        UsrRollbackArchivedCandidatePreservePersistenceError::Advance { durable, .. }
                    )
                )
            ) if *durable == expected
        ),
        "expected durable {expected:?} ActivateArchived advance failure, got {error:?}"
    );
}

pub(super) fn assert_candidate_persistence_authority_error(error: &startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackActivateArchivedDispatch(super::super::Error::CandidatePreserveDispatch(
                UsrRollbackCandidatePreserveDispatchError::ArchivedPersistence(
                    UsrRollbackArchivedCandidatePreservePersistenceError::Authority(_)
                )
            ))
        ),
        "expected ActivateArchived persistence authority error, got {error:?}"
    );
}

pub(super) fn assert_complete_persistence_advance(
    error: &startup_gate::Error,
    expected: DurableUsrRollbackActivateArchivedCompleteRouteRecord,
) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackActivateArchivedDispatch(
                super::super::Error::CompleteRoutePersistence(
                    UsrRollbackActivateArchivedCompleteRoutePersistenceError::Advance { durable, .. }
                )
            ) if *durable == expected
        ),
        "expected durable {expected:?} ActivateArchived completion-route advance failure, got {error:?}"
    );
}

pub(super) fn assert_complete_persistence_authority_error(error: &startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackActivateArchivedDispatch(super::super::Error::CompleteRoutePersistence(
                UsrRollbackActivateArchivedCompleteRoutePersistenceError::Authority(_)
            ))
        ),
        "expected exact ActivateArchived completion-route persistence authority error, got {error:?}"
    );
}

pub(super) fn candidate_move_count() -> usize {
    archived_candidate_preserve_move_attempt_count()
}

pub(super) fn assert_candidate_pending_audit(
    error: &startup_gate::Error,
    fixture: &CandidatePreserveFixture,
    expected: &TransitionRecord,
) {
    let startup_gate::Error::RecoveryPending(pending) = error else {
        panic!("expected exact CandidatePreserved recovery-pending result, got {error:?}");
    };
    assert_eq!(pending.transition_id(), &expected.transition_id);
    assert_eq!(pending.phase(), Phase::CandidatePreserved);
    assert_eq!(pending.disposition(), expected.recovery_disposition());
    assert!(
        pending.blockers().is_empty(),
        "unexpected pending blockers: {:?}",
        pending.blockers()
    );
    assert!(pending.retains_database(&fixture.fixture.database));
}

pub(super) fn assert_candidate_preserved_topology(fixture: &CandidatePreserveFixture, record: &TransitionRecord) {
    let target = fixture
        .fixture
        .installation
        .root
        .join(".cast/root")
        .join(fixture.fixture.candidate_state.to_string());
    let slot = archived_slot_path(&fixture.fixture, record);
    assert!(target.join("usr").is_dir());
    assert!(slot.is_file());
    assert_eq!(fs::symlink_metadata(slot).unwrap().nlink(), 2);
    assert!(!fixture.fixture.installation.staging_dir().join("usr").exists());
    assert_eq!(
        fs::read_to_string(fixture.fixture.installation.root.join("usr/.stateID")).unwrap(),
        fixture.fixture.previous_state.to_string(),
    );
}

pub(super) fn install_persistent_candidate_database(fixture: &mut CandidatePreserveFixture) {
    let database = open_state_database(&fixture.fixture.installation);
    let previous = database.add(&[], Some("rollback previous"), None).unwrap().id;
    assert_eq!(previous, fixture.fixture.previous_state);
    let transition = TransitionId::parse("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee").unwrap();
    let candidate = database
        .add_with_transition(&transition, &[], Some("rollback archived candidate"), None)
        .unwrap()
        .id;
    assert_eq!(candidate, fixture.fixture.candidate_state);
    let provenance = db::state::MetadataProvenance::from_outputs(OS_RELEASE, SYSTEM_MODEL);
    database
        .insert_fresh_metadata_provenance_if_transition_matches(candidate, &transition, &provenance)
        .unwrap();
    database.clear_transition_if_matches(candidate, &transition).unwrap();
    let old = std::mem::replace(&mut fixture.fixture.database, database);
    drop(old);
}

pub(super) fn release_candidate_handles(mut fixture: CandidatePreserveFixture) -> tempfile::TempDir {
    let retained = std::mem::replace(&mut fixture.fixture._temporary, private_installation_tempdir());
    drop(fixture);
    retained
}

pub(super) fn enter_candidate_with_fresh_handles(root: &Path) -> startup_gate::Error {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    let reservation = ActiveStateReservation::acquire().unwrap();
    match CleanSystemStartup::enter(&installation, &database, &reservation) {
        Ok(_) => panic!("fresh startup unexpectedly admitted unresolved ActivateArchived evidence"),
        Err(error) => error,
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

fn install_live_root_abi(installation: &Installation) {
    for (name, target) in ROOT_ABI {
        symlink(target, installation.root.join(name)).unwrap();
    }
}

impl CandidateOutcome {
    pub(super) const ALL: [Self; 2] = [Self::Applied, Self::AlreadySatisfied];

    pub(super) fn outcome(self) -> RollbackActionOutcome {
        match self {
            Self::Applied => RollbackActionOutcome::Applied,
            Self::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        }
    }
}

pub(super) struct RouteFixture {
    pub(super) fixture: CandidatePreserveFixture,
    pub(super) source: TransitionRecord,
}

impl RouteFixture {
    pub(super) fn new(
        epoch: Epoch,
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        let fixture = match epoch {
            Epoch::Current => {
                CandidatePreserveFixture::new(OperationKind::Archived, source, usr_outcome, CandidateLayout::Preserved)
            }
            Epoch::Historical => CandidatePreserveFixture::historical(
                OperationKind::Archived,
                source,
                usr_outcome,
                CandidateLayout::Preserved,
            ),
        };
        install_live_root_abi(&fixture.fixture.installation);
        let source = fixture
            .candidate_intent
            .rollback_successor(Some(candidate_outcome.outcome()))
            .unwrap();
        assert_eq!(source.phase, Phase::CandidatePreserved);
        let journal = fixture.open_journal();
        journal.advance(&fixture.candidate_intent, &source).unwrap();
        drop(journal);
        assert_eq!(fixture.fixture.canonical_record(), source);
        let route = Self { fixture, source };
        route.assert_exact_database_pair();
        route.assert_exact_archived_topology();
        route
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

    pub(super) fn database_snapshot(&self) -> DatabaseSnapshot {
        self.fixture.fixture.database_snapshot()
    }

    pub(super) fn namespace_snapshot(&self) -> Vec<NamespaceEntry> {
        self.fixture.fixture.namespace_snapshot()
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
    ) -> Result<
        UsrRollbackActivateArchivedCompleteRouteAdmission<'reservation>,
        UsrRollbackActivateArchivedCompleteRouteAuthorityError,
    > {
        capture_record(self, journal, reservation, &self.source)
    }

    pub(super) fn capture_ready<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackActivateArchivedCompleteRouteAuthority<'reservation> {
        match self.capture(journal, reservation).unwrap() {
            UsrRollbackActivateArchivedCompleteRouteAdmission::Ready(authority) => authority,
            _ => panic!("exact preserved ActivateArchived evidence did not admit completion routing"),
        }
    }

    pub(super) fn archived_wrapper_path(&self) -> PathBuf {
        self.fixture
            .fixture
            .installation
            .root
            .join(".cast/root")
            .join(self.fixture.fixture.candidate_state.to_string())
    }

    pub(super) fn archived_slot_path(&self) -> PathBuf {
        archived_slot_path(&self.fixture.fixture, &self.source)
    }

    pub(super) fn transition_quarantine_path(&self) -> PathBuf {
        self.fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join(self.source.quarantine_name.as_str())
    }

    pub(super) fn assert_exact_database_pair(&self) {
        let database = &self.fixture.fixture.database;
        let candidate = self.fixture.fixture.candidate_state;
        let previous = self.fixture.fixture.previous_state;
        assert_ne!(candidate, previous);
        assert_eq!(database.all().unwrap().len(), 2);
        assert_eq!(database.get(candidate).unwrap().id, candidate);
        assert_eq!(database.get(previous).unwrap().id, previous);
        assert_eq!(database.audit_in_flight_transition().unwrap(), None);
        assert_eq!(
            database
                .transition_ownership(candidate, &self.source.transition_id)
                .unwrap(),
            db::state::TransitionOwnership::Cleared
        );
        assert_eq!(
            database
                .transition_ownership(previous, &self.source.transition_id)
                .unwrap(),
            db::state::TransitionOwnership::Cleared
        );
        assert!(database.metadata_provenance(candidate).unwrap().is_some());
    }

    pub(super) fn assert_exact_archived_topology(&self) {
        let wrapper = self.archived_wrapper_path();
        let slot = self.archived_slot_path();
        let root = &self.fixture.fixture.installation.root;
        assert!(wrapper.join("usr").is_dir());
        assert!(slot.is_file());
        assert_eq!(fs::symlink_metadata(slot).unwrap().nlink(), 2);
        assert!(
            fs::read_dir(self.fixture.fixture.installation.staging_dir())
                .unwrap()
                .next()
                .is_none()
        );
        assert!(!self.transition_quarantine_path().exists());
        assert_eq!(
            fs::read_to_string(root.join("usr/.stateID")).unwrap(),
            self.fixture.fixture.previous_state.to_string()
        );
    }
}

pub(super) fn enter_route(fixture: &RouteFixture) -> startup_gate::Error {
    enter_candidate(&fixture.fixture)
}

pub(super) fn assert_route_pending_audit(
    error: &startup_gate::Error,
    fixture: &RouteFixture,
    expected: &TransitionRecord,
) {
    let startup_gate::Error::RecoveryPending(pending) = error else {
        panic!("expected exact RollbackComplete recovery-pending result, got {error:?}");
    };
    assert_eq!(pending.transition_id(), &expected.transition_id);
    assert_eq!(pending.phase(), Phase::RollbackComplete);
    assert_eq!(pending.disposition(), expected.recovery_disposition());
    assert!(pending.retains_database(&fixture.fixture.fixture.database));
}

pub(super) fn install_persistent_route_database(fixture: &mut RouteFixture) {
    install_persistent_candidate_database(&mut fixture.fixture);
}

pub(super) fn release_route_handles(fixture: RouteFixture) -> tempfile::TempDir {
    release_candidate_handles(fixture.fixture)
}

pub(super) fn persist_rollback_complete(fixture: &RouteFixture) -> TransitionRecord {
    let terminal = fixture.expected_successor();
    let journal = fixture.open_journal();
    journal.advance(&fixture.source, &terminal).unwrap();
    drop(journal);
    assert_eq!(fixture.canonical_record(), terminal);
    terminal
}

pub(super) fn capture_finalization_ready<'reservation>(
    fixture: &RouteFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> UsrRollbackActivateArchivedFinalizationAuthority<'reservation> {
    let seal = UsrRollbackActivateArchivedFinalizationSeal::new_for_test();
    let admission = UsrRollbackActivateArchivedFinalizationAuthority::capture(
        &seal,
        &fixture.fixture.fixture.installation,
        journal,
        &fixture.fixture.fixture.database,
        reservation,
        record,
    )
    .unwrap();
    let UsrRollbackActivateArchivedFinalizationAdmission::Ready(authority) = admission else {
        panic!("exact terminal ActivateArchived evidence did not admit finalization");
    };
    authority
}

pub(super) fn enter_clean_route(fixture: &RouteFixture) -> CleanSystemStartup {
    let reservation = ActiveStateReservation::acquire().unwrap();
    CleanSystemStartup::enter(
        &fixture.fixture.fixture.installation,
        &fixture.fixture.fixture.database,
        &reservation,
    )
    .expect("exact terminal ActivateArchived evidence did not admit clean startup")
}

pub(super) fn enter_clean_fresh_handles(root: &Path) -> CleanSystemStartup {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    let reservation = ActiveStateReservation::acquire().unwrap();
    CleanSystemStartup::enter(&installation, &database, &reservation)
        .expect("fresh handles did not finalize exact terminal ActivateArchived evidence")
}

pub(super) fn assert_canonical_absent(root: &Path) {
    assert!(!root.join(".cast/journal/state-transition").exists());
}

pub(super) fn assert_finalization_dispatch_error(error: &startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackActivateArchivedDispatch(super::super::Error::RollbackFinalization(_))
        ),
        "expected exact ActivateArchived rollback-finalization dispatch error, got {error:?}"
    );
}

pub(super) fn assert_fresh_exact_database_pair(
    root: &Path,
    record: &TransitionRecord,
    expected_provenance: &db::state::MetadataProvenance,
) {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    let candidate = crate::state::Id::from(record.candidate.id.unwrap());
    let previous = crate::state::Id::from(record.previous.id.unwrap());
    assert_ne!(candidate, previous);
    assert_eq!(database.all().unwrap().len(), 2);
    assert_eq!(database.get(candidate).unwrap().id, candidate);
    assert_eq!(database.get(previous).unwrap().id, previous);
    assert_eq!(database.audit_in_flight_transition().unwrap(), None);
    assert_eq!(
        database.transition_ownership(candidate, &record.transition_id).unwrap(),
        db::state::TransitionOwnership::Cleared
    );
    assert_eq!(
        database.transition_ownership(previous, &record.transition_id).unwrap(),
        db::state::TransitionOwnership::Cleared
    );
    assert_eq!(
        database.metadata_provenance(candidate).unwrap().as_ref(),
        Some(expected_provenance)
    );
}

pub(super) fn capture_record<'reservation>(
    fixture: &RouteFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> Result<
    UsrRollbackActivateArchivedCompleteRouteAdmission<'reservation>,
    UsrRollbackActivateArchivedCompleteRouteAuthorityError,
> {
    let seal = UsrRollbackActivateArchivedCompleteRouteSeal::new_for_test();
    UsrRollbackActivateArchivedCompleteRouteAuthority::capture(
        &seal,
        &fixture.fixture.fixture.installation,
        journal,
        &fixture.fixture.fixture.database,
        reservation,
        record,
    )
}

pub(super) fn assert_pending_phase(error: &startup_gate::Error, expected: Phase) {
    match error {
        startup_gate::Error::RecoveryPending(pending) => assert_eq!(pending.phase(), expected),
        other => panic!("expected {expected:?} recovery-pending result, got {other:?}"),
    }
}

pub(super) fn canonical_record_from_root(root: &Path) -> TransitionRecord {
    decode(&fs::read(root.join(".cast/journal/state-transition")).unwrap()).unwrap()
}
