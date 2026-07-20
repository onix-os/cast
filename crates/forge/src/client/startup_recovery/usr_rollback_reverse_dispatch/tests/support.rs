use std::{
    fs,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
};

use crate::{
    Installation,
    client::{
        MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal,
        active_state_snapshot::ActiveStateReservation,
        snapshot_startup_recovery_namespace,
        startup_gate::{self, CleanSystemStartup},
        startup_reconciliation::PendingSystemTransition,
    },
    db,
    installation::DatabaseKind,
    state::TransitionId,
    test_support::private_installation_tempdir,
    transition_journal::{Phase, RecoveryDisposition, RollbackActionOutcome, TransitionRecord},
};

use super::super::UsrRollbackReverseDispatchError;
pub(super) use super::super::test_fixture::create_private_directory;
use super::super::test_fixture::ROOT_ABI;
pub(super) use super::super::reverse_test_support::{
    EffectOperationKind as OperationKind, ReverseFixture as Fixture, ReverseLayout, SourceCase,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct UsrLayout {
    live: (u64, u64),
    staged: (u64, u64),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct RootAbiSnapshot(Vec<RootAbiLinkSnapshot>);

#[derive(Debug, Eq, PartialEq)]
struct RootAbiLinkSnapshot {
    name: &'static str,
    target: PathBuf,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
}

pub(super) fn enter(fixture: &Fixture) -> startup_gate::Error {
    let reservation = ActiveStateReservation::acquire().unwrap();
    match CleanSystemStartup::enter(&fixture.fixture.system, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted an unresolved rollback"),
        Err(error) => error,
    }
}

pub(super) fn pending(error: &startup_gate::Error) -> &PendingSystemTransition {
    match error {
        startup_gate::Error::RecoveryPending(pending) => pending,
        other => panic!("expected recovery-pending startup result, got {other:?}"),
    }
}

pub(super) fn assert_usr_restored_pending(error: &startup_gate::Error) {
    let pending = pending(error);
    assert_eq!(pending.phase(), Phase::UsrRestored);
    assert_eq!(
        pending.disposition(),
        RecoveryDisposition::ResumeRollback {
            phase: Phase::UsrRestored,
        }
    );
    assert!(
        pending.blockers().is_empty(),
        "unexpected blockers: {:?}",
        pending.blockers()
    );
}

pub(super) fn assert_candidate_preserve_intent_pending(error: &startup_gate::Error) {
    let pending = pending(error);
    assert_eq!(pending.phase(), Phase::CandidatePreserveIntent);
    assert_eq!(
        pending.disposition(),
        RecoveryDisposition::ResumeRollback {
            phase: Phase::CandidatePreserveIntent,
        }
    );
    assert!(
        pending.blockers().is_empty(),
        "unexpected blockers: {:?}",
        pending.blockers()
    );
}

pub(super) fn assert_not_applied(error: startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackReverseDispatch(UsrRollbackReverseDispatchError::NotApplied)
        ),
        "expected typed not-applied dispatch error, got {error:?}"
    );
}

pub(super) fn assert_ambiguous(error: startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackReverseDispatch(UsrRollbackReverseDispatchError::Ambiguous)
        ),
        "expected typed ambiguous dispatch error, got {error:?}"
    );
}

pub(super) fn expected_usr_restored(fixture: &Fixture, outcome: RollbackActionOutcome) -> TransitionRecord {
    fixture
        .record
        .rollback_successor(Some(outcome))
        .expect("exact reverse intent must admit its fixed UsrRestored successor")
}

pub(super) fn expected_candidate_preserve_intent(restored: &TransitionRecord) -> TransitionRecord {
    let successor = restored
        .rollback_successor(None)
        .expect("exact UsrRestored record must admit CandidatePreserveIntent");
    assert_eq!(successor.phase, Phase::CandidatePreserveIntent);
    successor
}

pub(super) fn persist_usr_restored_fixture(fixture: &Fixture, outcome: RollbackActionOutcome) -> TransitionRecord {
    let successor = expected_usr_restored(fixture, outcome);
    let journal = fixture.open_journal();
    journal.advance(&fixture.record, &successor).unwrap();
    drop(journal);
    successor
}

pub(super) fn usr_layout(fixture: &Fixture) -> UsrLayout {
    usr_layout_at(&fixture.fixture.installation.root)
}

pub(super) fn usr_layout_at(root: &Path) -> UsrLayout {
    UsrLayout {
        live: directory_identity(&root.join("usr")),
        staged: directory_identity(&root.join(".cast/root/staging/usr")),
    }
}

pub(super) fn assert_layout_reversed(before: UsrLayout, after: UsrLayout) {
    assert_eq!(after.live, before.staged);
    assert_eq!(after.staged, before.live);
}

pub(super) fn assert_layout_unchanged(before: UsrLayout, after: UsrLayout) {
    assert_eq!(after, before);
}

pub(super) fn namespace_snapshot(fixture: &Fixture) -> impl std::fmt::Debug + Eq {
    snapshot_startup_recovery_namespace(&fixture.fixture.installation.root)
}

pub(super) fn root_abi_snapshot_at(root: &Path) -> RootAbiSnapshot {
    RootAbiSnapshot(
        ROOT_ABI
            .into_iter()
            .map(|(name, expected_target)| {
                let path = root.join(name);
                let metadata = fs::symlink_metadata(&path).unwrap();
                assert!(metadata.file_type().is_symlink(), "{} is not a symlink", path.display());
                let target = fs::read_link(&path).unwrap();
                assert_eq!(target, PathBuf::from(expected_target));
                RootAbiLinkSnapshot {
                    name,
                    target,
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    mode: metadata.mode(),
                    links: metadata.nlink(),
                }
            })
            .collect(),
    )
}

pub(super) fn persistent_state_database(fixture: &Fixture, kind: OperationKind) -> db::state::Database {
    let database = open_state_database(&fixture.fixture.installation);
    let (previous, candidate) = seed_state_database(&database, kind, &fixture.record.transition_id);
    assert_eq!(previous, fixture.fixture.previous_state);
    assert_eq!(candidate, fixture.fixture.candidate_state);
    database
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

pub(super) fn test_system_capabilities(
    installation: &Installation,
    state_database: &db::state::Database,
    layout_database: &db::layout::Database,
) -> MutableSystemCapabilities {
    MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation.clone(),
        state_database.clone(),
        layout_database.clone(),
    )
}

pub(super) fn release_fixture_handles(fixture: &mut Fixture) -> tempfile::TempDir {
    let replacement_root = private_installation_tempdir();
    let replacement_installation = Installation::open(replacement_root.path(), None).unwrap();
    let replacement_database = db::state::Database::new(":memory:").unwrap();
    let replacement_layout_database = db::layout::Database::new(":memory:").unwrap();
    let replacement_system = test_system_capabilities(
        &replacement_installation,
        &replacement_database,
        &replacement_layout_database,
    );
    let retained_system = std::mem::replace(&mut fixture.fixture.system, replacement_system);
    let retained_installation = std::mem::replace(&mut fixture.fixture.installation, replacement_installation);
    let retained_database = std::mem::replace(&mut fixture.fixture.database, replacement_database);
    drop(retained_system);
    drop(retained_database);
    drop(retained_installation);
    replacement_root
}

fn seed_state_database(
    database: &db::state::Database,
    kind: OperationKind,
    transition: &TransitionId,
) -> (crate::state::Id, crate::state::Id) {
    match kind {
        OperationKind::NewState => {
            let previous = database.add(&[], Some("rollback previous"), None).unwrap().id;
            let candidate = add_state_with_provenance(database, transition, "rollback fresh candidate", false);
            (previous, candidate)
        }
        OperationKind::Archived => {
            let previous = database.add(&[], Some("rollback previous"), None).unwrap().id;
            let candidate_transition = TransitionId::parse("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee").unwrap();
            let candidate =
                add_state_with_provenance(database, &candidate_transition, "rollback archived candidate", true);
            (previous, candidate)
        }
        OperationKind::ActiveReblit => {
            let candidate_transition = TransitionId::parse("dddddddddddddddddddddddddddddddd").unwrap();
            let state = add_state_with_provenance(database, &candidate_transition, "rollback active reblit", true);
            (state, state)
        }
    }
}

fn add_state_with_provenance(
    database: &db::state::Database,
    transition: &TransitionId,
    summary: &str,
    clear_transition: bool,
) -> crate::state::Id {
    let state = database
        .add_with_transition(transition, &[], Some(summary), None)
        .unwrap();
    let provenance = db::state::MetadataProvenance::from_outputs(
        b"NAME=Rollback Decision Test\nID=rollback-decision-test\n",
        b"let system = { hostname = \"rollback-decision-test\" } in system\n",
    );
    database
        .insert_fresh_metadata_provenance_if_transition_matches(state.id, transition, &provenance)
        .unwrap();
    if clear_transition {
        database.clear_transition_if_matches(state.id, transition).unwrap();
    }
    state.id
}

fn directory_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_dir(), "{} is not a directory", path.display());
    (metadata.dev(), metadata.ino())
}
