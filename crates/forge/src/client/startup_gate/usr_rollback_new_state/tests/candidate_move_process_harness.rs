//! External-process control and retained evidence for the NewState kill matrix.

use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    Installation, State,
    client::startup_reconciliation::{
        NewStateCandidatePreservePostMoveDurabilityEvent, new_state_candidate_preserve_move_attempt_count,
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
    support::{Epoch, reopen_persistent_state_database},
};

const TEST_NAME: &str = concat!(
    "client::startup_gate::usr_rollback_new_state::tests::candidate_move_process_kill::",
    "startup_new_state_candidate_move_process_kill_recovers_without_second_move",
);
pub(super) const ROLE_ENV: &str = "CAST_FORGE_NEW_STATE_CANDIDATE_MOVE_KILL_ROLE";
const EPOCH_ENV: &str = "CAST_FORGE_NEW_STATE_CANDIDATE_MOVE_KILL_EPOCH";
const SOURCE_ENV: &str = "CAST_FORGE_NEW_STATE_CANDIDATE_MOVE_KILL_SOURCE";
const BOUNDARY_ENV: &str = "CAST_FORGE_NEW_STATE_CANDIDATE_MOVE_KILL_BOUNDARY";
const ROOT_ENV: &str = "CAST_FORGE_NEW_STATE_CANDIDATE_MOVE_KILL_ROOT";
const CONTROL_ENV: &str = "CAST_FORGE_NEW_STATE_CANDIDATE_MOVE_KILL_CONTROL";
pub(super) const CHILD_DEADLINE: Duration = Duration::from_secs(15);
const CAST_NAME: &str = ".cast";
const JOURNAL_NAME: &str = "journal";
const CANONICAL_NAME: &str = "state-transition";
const LOCK_NAME: &str = "state-transition.lock";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProcessRole {
    Crash,
    Recover,
}

impl ProcessRole {
    fn parse(value: &str) -> Self {
        match value {
            "crash" => Self::Crash,
            "recover" => Self::Recover,
            other => panic!("invalid NewState candidate-move process role {other:?}"),
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
pub(super) enum ProcessEpoch {
    Current,
    Historical,
}

impl ProcessEpoch {
    pub(super) fn from_fixture(epoch: Epoch) -> Self {
        match epoch {
            Epoch::Current => Self::Current,
            Epoch::Historical => Self::Historical,
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "current" => Self::Current,
            "historical" => Self::Historical,
            other => panic!("invalid NewState candidate-move epoch {other:?}"),
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
pub(super) enum ProcessSource {
    Intent,
    Exchanged,
}

impl ProcessSource {
    pub(super) fn from_fixture(source: CandidateSource) -> Self {
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
            other => panic!("invalid NewState candidate-move source {other:?}"),
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
pub(super) struct MatrixDimensions {
    pub(super) usr_outcome: RollbackActionOutcome,
}

impl MatrixDimensions {
    pub(super) fn for_case(epoch: ProcessEpoch, source: ProcessSource) -> Self {
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
pub(super) struct ChildCase {
    pub(super) role: ProcessRole,
    epoch: ProcessEpoch,
    source: ProcessSource,
    pub(super) boundary: CandidateProcessKillBoundary,
    pub(super) root: PathBuf,
    control: PathBuf,
}

impl ChildCase {
    pub(super) fn from_environment() -> Self {
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

    pub(super) fn source_record(&self) -> TransitionRecord {
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
            "external NewState candidate-move control does not match the source case"
        );
        record
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PublicJournalIdentity {
    cast: (u64, u64),
    journal: (u64, u64),
    lock: (u64, u64),
    canonical: (u64, u64),
}

impl PublicJournalIdentity {
    pub(super) fn capture(root: &Path) -> Self {
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

    pub(super) fn assert_same_public_anchors(self, actual: Self) {
        assert_eq!(actual.cast, self.cast);
        assert_eq!(actual.journal, self.journal);
        assert_eq!(actual.lock, self.lock);
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct FreshCandidateDatabase {
    states: Vec<State>,
    in_flight: Option<db::state::InFlightTransition>,
    candidate_ownership: db::state::TransitionOwnership,
    previous_ownership: db::state::TransitionOwnership,
    candidate_provenance: db::state::MetadataProvenance,
    previous_provenance: Option<db::state::MetadataProvenance>,
}

impl FreshCandidateDatabase {
    pub(super) fn capture(database: &db::state::Database, record: &TransitionRecord) -> Self {
        let candidate = Id::from(record.candidate.id.unwrap());
        let previous = Id::from(record.previous.id.unwrap());
        assert_ne!(candidate, previous);
        let states = database.all().unwrap();
        assert_eq!(states.len(), 2);
        assert_eq!(database.get(candidate).unwrap().id, candidate);
        assert_eq!(database.get(previous).unwrap().id, previous);
        let in_flight = database.audit_in_flight_transition().unwrap();
        assert_eq!(
            in_flight.as_ref().map(|row| (&row.state_id, &row.transition_id)),
            Some((&candidate, &record.transition_id))
        );
        let candidate_ownership = database.transition_ownership(candidate, &record.transition_id).unwrap();
        let previous_ownership = database.transition_ownership(previous, &record.transition_id).unwrap();
        assert_eq!(candidate_ownership, db::state::TransitionOwnership::Matching);
        assert_eq!(previous_ownership, db::state::TransitionOwnership::Cleared);
        let candidate_provenance = database
            .metadata_provenance(candidate)
            .unwrap()
            .expect("NewState candidate provenance must remain present");
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
pub(super) struct CandidateMoveEvidence {
    candidate_tree: Vec<TreeEntry>,
    live_tree: Vec<TreeEntry>,
    candidate: (u64, u64),
    marker: (u64, u64),
    state_id: (u64, u64),
    live: (u64, u64),
    staging: (u64, u64),
    target: (u64, u64),
    quarantine: (u64, u64),
    previous_state: String,
    candidate_state: String,
}

impl CandidateMoveEvidence {
    pub(super) fn capture_staged(root: &Path, record: &TransitionRecord) -> Self {
        assert_staged_topology(root, record);
        let candidate = staged_candidate_path(root);
        let live = root.join("usr");
        Self {
            candidate_tree: snapshot_tree(&candidate),
            live_tree: snapshot_tree(&live),
            candidate: directory_identity(&candidate),
            marker: file_identity(&candidate.join(".cast-tree-id")),
            state_id: file_identity(&candidate.join(".stateID")),
            live: directory_identity(&live),
            staging: directory_identity(&staging_path(root)),
            target: directory_identity(&target_path(root, record)),
            quarantine: directory_identity(&quarantine_path(root)),
            previous_state: fs::read_to_string(live.join(".stateID")).unwrap(),
            candidate_state: fs::read_to_string(candidate.join(".stateID")).unwrap(),
        }
    }

    pub(super) fn assert_preserved(&self, root: &Path, record: &TransitionRecord) {
        assert_preserved_topology(root, record);
        let candidate = preserved_candidate_path(root, record);
        let live = root.join("usr");
        assert_eq!(snapshot_tree(&candidate), self.candidate_tree);
        assert_eq!(snapshot_tree(&live), self.live_tree);
        assert_eq!(directory_identity(&candidate), self.candidate);
        assert_eq!(file_identity(&candidate.join(".cast-tree-id")), self.marker);
        assert_eq!(file_identity(&candidate.join(".stateID")), self.state_id);
        assert_eq!(directory_identity(&live), self.live);
        assert_eq!(directory_identity(&staging_path(root)), self.staging);
        assert_eq!(directory_identity(&target_path(root, record)), self.target);
        assert_eq!(directory_identity(&quarantine_path(root)), self.quarantine);
        assert_eq!(fs::read_to_string(live.join(".stateID")).unwrap(), self.previous_state);
        assert_eq!(
            fs::read_to_string(candidate.join(".stateID")).unwrap(),
            self.candidate_state
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

pub(super) fn assert_candidate_source(
    record: &TransitionRecord,
    epoch: ProcessEpoch,
    source: ProcessSource,
    dimensions: MatrixDimensions,
) {
    assert_eq!(record.operation, Operation::NewState);
    assert_eq!(record.phase, Phase::CandidatePreserveIntent);
    assert_eq!(record.candidate.origin, CandidateOrigin::Fresh);
    assert_eq!(record.previous.origin, PreviousOrigin::ActiveState);
    assert!(record.candidate.id.is_some());
    assert!(record.previous.id.is_some());
    assert_ne!(record.candidate.id, record.previous.id);
    epoch.validate(record);
    let rollback = record.rollback.as_ref().unwrap();
    assert_eq!(rollback.source, source.phase());
    assert_eq!(rollback.previous_archive, RollbackAction::NotRequired);
    assert_eq!(rollback.candidate.action, RollbackAction::Pending);
    assert_eq!(rollback.candidate.disposition, AbortDisposition::Quarantine);
    assert_eq!(rollback.fresh_db, RollbackAction::Pending);
    assert_eq!(rollback.boot, BootRollback::NotRequired);
    assert!(rollback.external_effects_may_remain);
    dimensions.validate(record);
}

pub(super) fn expected_candidate_preserved(source: &TransitionRecord) -> TransitionRecord {
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

pub(super) fn expected_post_events(
    root: &Path,
    record: &TransitionRecord,
) -> Vec<NewStateCandidatePreservePostMoveDurabilityEvent> {
    let candidate = directory_identity(&preserved_candidate_path(root, record));
    let staging = directory_identity(&staging_path(root));
    let target = directory_identity(&target_path(root, record));
    let quarantine = directory_identity(&quarantine_path(root));
    vec![
        NewStateCandidatePreservePostMoveDurabilityEvent::CandidateSynced {
            device: candidate.0,
            inode: candidate.1,
        },
        NewStateCandidatePreservePostMoveDurabilityEvent::StagingParentSynced {
            device: staging.0,
            inode: staging.1,
        },
        NewStateCandidatePreservePostMoveDurabilityEvent::TargetParentSynced {
            device: target.0,
            inode: target.1,
        },
        NewStateCandidatePreservePostMoveDurabilityEvent::QuarantineParentSynced {
            device: quarantine.0,
            inode: quarantine.1,
        },
        NewStateCandidatePreservePostMoveDurabilityEvent::FinalPostProven,
    ]
}

pub(super) fn assert_staged_topology(root: &Path, record: &TransitionRecord) {
    let target = target_path(root, record);
    let candidate = staged_candidate_path(root);
    assert!(candidate.is_dir());
    assert!(target.is_dir());
    assert!(!target.join("usr").exists());
    assert_directory_names(&target, &[]);
    assert_directory_names(&staging_path(root), &[OsStr::new("usr")]);
    assert_common_candidate_topology(root, record, &candidate);
}

pub(super) fn assert_preserved_topology(root: &Path, record: &TransitionRecord) {
    let target = target_path(root, record);
    let candidate = preserved_candidate_path(root, record);
    assert!(!staged_candidate_path(root).exists());
    assert!(candidate.is_dir());
    assert_directory_names(&target, &[OsStr::new("usr")]);
    assert_directory_names(&staging_path(root), &[]);
    assert_common_candidate_topology(root, record, &candidate);
}

fn assert_common_candidate_topology(root: &Path, record: &TransitionRecord, candidate: &Path) {
    let target_metadata = fs::symlink_metadata(target_path(root, record)).unwrap();
    assert!(target_metadata.is_dir());
    assert_eq!(target_metadata.mode() & 0o7777, 0o700);
    assert_eq!(
        fs::read_to_string(candidate.join(".stateID")).unwrap(),
        record.candidate.id.unwrap().to_string()
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

pub(super) fn capture_database_at_root(root: &Path, record: &TransitionRecord) -> FreshCandidateDatabase {
    let installation = Installation::open(root, None).unwrap();
    let database = reopen_persistent_state_database(&installation);
    FreshCandidateDatabase::capture(&database, record)
}

pub(super) fn assert_journal_reopenable(root: &Path, expected: &TransitionRecord) {
    let installation = Installation::open(root, None).unwrap();
    assert_journal_reopenable_from_installation(&installation, expected);
}

pub(super) fn assert_journal_reopenable_from_installation(installation: &Installation, expected: &TransitionRecord) {
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let journal = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root).unwrap();
    assert_eq!(journal.load().unwrap(), Some(expected.clone()));
    drop(journal);
    assert_journal_inventory(&installation.root);
}

pub(super) fn spawn_child(
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

pub(super) fn write_control_case(
    control: &Path,
    epoch: ProcessEpoch,
    source: ProcessSource,
    boundary: CandidateProcessKillBoundary,
    dimensions: MatrixDimensions,
    record: &TransitionRecord,
    source_bytes: &[u8],
) {
    fs::write(
        control.join("case"),
        format!(
            "v1\n{}\n{}\n{}\n{:?}\n{}\n{}\n",
            epoch.as_str(),
            source.as_str(),
            boundary.as_str(),
            dimensions.usr_outcome,
            record.transition_id,
            record.generation,
        ),
    )
    .unwrap();
    fs::write(control.join("source-record"), source_bytes).unwrap();
}

pub(super) fn assert_journal_inventory(root: &Path) {
    let mut names = fs::read_dir(root.join(CAST_NAME).join(JOURNAL_NAME))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    let mut expected = vec![OsString::from(LOCK_NAME), OsString::from(CANONICAL_NAME)];
    expected.sort();
    assert_eq!(names, expected, "unexpected NewState candidate-move journal inventory");
}

pub(super) fn canonical_path(root: &Path) -> PathBuf {
    root.join(CAST_NAME).join(JOURNAL_NAME).join(CANONICAL_NAME)
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

fn quarantine_path(root: &Path) -> PathBuf {
    root.join(CAST_NAME).join("quarantine")
}

fn target_path(root: &Path, record: &TransitionRecord) -> PathBuf {
    quarantine_path(root).join(record.quarantine_name.as_str())
}

fn preserved_candidate_path(root: &Path, record: &TransitionRecord) -> PathBuf {
    target_path(root, record).join("usr")
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
        .unwrap_or_else(|| panic!("required NewState candidate-move environment {name} is missing"))
        .into_string()
        .unwrap_or_else(|_| panic!("NewState candidate-move environment {name} is not UTF-8"))
}

fn canonical_environment_path(name: &str) -> PathBuf {
    let supplied = PathBuf::from(required_unicode_env(name));
    assert!(supplied.is_absolute(), "{name} must be an absolute path");
    let canonical = fs::canonicalize(&supplied).unwrap_or_else(|error| panic!("canonicalize {name}: {error}"));
    assert_eq!(supplied, canonical, "{name} must already be canonical");
    canonical
}

pub(super) fn assert_separate_control_path(root: &Path, control: &Path) {
    assert_ne!(root, control, "control directory cannot be the installation root");
    assert!(
        !root.starts_with(control) && !control.starts_with(root),
        "NewState candidate-move control must remain outside the installation"
    );
}

pub(super) fn assert_parent_environment_clean() {
    for name in [ROLE_ENV, EPOCH_ENV, SOURCE_ENV, BOUNDARY_ENV, ROOT_ENV, CONTROL_ENV] {
        assert!(
            env::var_os(name).is_none(),
            "parent process inherited child-only environment {name}"
        );
    }
}

pub(super) fn kill_after_real_candidate_move() {
    assert_eq!(
        new_state_candidate_preserve_move_attempt_count(),
        1,
        "SIGKILL seam must follow exactly one real candidate move attempt"
    );
    // This proves real same-boot process death only. It is not a reboot or
    // power-loss oracle, and a historical record epoch is not a reboot.
    let result = unsafe { nix::libc::kill(nix::libc::getpid(), nix::libc::SIGKILL) };
    panic!(
        "SIGKILL self-injection unexpectedly returned {result}: {}",
        io::Error::last_os_error()
    );
}

pub(super) struct DeadlineChild {
    child: Option<Child>,
    description: &'static str,
}

impl DeadlineChild {
    pub(super) fn new(child: Child, description: &'static str) -> Self {
        Self {
            child: Some(child),
            description,
        }
    }

    pub(super) fn wait(mut self, timeout: Duration) -> ExitStatus {
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
