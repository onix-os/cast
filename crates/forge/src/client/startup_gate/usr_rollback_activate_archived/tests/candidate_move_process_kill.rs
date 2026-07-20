//! Real-process restart proof across ActivateArchived candidate preservation.

use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _, process::ExitStatusExt as _},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    Installation, State,
    client::{
        MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal,
        active_state_snapshot::ActiveStateReservation,
        snapshot_startup_recovery_namespace,
        startup_gate::{self, CleanSystemStartup},
        startup_reconciliation::{
            ArchivedCandidatePreservePostMoveDurabilityEvent, archived_candidate_preserve_move_attempt_count,
            arm_before_archived_candidate_preserve_pre_move_revalidation,
            reset_archived_candidate_preserve_move_attempt_count,
            reset_archived_candidate_preserve_post_move_durability_events,
            take_archived_candidate_preserve_post_move_durability_events,
        },
    },
    db,
    state::Id,
    transition_journal::{
        AbortDisposition, BootRollback, CandidateOrigin, ForwardPhase, Operation, Phase, PreviousOrigin,
        RollbackAction, RollbackActionOutcome, TransitionJournalStore, TransitionRecord, decode, encode,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    candidate_process_kill_boundaries::CandidateProcessKillBoundary,
    support::{
        CandidateOrigin as FixtureCandidateOrigin, Epoch, build_candidate, install_persistent_candidate_database,
        open_layout_database, open_state_database, release_candidate_handles,
    },
};

const TEST_NAME: &str = concat!(
    "client::startup_gate::usr_rollback_activate_archived::tests::candidate_move_process_kill::",
    "startup_activate_archived_candidate_move_process_kill_recovers_without_second_move",
);
const ROLE_ENV: &str = "CAST_FORGE_ACTIVATE_ARCHIVED_CANDIDATE_MOVE_KILL_ROLE";
const EPOCH_ENV: &str = "CAST_FORGE_ACTIVATE_ARCHIVED_CANDIDATE_MOVE_KILL_EPOCH";
const SOURCE_ENV: &str = "CAST_FORGE_ACTIVATE_ARCHIVED_CANDIDATE_MOVE_KILL_SOURCE";
const BOUNDARY_ENV: &str = "CAST_FORGE_ACTIVATE_ARCHIVED_CANDIDATE_MOVE_KILL_BOUNDARY";
const ROOT_ENV: &str = "CAST_FORGE_ACTIVATE_ARCHIVED_CANDIDATE_MOVE_KILL_ROOT";
const CONTROL_ENV: &str = "CAST_FORGE_ACTIVATE_ARCHIVED_CANDIDATE_MOVE_KILL_CONTROL";
const CHILD_DEADLINE: Duration = Duration::from_secs(15);
const CAST_NAME: &str = ".cast";
const JOURNAL_NAME: &str = "journal";
const CANONICAL_NAME: &str = "state-transition";
const LOCK_NAME: &str = "state-transition.lock";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessRole {
    Crash,
    Recover,
}

impl ProcessRole {
    fn parse(value: &str) -> Self {
        match value {
            "crash" => Self::Crash,
            "recover" => Self::Recover,
            other => panic!("invalid ActivateArchived candidate-move process role {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Crash => "crash",
            Self::Recover => "recover",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessEpoch {
    Current,
    Historical,
}

impl ProcessEpoch {
    fn from_fixture(epoch: Epoch) -> Self {
        match epoch {
            Epoch::Current => Self::Current,
            Epoch::Historical => Self::Historical,
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "current" => Self::Current,
            "historical" => Self::Historical,
            other => panic!("invalid ActivateArchived candidate-move epoch {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Historical => "historical",
        }
    }

    fn validate(self, record: &TransitionRecord) {
        let current = crate::transition_journal::RuntimeEpoch::capture().unwrap();
        match self {
            Self::Current => assert_eq!(record.creation_epoch, current),
            Self::Historical => assert_ne!(record.creation_epoch, current),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessSource {
    Intent,
    Exchanged,
}

impl ProcessSource {
    fn from_fixture(source: CandidateSource) -> Self {
        match source {
            CandidateSource::Intent => Self::Intent,
            CandidateSource::Exchanged => Self::Exchanged,
            CandidateSource::RootLinksComplete => {
                unreachable!("RootLinksComplete is outside the later process-kill source axis")
            }
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "intent" => Self::Intent,
            "exchanged" => Self::Exchanged,
            other => panic!("invalid ActivateArchived candidate-move source {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Exchanged => "exchanged",
        }
    }

    fn phase(self) -> ForwardPhase {
        match self {
            Self::Intent => ForwardPhase::UsrExchangeIntent,
            Self::Exchanged => ForwardPhase::UsrExchanged,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MatrixDimensions {
    usr_outcome: RollbackActionOutcome,
}

impl MatrixDimensions {
    fn for_case(epoch: ProcessEpoch, source: ProcessSource) -> Self {
        let usr_outcome =
            match (epoch, source) {
                (ProcessEpoch::Current, ProcessSource::Intent)
                | (ProcessEpoch::Historical, ProcessSource::Exchanged) => RollbackActionOutcome::Applied,
                (ProcessEpoch::Current, ProcessSource::Exchanged)
                | (ProcessEpoch::Historical, ProcessSource::Intent) => RollbackActionOutcome::AlreadySatisfied,
            };
        Self { usr_outcome }
    }

    fn validate(self, record: &TransitionRecord) {
        assert_eq!(
            record.rollback.as_ref().unwrap().usr_exchange,
            recorded_action(self.usr_outcome)
        );
    }
}

#[derive(Debug)]
struct ChildCase {
    role: ProcessRole,
    epoch: ProcessEpoch,
    source: ProcessSource,
    boundary: CandidateProcessKillBoundary,
    root: PathBuf,
    control: PathBuf,
}

impl ChildCase {
    fn from_environment() -> Self {
        let role = ProcessRole::parse(&required_unicode_env(ROLE_ENV));
        let epoch = ProcessEpoch::parse(&required_unicode_env(EPOCH_ENV));
        let source = ProcessSource::parse(&required_unicode_env(SOURCE_ENV));
        let boundary = CandidateProcessKillBoundary::parse(&required_unicode_env(BOUNDARY_ENV));
        let root = canonical_environment_path(ROOT_ENV);
        let control = canonical_environment_path(CONTROL_ENV);
        assert_separate_control_path(&root, &control);
        Self {
            role,
            epoch,
            source,
            boundary,
            root,
            control,
        }
    }

    fn dimensions(&self) -> MatrixDimensions {
        MatrixDimensions::for_case(self.epoch, self.source)
    }

    fn source_record(&self) -> TransitionRecord {
        let bytes = fs::read(self.control.join("source-record")).unwrap();
        let record = decode(&bytes).unwrap();
        assert_eq!(encode(&record).unwrap(), bytes);
        assert_candidate_source(&record, self.epoch, self.source, self.dimensions());
        let expected_case = format!(
            "v1\n{}\n{}\n{}\n{:?}\n{}\n{}\n",
            self.epoch.as_str(),
            self.source.as_str(),
            self.boundary.as_str(),
            self.dimensions().usr_outcome,
            record.transition_id,
            record.generation,
        );
        assert_eq!(
            fs::read_to_string(self.control.join("case")).unwrap(),
            expected_case,
            "external ActivateArchived candidate-move control does not match the source case"
        );
        record
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PublicJournalIdentity {
    cast: (u64, u64),
    journal: (u64, u64),
    lock: (u64, u64),
    canonical: (u64, u64),
}

impl PublicJournalIdentity {
    fn capture(root: &Path) -> Self {
        let cast = root.join(CAST_NAME);
        let journal = cast.join(JOURNAL_NAME);
        assert_journal_inventory(root);
        Self {
            cast: directory_identity(&cast),
            journal: directory_identity(&journal),
            lock: file_identity(&journal.join(LOCK_NAME)),
            canonical: file_identity(&journal.join(CANONICAL_NAME)),
        }
    }

    fn assert_same_public_anchors(self, actual: Self) {
        assert_eq!(actual.cast, self.cast);
        assert_eq!(actual.journal, self.journal);
        assert_eq!(actual.lock, self.lock);
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ExistingCandidateDatabase {
    states: Vec<State>,
    in_flight: Option<db::state::InFlightTransition>,
    candidate_ownership: db::state::TransitionOwnership,
    previous_ownership: db::state::TransitionOwnership,
    candidate_provenance: db::state::MetadataProvenance,
    previous_provenance: Option<db::state::MetadataProvenance>,
}

impl ExistingCandidateDatabase {
    fn capture(database: &db::state::Database, record: &TransitionRecord) -> Self {
        let candidate = Id::from(record.candidate.id.unwrap());
        let previous = Id::from(record.previous.id.unwrap());
        assert_ne!(candidate, previous);
        let states = database.all().unwrap();
        assert_eq!(states.len(), 2);
        assert_eq!(database.get(candidate).unwrap().id, candidate);
        assert_eq!(database.get(previous).unwrap().id, previous);
        let in_flight = database.audit_in_flight_transition().unwrap();
        assert_eq!(in_flight, None);
        let candidate_ownership = database.transition_ownership(candidate, &record.transition_id).unwrap();
        let previous_ownership = database.transition_ownership(previous, &record.transition_id).unwrap();
        assert_eq!(candidate_ownership, db::state::TransitionOwnership::Cleared);
        assert_eq!(previous_ownership, db::state::TransitionOwnership::Cleared);
        let candidate_provenance = database
            .metadata_provenance(candidate)
            .unwrap()
            .expect("ActivateArchived candidate provenance must remain present");
        let previous_provenance = database.metadata_provenance(previous).unwrap();
        assert_eq!(previous_provenance, None);
        Self {
            states,
            in_flight,
            candidate_ownership,
            previous_ownership,
            candidate_provenance,
            previous_provenance,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct CandidateMoveEvidence {
    tree: Vec<TreeEntry>,
    candidate: (u64, u64),
    marker: (u64, u64),
    slot: (u64, u64),
    wrapper: (u64, u64),
    staging: (u64, u64),
    roots: (u64, u64),
    previous_state: String,
}

impl CandidateMoveEvidence {
    fn capture_staged(root: &Path, record: &TransitionRecord) -> Self {
        assert_staged_topology(root, record);
        let candidate = staged_candidate_path(root);
        let marker = candidate.join(".cast-tree-id");
        let slot = archived_slot_path(root, record);
        Self {
            tree: snapshot_tree(&candidate),
            candidate: directory_identity(&candidate),
            marker: file_identity(&marker),
            slot: file_identity(&slot),
            wrapper: directory_identity(&archived_wrapper_path(root, record)),
            staging: directory_identity(&staging_path(root)),
            roots: directory_identity(&roots_path(root)),
            previous_state: fs::read_to_string(root.join("usr/.stateID")).unwrap(),
        }
    }

    fn assert_preserved(&self, root: &Path, record: &TransitionRecord) {
        assert_preserved_topology(root, record);
        let candidate = preserved_candidate_path(root, record);
        assert_eq!(snapshot_tree(&candidate), self.tree);
        assert_eq!(directory_identity(&candidate), self.candidate);
        assert_eq!(file_identity(&candidate.join(".cast-tree-id")), self.marker);
        assert_eq!(file_identity(&archived_slot_path(root, record)), self.slot);
        assert_eq!(directory_identity(&archived_wrapper_path(root, record)), self.wrapper);
        assert_eq!(directory_identity(&staging_path(root)), self.staging);
        assert_eq!(directory_identity(&roots_path(root)), self.roots);
        assert_eq!(
            fs::read_to_string(root.join("usr/.stateID")).unwrap(),
            self.previous_state
        );
    }
}

#[derive(Debug, Eq, PartialEq)]
struct TreeEntry {
    relative: PathBuf,
    kind: TreeEntryKind,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    length: u64,
    payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TreeEntryKind {
    Directory,
    File,
    Symlink,
}

#[test]
fn startup_activate_archived_candidate_move_process_kill_recovers_without_second_move() {
    match env::var_os(ROLE_ENV) {
        Some(_) => run_child(ChildCase::from_environment()),
        None => run_parent(),
    }
}

fn run_parent() {
    assert_parent_environment_clean();
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for boundary in CandidateProcessKillBoundary::ALL {
                run_parent_case(epoch, source, boundary);
                cases += 1;
            }
        }
    }
    assert_eq!(
        cases, 28,
        "ActivateArchived candidate-move SIGKILL matrix must remain exactly 2 x 2 x 7"
    );
}

fn run_parent_case(epoch: Epoch, source: CandidateSource, boundary: CandidateProcessKillBoundary) {
    let process_epoch = ProcessEpoch::from_fixture(epoch);
    let process_source = ProcessSource::from_fixture(source);
    let dimensions = MatrixDimensions::for_case(process_epoch, process_source);
    let mut fixture = build_candidate(epoch, source, dimensions.usr_outcome, FixtureCandidateOrigin::Applied);
    install_persistent_candidate_database(&mut fixture);
    let source_record = fixture.candidate_intent.clone();
    assert_candidate_source(&source_record, process_epoch, process_source, dimensions);
    let expected = expected_candidate_preserved(&source_record);

    let root = fs::canonicalize(&fixture.fixture.installation.root).unwrap();
    let source_bytes = fs::read(canonical_path(&root)).unwrap();
    assert_eq!(source_bytes, encode(&source_record).unwrap());
    let public_before = PublicJournalIdentity::capture(&root);
    let database_before = ExistingCandidateDatabase::capture(&fixture.fixture.database, &source_record);
    let move_evidence = CandidateMoveEvidence::capture_staged(&root, &source_record);

    let control = tempfile::tempdir().unwrap();
    let control_path = fs::canonicalize(control.path()).unwrap();
    assert_separate_control_path(&root, &control_path);
    fs::write(
        control_path.join("case"),
        format!(
            "v1\n{}\n{}\n{}\n{:?}\n{}\n{}\n",
            process_epoch.as_str(),
            process_source.as_str(),
            boundary.as_str(),
            dimensions.usr_outcome,
            source_record.transition_id,
            source_record.generation,
        ),
    )
    .unwrap();
    fs::write(control_path.join("source-record"), &source_bytes).unwrap();
    let retained_root = release_candidate_handles(fixture);

    let crash = spawn_child(
        ProcessRole::Crash,
        process_epoch,
        process_source,
        boundary,
        &root,
        &control_path,
    );
    let crash_status = DeadlineChild::new(crash, "ActivateArchived candidate-move crash child").wait(CHILD_DEADLINE);
    assert_eq!(
        crash_status.signal(),
        Some(nix::libc::SIGKILL),
        "crash child for {process_epoch:?} {process_source:?} {boundary:?} missed its boundary: {crash_status:?}"
    );
    let public_after_crash = PublicJournalIdentity::capture(&root);
    assert_eq!(public_after_crash, public_before);
    assert_eq!(fs::read(canonical_path(&root)).unwrap(), source_bytes);
    assert_eq!(capture_database_at_root(&root, &source_record), database_before);
    move_evidence.assert_preserved(&root, &source_record);
    let namespace_after_crash = snapshot_startup_recovery_namespace(&root);
    assert_journal_reopenable(&root, &source_record);

    let recovery = spawn_child(
        ProcessRole::Recover,
        process_epoch,
        process_source,
        boundary,
        &root,
        &control_path,
    );
    let recovery_status =
        DeadlineChild::new(recovery, "ActivateArchived candidate-move recovery child").wait(CHILD_DEADLINE);
    assert!(
        recovery_status.success(),
        "recovery child failed for {process_epoch:?} {process_source:?}: {recovery_status:?}"
    );
    assert_eq!(recovery_status.signal(), None);

    let public_after_recovery = PublicJournalIdentity::capture(&root);
    public_before.assert_same_public_anchors(public_after_recovery);
    assert_eq!(fs::read(canonical_path(&root)).unwrap(), encode(&expected).unwrap());
    assert_eq!(capture_database_at_root(&root, &expected), database_before);
    assert_eq!(snapshot_startup_recovery_namespace(&root), namespace_after_crash);
    move_evidence.assert_preserved(&root, &expected);
    assert_journal_reopenable(&root, &expected);

    drop(retained_root);
    drop(control);
}

fn run_child(case: ChildCase) {
    let source = case.source_record();
    let installation = Installation::open(&case.root, None).unwrap();
    assert_eq!(installation.root, case.root);
    let database = open_state_database(&installation);
    let layout_database = open_layout_database(&installation);
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation,
        database,
        layout_database,
    );
    ExistingCandidateDatabase::capture(system.state_db(), &source);
    match case.role {
        ProcessRole::Crash => run_crash_child(&case, &system, &source),
        ProcessRole::Recover => run_recovery_child(&case, &system, &source),
    }
}

fn run_crash_child(case: &ChildCase, system: &MutableSystemCapabilities, source: &TransitionRecord) {
    assert_eq!(decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(), *source);
    assert_journal_inventory(&case.root);
    assert_staged_topology(&case.root, source);
    reset_archived_candidate_preserve_move_attempt_count();
    reset_archived_candidate_preserve_post_move_durability_events();
    assert_eq!(archived_candidate_preserve_move_attempt_count(), 0);
    assert!(take_archived_candidate_preserve_post_move_durability_events().is_empty());
    case.boundary.arm(kill_after_real_candidate_move);

    let reservation = ActiveStateReservation::acquire().unwrap();
    let result = CleanSystemStartup::enter(system, &reservation);
    panic!(
        "crash child escaped ActivateArchived post-move boundary with startup success={} error={:?}",
        result.is_ok(),
        result.err(),
    );
}

fn run_recovery_child(case: &ChildCase, system: &MutableSystemCapabilities, source: &TransitionRecord) {
    let installation = system.installation();
    let database = system.state_db();
    assert_eq!(decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(), *source);
    assert_journal_inventory(&case.root);
    assert_preserved_topology(&case.root, source);
    let database_before = ExistingCandidateDatabase::capture(database, source);
    let namespace_before = snapshot_startup_recovery_namespace(&case.root);
    let expected = expected_candidate_preserved(source);
    let expected_events = expected_post_events(&case.root, source);
    reset_archived_candidate_preserve_move_attempt_count();
    reset_archived_candidate_preserve_post_move_durability_events();
    arm_before_archived_candidate_preserve_pre_move_revalidation(|| {
        panic!("fresh ActivateArchived recovery selected Apply instead of the no-move Finish path")
    });

    let reservation = ActiveStateReservation::acquire().unwrap();
    let error = match CleanSystemStartup::enter(system, &reservation) {
        Ok(_) => panic!("fresh startup admitted unresolved ActivateArchived candidate evidence"),
        Err(error) => error,
    };
    let startup_gate::Error::RecoveryPending(pending) = &error else {
        panic!("expected exact CandidatePreserved recovery-pending result, got {error:?}");
    };
    assert_eq!(pending.transition_id(), &expected.transition_id);
    assert_eq!(pending.phase(), Phase::CandidatePreserved);
    assert_eq!(pending.disposition(), expected.recovery_disposition());
    assert!(
        pending.blockers().is_empty(),
        "unexpected startup blockers: {:?}",
        pending.blockers()
    );
    assert!(pending.retains_database(database));
    assert_eq!(archived_candidate_preserve_move_attempt_count(), 0);
    assert_eq!(
        take_archived_candidate_preserve_post_move_durability_events(),
        expected_events
    );
    assert_eq!(
        decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(),
        expected
    );
    assert_eq!(ExistingCandidateDatabase::capture(database, &expected), database_before);
    assert_eq!(snapshot_startup_recovery_namespace(&case.root), namespace_before);
    assert_preserved_topology(&case.root, &expected);
    assert_journal_inventory(&case.root);
    drop(error);
    drop(reservation);
    assert_journal_reopenable_from_installation(installation, &expected);
}

fn assert_candidate_source(
    record: &TransitionRecord,
    epoch: ProcessEpoch,
    source: ProcessSource,
    dimensions: MatrixDimensions,
) {
    assert_eq!(record.operation, Operation::ActivateArchived);
    assert_eq!(record.phase, Phase::CandidatePreserveIntent);
    assert_eq!(record.candidate.origin, CandidateOrigin::Archived);
    assert_eq!(record.previous.origin, PreviousOrigin::ActiveState);
    assert!(record.candidate.id.is_some());
    assert!(record.previous.id.is_some());
    assert_ne!(record.candidate.id, record.previous.id);
    epoch.validate(record);
    let rollback = record.rollback.as_ref().unwrap();
    assert_eq!(rollback.source, source.phase());
    assert_eq!(rollback.previous_archive, RollbackAction::NotRequired);
    assert_eq!(rollback.candidate.action, RollbackAction::Pending);
    assert_eq!(rollback.candidate.disposition, AbortDisposition::Rearchive);
    assert_eq!(rollback.fresh_db, RollbackAction::NotRequired);
    assert_eq!(rollback.boot, BootRollback::NotRequired);
    assert!(!rollback.external_effects_may_remain);
    dimensions.validate(record);
}

fn expected_candidate_preserved(source: &TransitionRecord) -> TransitionRecord {
    let expected = source
        .rollback_successor(Some(RollbackActionOutcome::AlreadySatisfied))
        .unwrap();
    assert_eq!(expected.phase, Phase::CandidatePreserved);
    assert_eq!(
        expected.rollback.as_ref().unwrap().candidate.action,
        RollbackAction::AlreadySatisfied
    );
    expected
}

fn expected_post_events(
    root: &Path,
    record: &TransitionRecord,
) -> Vec<ArchivedCandidatePreservePostMoveDurabilityEvent> {
    let candidate = directory_identity(&preserved_candidate_path(root, record));
    let staging = directory_identity(&staging_path(root));
    let target = directory_identity(&archived_wrapper_path(root, record));
    let roots = directory_identity(&roots_path(root));
    vec![
        ArchivedCandidatePreservePostMoveDurabilityEvent::CandidateSynced {
            device: candidate.0,
            inode: candidate.1,
        },
        ArchivedCandidatePreservePostMoveDurabilityEvent::StagingParentSynced {
            device: staging.0,
            inode: staging.1,
        },
        ArchivedCandidatePreservePostMoveDurabilityEvent::TargetParentSynced {
            device: target.0,
            inode: target.1,
        },
        ArchivedCandidatePreservePostMoveDurabilityEvent::RootsParentSynced {
            device: roots.0,
            inode: roots.1,
        },
        ArchivedCandidatePreservePostMoveDurabilityEvent::FinalPostProven,
    ]
}

fn assert_staged_topology(root: &Path, record: &TransitionRecord) {
    let wrapper = archived_wrapper_path(root, record);
    let slot = archived_slot_path(root, record);
    let candidate = staged_candidate_path(root);
    let marker = candidate.join(".cast-tree-id");
    assert!(candidate.is_dir());
    assert!(!wrapper.join("usr").exists());
    assert_eq!(file_identity(&marker), file_identity(&slot));
    assert_eq!(fs::symlink_metadata(&marker).unwrap().nlink(), 2);
    assert_eq!(fs::symlink_metadata(&slot).unwrap().nlink(), 2);
    assert_directory_names(&wrapper, &[slot.file_name().unwrap()]);
    assert_directory_names(&staging_path(root), &[OsStr::new("usr")]);
    assert_common_candidate_topology(root, record);
}

fn assert_preserved_topology(root: &Path, record: &TransitionRecord) {
    let wrapper = archived_wrapper_path(root, record);
    let slot = archived_slot_path(root, record);
    let candidate = preserved_candidate_path(root, record);
    let marker = candidate.join(".cast-tree-id");
    assert!(!staged_candidate_path(root).exists());
    assert!(candidate.is_dir());
    assert_eq!(file_identity(&marker), file_identity(&slot));
    assert_eq!(fs::symlink_metadata(&marker).unwrap().nlink(), 2);
    assert_eq!(fs::symlink_metadata(&slot).unwrap().nlink(), 2);
    assert_directory_names(&wrapper, &[OsStr::new("usr"), slot.file_name().unwrap()]);
    assert_directory_names(&staging_path(root), &[]);
    assert_common_candidate_topology(root, record);
}

fn assert_common_candidate_topology(root: &Path, record: &TransitionRecord) {
    assert!(
        !root
            .join(CAST_NAME)
            .join("quarantine")
            .join(record.quarantine_name.as_str())
            .exists()
    );
    assert_eq!(
        fs::read_to_string(root.join("usr/.stateID")).unwrap(),
        record.previous.id.unwrap().to_string()
    );
}

fn assert_directory_names(directory: &Path, expected: &[&OsStr]) {
    let mut actual = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    actual.sort();
    let mut expected = expected.iter().map(|name| OsString::from(*name)).collect::<Vec<_>>();
    expected.sort();
    assert_eq!(actual, expected, "unexpected entries in {}", directory.display());
}

fn snapshot_tree(root: &Path) -> Vec<TreeEntry> {
    let mut entries = Vec::new();
    snapshot_tree_path(root, root, &mut entries);
    entries
}

fn snapshot_tree_path(root: &Path, path: &Path, entries: &mut Vec<TreeEntry>) {
    let metadata = fs::symlink_metadata(path).unwrap();
    let file_type = metadata.file_type();
    let (kind, payload) = if file_type.is_dir() {
        (TreeEntryKind::Directory, Vec::new())
    } else if file_type.is_file() {
        (TreeEntryKind::File, fs::read(path).unwrap())
    } else if file_type.is_symlink() {
        (
            TreeEntryKind::Symlink,
            fs::read_link(path).unwrap().as_os_str().as_bytes().to_vec(),
        )
    } else {
        panic!("unexpected candidate tree entry kind at {}", path.display());
    };
    entries.push(TreeEntry {
        relative: path.strip_prefix(root).unwrap().to_owned(),
        kind,
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        length: metadata.len(),
        payload,
    });
    if file_type.is_dir() {
        let mut children = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        children.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
        for child in children {
            snapshot_tree_path(root, &child, entries);
        }
    }
}

fn capture_database_at_root(root: &Path, record: &TransitionRecord) -> ExistingCandidateDatabase {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    ExistingCandidateDatabase::capture(&database, record)
}

fn assert_journal_reopenable(root: &Path, expected: &TransitionRecord) {
    let installation = Installation::open(root, None).unwrap();
    assert_journal_reopenable_from_installation(&installation, expected);
}

fn assert_journal_reopenable_from_installation(installation: &Installation, expected: &TransitionRecord) {
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let journal = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root).unwrap();
    assert_eq!(journal.load().unwrap(), Some(expected.clone()));
    drop(journal);
    assert_journal_inventory(&installation.root);
}

fn spawn_child(
    role: ProcessRole,
    epoch: ProcessEpoch,
    source: ProcessSource,
    boundary: CandidateProcessKillBoundary,
    root: &Path,
    control: &Path,
) -> Child {
    Command::new(env::current_exe().unwrap())
        .arg(TEST_NAME)
        .arg("--exact")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(ROLE_ENV, role.as_str())
        .env(EPOCH_ENV, epoch.as_str())
        .env(SOURCE_ENV, source.as_str())
        .env(BOUNDARY_ENV, boundary.as_str())
        .env(ROOT_ENV, root)
        .env(CONTROL_ENV, control)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

fn assert_journal_inventory(root: &Path) {
    let mut names = fs::read_dir(root.join(CAST_NAME).join(JOURNAL_NAME))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    let mut expected = vec![OsString::from(LOCK_NAME), OsString::from(CANONICAL_NAME)];
    expected.sort();
    assert_eq!(
        names, expected,
        "unexpected ActivateArchived candidate-move journal inventory"
    );
}

fn roots_path(root: &Path) -> PathBuf {
    root.join(CAST_NAME).join("root")
}

fn staging_path(root: &Path) -> PathBuf {
    roots_path(root).join("staging")
}

fn staged_candidate_path(root: &Path) -> PathBuf {
    staging_path(root).join("usr")
}

fn archived_wrapper_path(root: &Path, record: &TransitionRecord) -> PathBuf {
    roots_path(root).join(record.candidate.id.unwrap().to_string())
}

fn preserved_candidate_path(root: &Path, record: &TransitionRecord) -> PathBuf {
    archived_wrapper_path(root, record).join("usr")
}

fn archived_slot_path(root: &Path, record: &TransitionRecord) -> PathBuf {
    let candidate = record.candidate.id.unwrap();
    archived_wrapper_path(root, record).join(format!(
        ".cast-state-slot-{candidate}-{}",
        record.candidate.tree_token.as_str()
    ))
}

fn canonical_path(root: &Path) -> PathBuf {
    root.join(CAST_NAME).join(JOURNAL_NAME).join(CANONICAL_NAME)
}

fn directory_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_dir(), "{} is not a directory", path.display());
    (metadata.dev(), metadata.ino())
}

fn file_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_file(), "{} is not a regular file", path.display());
    (metadata.dev(), metadata.ino())
}

fn recorded_action(outcome: RollbackActionOutcome) -> RollbackAction {
    match outcome {
        RollbackActionOutcome::Applied => RollbackAction::Applied,
        RollbackActionOutcome::AlreadySatisfied => RollbackAction::AlreadySatisfied,
    }
}

fn required_unicode_env(name: &str) -> String {
    env::var_os(name)
        .unwrap_or_else(|| panic!("required ActivateArchived candidate-move environment {name} is missing"))
        .into_string()
        .unwrap_or_else(|_| panic!("ActivateArchived candidate-move environment {name} is not UTF-8"))
}

fn canonical_environment_path(name: &str) -> PathBuf {
    let supplied = PathBuf::from(required_unicode_env(name));
    assert!(supplied.is_absolute(), "{name} must be an absolute path");
    let canonical = fs::canonicalize(&supplied).unwrap_or_else(|error| panic!("canonicalize {name}: {error}"));
    assert_eq!(supplied, canonical, "{name} must already be canonical");
    canonical
}

fn assert_separate_control_path(root: &Path, control: &Path) {
    assert_ne!(root, control, "control directory cannot be the installation root");
    assert!(
        !root.starts_with(control) && !control.starts_with(root),
        "ActivateArchived candidate-move control must remain outside the installation"
    );
}

fn assert_parent_environment_clean() {
    for name in [ROLE_ENV, EPOCH_ENV, SOURCE_ENV, BOUNDARY_ENV, ROOT_ENV, CONTROL_ENV] {
        assert!(
            env::var_os(name).is_none(),
            "parent process inherited child-only environment {name}"
        );
    }
}

fn kill_after_real_candidate_move() {
    assert_eq!(
        archived_candidate_preserve_move_attempt_count(),
        1,
        "SIGKILL seam must follow exactly one real candidate move attempt"
    );
    kill_self();
}

fn kill_self() {
    // This proves real same-boot process death only. It is not a reboot or
    // power-loss oracle, and a historical record epoch is not a reboot.
    let result = unsafe { nix::libc::kill(nix::libc::getpid(), nix::libc::SIGKILL) };
    panic!(
        "SIGKILL self-injection unexpectedly returned {result}: {}",
        io::Error::last_os_error()
    );
}

struct DeadlineChild {
    child: Option<Child>,
    description: &'static str,
}

impl DeadlineChild {
    fn new(child: Child, description: &'static str) -> Self {
        Self {
            child: Some(child),
            description,
        }
    }

    fn wait(mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                self.child.take();
                return status;
            }
            if Instant::now() >= deadline {
                let mut child = self.child.take().unwrap();
                let _ = child.kill();
                let status = child.wait().unwrap();
                panic!(
                    "{} exceeded {timeout:?}; killed and reaped with {status:?}",
                    self.description
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for DeadlineChild {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
