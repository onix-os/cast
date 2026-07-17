use std::{
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

use crate::{
    Installation,
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{self, CleanSystemStartup},
        startup_recovery::{
            DurableUsrRollbackActiveReblitCandidatePreserveRecord,
            UsrRollbackActiveReblitCandidatePreservePersistenceError, UsrRollbackCandidatePreserveDispatchError,
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
    fixture.with_active_reblit_wrapper_index(WRAPPER_INDEX)
}

pub(super) fn build_other(
    kind: OperationKind,
    source: CandidateSource,
    layout: CandidateLayout,
) -> CandidatePreserveFixture {
    assert_ne!(kind, OperationKind::ActiveReblit);
    let fixture = CandidatePreserveFixture::new(kind, source, RollbackActionOutcome::Applied, layout);
    if kind == OperationKind::NewState && source == CandidateSource::Exchanged {
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

pub(super) fn enter(installation: &Installation, database: &db::state::Database) -> startup_gate::Error {
    let reservation = ActiveStateReservation::acquire().unwrap();
    match CleanSystemStartup::enter(installation, database, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted an unresolved transition"),
        Err(error) => error,
    }
}

pub(super) fn enter_candidate(fixture: &CandidatePreserveFixture) -> startup_gate::Error {
    enter(&fixture.fixture.installation, &fixture.fixture.database)
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

pub(super) fn canonical_record(root: &Path) -> TransitionRecord {
    decode(&fs::read(root.join(".cast/journal/state-transition")).unwrap()).unwrap()
}

pub(super) fn active_wrapper_path(fixture: &CandidatePreserveFixture) -> PathBuf {
    active_reblit_wrapper_path(&fixture.fixture, &fixture.candidate_intent, WRAPPER_INDEX)
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
    enter(&installation, &database)
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
