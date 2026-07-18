use std::{
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

use crate::{
    Installation,
    client::{
        MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal,
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{self, CleanSystemStartup, UsrRollbackActiveReblitFinalizationSeal},
        startup_reconciliation::{
            UsrRollbackActiveReblitFinalizationAdmission, UsrRollbackActiveReblitFinalizationAuthority,
            active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_post_exchange_durability_events,
            take_active_reblit_candidate_preserve_post_exchange_durability_events,
        },
        startup_recovery::{
            DurableUsrRollbackActiveReblitCandidatePreserveRecord, DurableUsrRollbackActiveReblitCompleteRouteRecord,
            UsrRollbackActiveReblitCandidatePreservePersistenceError,
            UsrRollbackActiveReblitCompleteRoutePersistenceError, UsrRollbackCandidatePreserveDispatchError,
        },
    },
    db,
    installation::DatabaseKind,
    test_support::private_installation_tempdir,
    transition_journal::{Phase, RollbackActionOutcome, TransitionRecord, decode},
};

use super::super::{
    Error as ActiveReblitDispatchError,
    candidate_test_support::{CandidateLayout, CandidatePreserveFixture, CandidateSource, active_reblit_wrapper_path},
    test_fixture::OperationKind,
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

pub(super) const WRAPPER_INDEX: usize = 13;
pub(super) const WRAPPER_INDICES: [usize; 2] = [0, WRAPPER_INDEX];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Epoch {
    Current,
    Historical,
}

impl Epoch {
    pub(super) const ALL: [Self; 2] = [Self::Current, Self::Historical];
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

pub(super) fn build_active(
    epoch: Epoch,
    source: CandidateSource,
    usr_outcome: RollbackActionOutcome,
    origin: CandidateOrigin,
) -> CandidatePreserveFixture {
    build_active_at_wrapper_index(epoch, source, usr_outcome, origin, WRAPPER_INDEX)
}

pub(super) fn build_active_at_wrapper_index(
    epoch: Epoch,
    source: CandidateSource,
    usr_outcome: RollbackActionOutcome,
    origin: CandidateOrigin,
    wrapper_index: usize,
) -> CandidatePreserveFixture {
    let fixture = match epoch {
        Epoch::Current => {
            CandidatePreserveFixture::new(OperationKind::ActiveReblit, source, usr_outcome, origin.layout())
        }
        Epoch::Historical => {
            CandidatePreserveFixture::historical(OperationKind::ActiveReblit, source, usr_outcome, origin.layout())
        }
    };
    if source == CandidateSource::Exchanged {
        install_live_root_abi(&fixture.fixture.installation);
    }
    fixture.with_active_reblit_wrapper_index(wrapper_index)
}

pub(super) fn build_other(
    kind: OperationKind,
    source: CandidateSource,
    layout: CandidateLayout,
) -> CandidatePreserveFixture {
    assert_ne!(kind, OperationKind::ActiveReblit);
    let fixture = CandidatePreserveFixture::new(kind, source, RollbackActionOutcome::Applied, layout);
    if kind == OperationKind::Archived || (kind == OperationKind::NewState && source == CandidateSource::Exchanged) {
        install_live_root_abi(&fixture.fixture.installation);
    }
    fixture
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

pub(super) fn expected_rollback_complete(candidate_preserved: &TransitionRecord) -> TransitionRecord {
    let successor = candidate_preserved.rollback_successor(None).unwrap();
    assert_eq!(successor.phase, Phase::RollbackComplete);
    successor
}

pub(super) fn persist_candidate_preserved(
    fixture: &CandidatePreserveFixture,
    origin: CandidateOrigin,
) -> TransitionRecord {
    let successor = expected_candidate_preserved(fixture, origin);
    let journal = fixture.open_journal();
    journal.advance(&fixture.candidate_intent, &successor).unwrap();
    drop(journal);
    successor
}

pub(super) fn persist_rollback_complete(
    fixture: &CandidatePreserveFixture,
    origin: CandidateOrigin,
) -> TransitionRecord {
    let preserved = persist_candidate_preserved(fixture, origin);
    let complete = expected_rollback_complete(&preserved);
    let journal = fixture.open_journal();
    journal.advance(&preserved, &complete).unwrap();
    drop(journal);
    complete
}

pub(super) fn capture_finalization_ready<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &crate::transition_journal::TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> UsrRollbackActiveReblitFinalizationAuthority<'reservation> {
    let seal = UsrRollbackActiveReblitFinalizationSeal::new_for_test();
    let admission = UsrRollbackActiveReblitFinalizationAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        journal,
        &fixture.fixture.database,
        reservation,
        record,
    )
    .unwrap();
    let UsrRollbackActiveReblitFinalizationAdmission::Ready(authority) = admission else {
        panic!("exact terminal ActiveReblit evidence did not admit finalization");
    };
    authority
}

pub(super) fn reset_candidate_effect_observers() {
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    reset_active_reblit_candidate_preserve_post_exchange_durability_events();
}

pub(super) fn assert_no_candidate_effects() {
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    assert!(
        take_active_reblit_candidate_preserve_post_exchange_durability_events().is_empty(),
        "completion routing must not repeat candidate-preservation durability"
    );
}

pub(super) fn enter(system: &MutableSystemCapabilities) -> startup_gate::Error {
    let reservation = ActiveStateReservation::acquire().unwrap();
    match CleanSystemStartup::enter(system, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted an unresolved transition"),
        Err(error) => error,
    }
}

pub(super) fn enter_candidate(fixture: &CandidatePreserveFixture) -> startup_gate::Error {
    enter(&fixture.fixture.system)
}

pub(super) fn enter_clean_candidate(fixture: &CandidatePreserveFixture) -> CleanSystemStartup {
    let reservation = ActiveStateReservation::acquire().unwrap();
    CleanSystemStartup::enter(&fixture.fixture.system, &reservation)
        .expect("exact terminal ActiveReblit evidence did not admit clean startup")
}

pub(super) fn enter_clean_fresh_handles(root: &Path) -> CleanSystemStartup {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    let layout_database = open_layout_database(&installation);
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation,
        database,
        layout_database,
    );
    let reservation = ActiveStateReservation::acquire().unwrap();
    CleanSystemStartup::enter(&system, &reservation)
        .expect("fresh handles did not finalize exact terminal ActiveReblit evidence")
}

pub(super) fn assert_canonical_absent(root: &Path) {
    assert!(!root.join(".cast/journal/state-transition").exists());
}

pub(super) fn assert_pending_phase(error: &startup_gate::Error, phase: Phase) {
    match error {
        startup_gate::Error::RecoveryPending(pending) => assert_eq!(pending.phase(), phase),
        other => panic!("expected {phase:?} recovery-pending result, got {other:?}"),
    }
}

pub(super) fn assert_active_authority_dispatch_error(error: &startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackActiveReblitDispatch(ActiveReblitDispatchError::CandidatePreserveDispatch(
                UsrRollbackCandidatePreserveDispatchError::Authority(_)
            ))
        ),
        "expected exact ActiveReblit candidate-preservation authority error, got {error:?}"
    );
}

pub(super) fn assert_active_persistence_authority_error(error: &startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackActiveReblitDispatch(ActiveReblitDispatchError::CandidatePreserveDispatch(
                UsrRollbackCandidatePreserveDispatchError::ActiveReblitPersistence(
                    UsrRollbackActiveReblitCandidatePreservePersistenceError::Authority(_)
                )
            ))
        ),
        "expected exact ActiveReblit persistence-authority error, got {error:?}"
    );
}

pub(super) fn assert_not_applied(error: startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackActiveReblitDispatch(ActiveReblitDispatchError::CandidatePreserveDispatch(
                UsrRollbackCandidatePreserveDispatchError::NotApplied
            ))
        ),
        "expected ActiveReblit candidate NotApplied, got {error:?}"
    );
}

pub(super) fn assert_persistence_advance(
    error: &startup_gate::Error,
    expected: DurableUsrRollbackActiveReblitCandidatePreserveRecord,
) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackActiveReblitDispatch(
                ActiveReblitDispatchError::CandidatePreserveDispatch(
                    UsrRollbackCandidatePreserveDispatchError::ActiveReblitPersistence(
                        UsrRollbackActiveReblitCandidatePreservePersistenceError::Advance {
                            durable,
                            ..
                        }
                    )
                )
            ) if *durable == expected
        ),
        "expected durable {expected:?} ActiveReblit advance failure, got {error:?}"
    );
}

pub(super) fn assert_complete_persistence_advance(
    error: &startup_gate::Error,
    expected: DurableUsrRollbackActiveReblitCompleteRouteRecord,
) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackActiveReblitDispatch(
                ActiveReblitDispatchError::CompleteRoutePersistence(
                    UsrRollbackActiveReblitCompleteRoutePersistenceError::Advance {
                        durable,
                        ..
                    }
                )
            ) if *durable == expected
        ),
        "expected durable {expected:?} ActiveReblit completion-route advance failure, got {error:?}"
    );
}

pub(super) fn assert_complete_persistence_authority_error(error: &startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackActiveReblitDispatch(ActiveReblitDispatchError::CompleteRoutePersistence(
                UsrRollbackActiveReblitCompleteRoutePersistenceError::Authority(_)
            ))
        ),
        "expected exact ActiveReblit completion-route persistence authority error, got {error:?}"
    );
}

pub(super) fn canonical_record(root: &Path) -> TransitionRecord {
    decode(&fs::read(root.join(".cast/journal/state-transition")).unwrap()).unwrap()
}

pub(super) fn active_wrapper_path(fixture: &CandidatePreserveFixture) -> PathBuf {
    active_reblit_wrapper_path(&fixture.fixture, &fixture.candidate_intent, WRAPPER_INDEX)
}

pub(super) fn active_wrapper_path_at(fixture: &CandidatePreserveFixture, wrapper_index: usize) -> PathBuf {
    active_reblit_wrapper_path(&fixture.fixture, &fixture.candidate_intent, wrapper_index)
}

pub(super) fn install_persistent_database(fixture: &mut CandidatePreserveFixture) {
    let database = open_state_database(&fixture.fixture.installation);
    let transition = &fixture.candidate_intent.transition_id;
    let candidate = database
        .add_with_transition(transition, &[], Some("rollback active reblit"), None)
        .unwrap()
        .id;
    assert_eq!(candidate, fixture.fixture.candidate_state);
    assert_eq!(candidate, fixture.fixture.previous_state);
    let provenance = db::state::MetadataProvenance::from_outputs(OS_RELEASE, SYSTEM_MODEL);
    database
        .insert_fresh_metadata_provenance_if_transition_matches(candidate, transition, &provenance)
        .unwrap();
    database.clear_transition_if_matches(candidate, transition).unwrap();
    let old = std::mem::replace(&mut fixture.fixture.database, database);
    drop(old);
}

pub(super) fn release_candidate_handles(mut fixture: CandidatePreserveFixture) -> tempfile::TempDir {
    let retained = std::mem::replace(&mut fixture.fixture._temporary, private_installation_tempdir());
    drop(fixture);
    retained
}

pub(super) fn enter_fresh_handles(root: &Path) -> startup_gate::Error {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    let layout_database = open_layout_database(&installation);
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation,
        database,
        layout_database,
    );
    enter(&system)
}

pub(super) fn assert_fresh_existing_candidate_database(
    root: &Path,
    record: &TransitionRecord,
    expected_provenance: &db::state::MetadataProvenance,
) {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    let candidate = crate::state::Id::from(record.candidate.id.unwrap());
    assert_eq!(record.candidate.id, record.previous.id);
    assert_eq!(database.get(candidate).unwrap().id, candidate);
    assert_eq!(database.all().unwrap().len(), 1);
    assert_eq!(database.audit_in_flight_transition().unwrap(), None);
    assert_eq!(
        database.transition_ownership(candidate, &record.transition_id).unwrap(),
        db::state::TransitionOwnership::Cleared
    );
    assert_eq!(
        database.metadata_provenance(candidate).unwrap().as_ref(),
        Some(expected_provenance)
    );
}

pub(super) fn open_state_database(installation: &Installation) -> db::state::Database {
    let location = installation.mutable_database_location(DatabaseKind::State).unwrap();
    let (url, anchor) = location.parts();
    let database = db::state::Database::new_anchored(url, anchor).unwrap();
    location.revalidate().unwrap();
    installation.revalidate_mutable_namespace().unwrap();
    database
}

pub(super) fn open_layout_database(installation: &Installation) -> db::layout::Database {
    let location = installation.mutable_database_location(DatabaseKind::Layout).unwrap();
    let (url, anchor) = location.parts();
    let database = db::layout::Database::new_anchored(url, anchor).unwrap();
    location.revalidate().unwrap();
    installation.revalidate_mutable_namespace().unwrap();
    database
}

fn install_live_root_abi(installation: &Installation) {
    for (name, target) in ROOT_ABI {
        symlink(target, installation.root.join(name)).unwrap();
    }
}
