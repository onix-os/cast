use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use crate::{
    Installation,
    client::{
        JournalUsrExchangeAuthority, JournalUsrExchangeAuthorityPreflight,
        assert_reverse_exchange_intent_recovers_to_usr_restored,
        assert_usr_exchange_intent_post_recovers_to_pending_reverse,
        assert_usr_restored_routes_to_candidate_preserve_intent,
        assert_usr_rollback_decision_routes_to_reverse_exchange_intent, snapshot_startup_recovery_namespace,
    },
    db,
    state::{self, TransitionId},
    test_support::private_installation_tempdir,
    transition_journal::{
        AbortDisposition, BootRollback, CandidateOrigin, CandidateRollback, ForwardPhase, Operation, Phase,
        PreviousOrigin, RollbackAction, RollbackActionOutcome, RollbackPlan, RuntimeEpoch, RuntimeTreeIdentity,
        TransitionJournalStore, TransitionRecord, decode,
    },
    tree_marker::TreeMarkerStore,
};

use super::*;
use crate::db::state::TransitionOwnership;
use crate::transition_identity::StatefulTreeIdentity;
use crate::transition_identity::{
    RetainedExchangeFaultPoint, RetainedExchangeOutcome, RetainedExchangeSyscallFault,
    arm_after_retained_exchange_rename, arm_before_retained_exchange_rename, arm_retained_exchange_fault,
    arm_retained_exchange_syscall_fault, reset_retained_exchange_syscall_count, retained_exchange_syscall_count,
};

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
    layout_database: db::layout::Database,
    previous_state: state::Id,
    candidate_state: state::Id,
    candidate_path: PathBuf,
}

fn fixture(candidate_kind: CandidateKind, previous_kind: PreviousKind) -> (CoordinatorFixture, StatefulTreeIdentity) {
    let (fixture, identity, authority) = fixture_parts(candidate_kind, previous_kind, false, false);
    assert!(authority.is_none());
    (fixture, identity)
}

fn fixture_with_exchange_authority(
    candidate_kind: CandidateKind,
    previous_kind: PreviousKind,
) -> (CoordinatorFixture, StatefulTreeIdentity, JournalUsrExchangeAuthority) {
    let (fixture, identity, authority) = fixture_parts(candidate_kind, previous_kind, true, false);
    (
        fixture,
        identity,
        authority.expect("exchange fixture requested pre-journal client authority"),
    )
}

fn fixture_with_exchange_authority_and_previous_slot()
-> (CoordinatorFixture, StatefulTreeIdentity, JournalUsrExchangeAuthority) {
    let (fixture, identity, authority) = fixture_parts(CandidateKind::ActiveReblit, PreviousKind::Active, true, true);
    (
        fixture,
        identity,
        authority.expect("two-link exchange fixture requested pre-journal client authority"),
    )
}

fn fixture_parts(
    candidate_kind: CandidateKind,
    previous_kind: PreviousKind,
    retain_exchange_authority: bool,
    retain_previous_slot: bool,
) -> (
    CoordinatorFixture,
    StatefulTreeIdentity,
    Option<JournalUsrExchangeAuthority>,
) {
    let temporary = private_installation_tempdir();
    let mut installation = Installation::open(temporary.path(), None).unwrap();
    let database = db::state::Database::new(":memory:").unwrap();
    let layout_database = db::layout::Database::new(":memory:").unwrap();
    let previous_state = if candidate_kind == CandidateKind::ActiveReblit {
        add_cleared_state_with_provenance(&database, "coordinator active reblit", 'd')
    } else {
        database.add(&[], Some("coordinator previous"), None).unwrap().id
    };
    let candidate_state = match candidate_kind {
        // The row is intentionally absent. SQLite will allocate this exact
        // next ID only after FreshStateAllocating is durable.
        CandidateKind::NewState => previous_state.next(),
        CandidateKind::Archived => add_cleared_state_with_provenance(&database, "coordinator archived candidate", 'e'),
        CandidateKind::ActiveReblit => previous_state,
    };

    prepare_previous_tree(&installation, previous_kind, previous_state);
    installation.active_state = (previous_kind == PreviousKind::Active).then_some(previous_state);

    if retain_previous_slot {
        assert_eq!(candidate_kind, CandidateKind::ActiveReblit);
        let live_usr = installation.root.join("usr");
        let marker_store = TreeMarkerStore::open_path(&live_usr).unwrap();
        let marker = marker_store.adopt_or_create_before_journal().unwrap();
        let wrapper = installation.root_path(previous_state.to_string());
        fs::create_dir(&wrapper).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        fs::hard_link(
            live_usr.join(".cast-tree-id"),
            wrapper.join(format!(
                ".cast-state-slot-{}-{}",
                previous_state,
                marker.token().as_str()
            )),
        )
        .unwrap();
    }

    let active_reblit = (candidate_kind == CandidateKind::ActiveReblit).then(|| database.get(candidate_state).unwrap());
    let exchange_preflight = retain_exchange_authority.then(|| {
        JournalUsrExchangeAuthorityPreflight::acquire_prejournal_for_test(&installation, active_reblit.clone()).unwrap()
    });

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
    let (identity, exchange_authority) = match (candidate_kind, exchange_preflight) {
        (CandidateKind::NewState, preflight) => {
            write_canonical_file(&candidate_path.join("payload-sentinel"), NEW_STATE_PAYLOAD_SENTINEL);
            if let Some(preflight) = preflight {
                let (identity, authority) = preflight
                    .prepare_unallocated_candidate(&database, &candidate_path)
                    .unwrap();
                (identity, Some(authority))
            } else {
                (
                    StatefulTreeIdentity::prepare_unallocated_candidate(&installation, &database, &candidate_path)
                        .unwrap(),
                    None,
                )
            }
        }
        (CandidateKind::Archived, preflight) => {
            write_canonical_file(&candidate_path.join(".stateID"), candidate_state.to_string().as_bytes());
            if let Some(preflight) = preflight {
                let (identity, authority) = preflight
                    .prepare_candidate(&database, &candidate_path, candidate_state)
                    .unwrap();
                (identity, Some(authority))
            } else {
                (
                    StatefulTreeIdentity::prepare(&installation, &database, &candidate_path, candidate_state).unwrap(),
                    None,
                )
            }
        }
        (CandidateKind::ActiveReblit, preflight) => {
            if let Some(preflight) = preflight {
                let (identity, authority) = preflight
                    .prepare_active_reblit_identity(&database, &candidate_path, candidate_state)
                    .unwrap();
                (identity, Some(authority))
            } else {
                (
                    StatefulTreeIdentity::prepare_active_reblit_candidate(
                        &installation,
                        &database,
                        &candidate_path,
                        candidate_state,
                    )
                    .unwrap(),
                    None,
                )
            }
        }
    };
    let fixture = CoordinatorFixture {
        _temporary: temporary,
        installation,
        database,
        layout_database,
        previous_state,
        candidate_state,
        candidate_path,
    };
    (fixture, identity, exchange_authority)
}

fn add_cleared_state_with_provenance(database: &db::state::Database, summary: &str, digit: char) -> state::Id {
    let transition = TransitionId::parse(digit.to_string().repeat(TransitionId::TEXT_LENGTH)).unwrap();
    let state = database
        .add_with_transition(&transition, &[], Some(summary), None)
        .unwrap();
    let provenance = db::state::MetadataProvenance::from_outputs(COORDINATOR_OS_RELEASE, COORDINATOR_SYSTEM_SNAPSHOT);
    database
        .insert_fresh_metadata_provenance_if_transition_matches(state.id, &transition, &provenance)
        .unwrap();
    database.clear_transition_if_matches(state.id, &transition).unwrap();
    state.id
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
            previous: fixture
                .installation
                .active_state
                .map(NewStatePrevious::Active)
                .unwrap_or(NewStatePrevious::SynthesizedEmpty),
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
    coordinator.finish_candidate_prepare(|_| {
        crate::transition_identity::CandidateMetadataOutputs::from_policy(
            COORDINATOR_OS_RELEASE,
            COORDINATOR_SYSTEM_SNAPSHOT,
        )
    })
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
include!("usr_exchange_intent.rs");
include!("usr_exchange_effect.rs");
include!("metadata_provenance.rs");
include!("active_reblit_reservation.rs");
include!("active_reblit_readiness.rs");
include!("transaction_isolation.rs");
