use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use crate::{
    Installation, db,
    state::{self, TransitionId},
    test_support::private_installation_tempdir,
    transition_journal::{
        CandidateOrigin, Operation, Phase, PreviousOrigin, RuntimeEpoch, RuntimeTreeIdentity, TransitionJournalStore,
        TransitionRecord, decode,
    },
};

use super::*;
use crate::db::state::TransitionOwnership;
use crate::transition_identity::StatefulTreeIdentity;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateKind {
    NewState,
    Archived,
    ActiveReblit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreviousKind {
    Active,
    SynthesizedEmpty,
}

const NEW_STATE_PAYLOAD_SENTINEL: &[u8] = b"state-ID-unallocated payload";
const COORDINATOR_SYSTEM_SNAPSHOT: &[u8] = b"let system = { hostname = \"coordinator-test\" } in system\n";
const COORDINATOR_OS_RELEASE: &[u8] = b"NAME=\"Coordinator Test OS\"\nID=coordinator-test\n";

struct CoordinatorFixture {
    _temporary: tempfile::TempDir,
    installation: Installation,
    database: db::state::Database,
    previous_state: state::Id,
    candidate_state: state::Id,
    candidate_path: PathBuf,
}

fn fixture(candidate_kind: CandidateKind, previous_kind: PreviousKind) -> (CoordinatorFixture, StatefulTreeIdentity) {
    let temporary = private_installation_tempdir();
    let mut installation = Installation::open(temporary.path(), None).unwrap();
    let database = db::state::Database::new(":memory:").unwrap();
    let previous_state = database.add(&[], Some("coordinator previous"), None).unwrap().id;
    let candidate_state = match candidate_kind {
        // The row is intentionally absent. SQLite will allocate this exact
        // next ID only after FreshStateAllocating is durable.
        CandidateKind::NewState => previous_state.next(),
        CandidateKind::Archived => {
            database
                .add(&[], Some("coordinator archived candidate"), None)
                .unwrap()
                .id
        }
        CandidateKind::ActiveReblit => previous_state,
    };

    prepare_previous_tree(&installation, previous_kind, previous_state);
    installation.active_state = (previous_kind == PreviousKind::Active).then_some(previous_state);

    let candidate_path = installation.staging_path("usr");
    create_canonical_directory(&candidate_path);
    if candidate_kind == CandidateKind::Archived {
        create_canonical_directory(&candidate_path.join("lib"));
        write_canonical_file(&candidate_path.join("lib/os-release"), COORDINATOR_OS_RELEASE);
        write_canonical_file(
            &candidate_path.join("lib/system-model.glu"),
            COORDINATOR_SYSTEM_SNAPSHOT,
        );
    }
    let identity = match candidate_kind {
        CandidateKind::NewState => {
            write_canonical_file(&candidate_path.join("payload-sentinel"), NEW_STATE_PAYLOAD_SENTINEL);
            StatefulTreeIdentity::prepare_unallocated_candidate(&installation, &database, &candidate_path).unwrap()
        }
        CandidateKind::Archived => {
            write_canonical_file(&candidate_path.join(".stateID"), candidate_state.to_string().as_bytes());
            StatefulTreeIdentity::prepare(&installation, &database, &candidate_path, candidate_state).unwrap()
        }
        CandidateKind::ActiveReblit => StatefulTreeIdentity::prepare_active_reblit_candidate(
            &installation,
            &database,
            &candidate_path,
            candidate_state,
        )
        .unwrap(),
    };
    let fixture = CoordinatorFixture {
        _temporary: temporary,
        installation,
        database,
        previous_state,
        candidate_state,
        candidate_path,
    };
    (fixture, identity)
}

fn prepare_previous_tree(installation: &Installation, previous_kind: PreviousKind, previous_state: state::Id) {
    let usr = installation.root.join("usr");
    match previous_kind {
        PreviousKind::Active => {
            create_canonical_directory(&usr);
            write_canonical_file(&usr.join(".stateID"), previous_state.to_string().as_bytes());
        }
        PreviousKind::SynthesizedEmpty => {}
    }
}

fn create_canonical_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn write_canonical_file(path: &Path, contents: &[u8]) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
}

fn state_id_path(fixture: &CoordinatorFixture) -> PathBuf {
    fixture.candidate_path.join(".stateID")
}

fn state_id_temporary_path(fixture: &CoordinatorFixture) -> PathBuf {
    fixture.candidate_path.join(".cast-state-id.tmp")
}

fn assert_new_state_payload_sentinel(fixture: &CoordinatorFixture) {
    assert_eq!(
        fs::read(fixture.candidate_path.join("payload-sentinel")).unwrap(),
        NEW_STATE_PAYLOAD_SENTINEL
    );
}

fn assert_candidate_state_id_absent(fixture: &CoordinatorFixture) {
    assert_state_metadata_name_absent(&state_id_path(fixture));
    assert_candidate_state_id_temporary_absent(fixture);
}

fn assert_candidate_state_id_temporary_absent(fixture: &CoordinatorFixture) {
    assert_state_metadata_name_absent(&state_id_temporary_path(fixture));
}

fn assert_state_metadata_name_absent(path: &Path) {
    match fs::symlink_metadata(path) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => panic!("inspect expected-absent state metadata {path:?}: {source}"),
        Ok(metadata) => panic!(
            "expected state metadata to be absent, found mode {:04o} at {path:?}",
            metadata.permissions().mode() & 0o7777
        ),
    }
}

fn assert_candidate_state_id(fixture: &CoordinatorFixture, expected: state::Id) {
    let path = state_id_path(fixture);
    assert_eq!(fs::read(&path).unwrap(), expected.to_string().as_bytes());
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.file_type().is_file());
    assert_eq!(metadata.uid(), nix::unistd::Uid::effective().as_raw());
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o644);
    assert_eq!(metadata.nlink(), 1);
    assert_candidate_state_id_temporary_absent(fixture);
}

fn request(
    candidate_kind: CandidateKind,
    fixture: &CoordinatorFixture,
    run_system_triggers: bool,
    run_boot_sync: bool,
) -> StatefulTransitionRequest {
    match candidate_kind {
        CandidateKind::NewState => StatefulTransitionRequest::NewState {
            previous: NewStatePrevious::Active(fixture.previous_state),
            run_system_triggers,
            run_boot_sync,
        },
        CandidateKind::Archived => StatefulTransitionRequest::ActivateArchived {
            candidate: fixture.candidate_state,
            previous: fixture.previous_state,
            run_system_triggers,
            run_boot_sync,
        },
        CandidateKind::ActiveReblit => StatefulTransitionRequest::ActiveReblit {
            state: fixture.candidate_state,
            run_system_triggers,
            run_boot_sync,
        },
    }
}

fn canonical_journal(root: &Path) -> PathBuf {
    root.join(".cast/journal/state-transition")
}

fn read_canonical(root: &Path) -> TransitionRecord {
    decode(&fs::read(canonical_journal(root)).unwrap()).unwrap()
}

fn assert_canonical_journal_absent(root: &Path) {
    match fs::symlink_metadata(canonical_journal(root)) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        other => panic!("canonical coordinator journal unexpectedly exists: {other:?}"),
    }
}

fn reopen_record(root: &Path) -> TransitionRecord {
    TransitionJournalStore::open(root)
        .unwrap()
        .load()
        .unwrap()
        .expect("coordinator journal must remain durable")
}

fn other_transition_id() -> TransitionId {
    TransitionId::parse("f000000000000000000000000000000f").unwrap()
}

fn allocate_matching_state(fixture: &CoordinatorFixture, coordinator: &StatefulTransitionCoordinator) -> state::Id {
    let transition = coordinator.transition_id_for_allocation().unwrap();
    fixture
        .database
        .add_with_transition(transition, &[], Some("correlated fresh candidate"), None)
        .unwrap()
        .id
}

fn finish_candidate_prepare(
    coordinator: StatefulTransitionCoordinator,
) -> Result<PreparedStatefulTransitionCoordinator, StatefulTransitionCoordinatorError> {
    coordinator.finish_candidate_prepare(COORDINATOR_SYSTEM_SNAPSHOT, |_| COORDINATOR_OS_RELEASE.to_vec())
}

fn assert_candidate_metadata(fixture: &CoordinatorFixture) {
    assert_eq!(
        fs::read(fixture.candidate_path.join("lib/os-release")).unwrap(),
        COORDINATOR_OS_RELEASE
    );
    assert_eq!(
        fs::read(fixture.candidate_path.join("lib/system-model.glu")).unwrap(),
        COORDINATOR_SYSTEM_SNAPSHOT
    );
}

fn assert_record_prefix(record: &TransitionRecord, operation: Operation, phase: Phase, generation: u64) {
    assert_eq!(record.operation, operation);
    assert_eq!(record.phase, phase);
    assert_eq!(record.generation, generation);
    assert!(record.rollback.is_none());
}

fn journal_names(root: &Path) -> Vec<String> {
    let mut names = fs::read_dir(root.join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

include!("operation_prefixes.rs");
include!("failure_evidence.rs");
include!("transaction_triggers.rs");
include!("metadata_proof.rs");
