//! External-process control and retained evidence for ActiveReblit wrapper exchange.

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
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent,
        active_reblit_candidate_preserve_exchange_attempt_count,
        take_active_reblit_candidate_preserve_post_exchange_durability_events,
    },
    db,
    state::Id,
    transition_journal::{
        AbortDisposition, BootRollback, CandidateOrigin as JournalCandidateOrigin, ForwardPhase, Operation, Phase,
        PreviousOrigin, RollbackAction, RollbackActionOutcome, TransitionJournalStore, TransitionRecord, decode,
        encode,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    candidate_wrapper_exchange_kill_boundaries::CandidateWrapperExchangeKillBoundary,
    support::{Epoch, open_state_database},
};

const TEST_NAME: &str = concat!(
    "client::startup_gate::usr_rollback_active_reblit::tests::candidate_wrapper_exchange_process_kill::",
    "startup_active_reblit_candidate_wrapper_exchange_process_kill_recovers_without_second_exchange",
);
pub(super) const ROLE_ENV: &str = "CAST_FORGE_ACTIVE_REBLIT_WRAPPER_EXCHANGE_KILL_ROLE";
const EPOCH_ENV: &str = "CAST_FORGE_ACTIVE_REBLIT_WRAPPER_EXCHANGE_KILL_EPOCH";
const SOURCE_ENV: &str = "CAST_FORGE_ACTIVE_REBLIT_WRAPPER_EXCHANGE_KILL_SOURCE";
const BOUNDARY_ENV: &str = "CAST_FORGE_ACTIVE_REBLIT_WRAPPER_EXCHANGE_KILL_BOUNDARY";
const ROOT_ENV: &str = "CAST_FORGE_ACTIVE_REBLIT_WRAPPER_EXCHANGE_KILL_ROOT";
const CONTROL_ENV: &str = "CAST_FORGE_ACTIVE_REBLIT_WRAPPER_EXCHANGE_KILL_CONTROL";
pub(super) const CHILD_DEADLINE: Duration = Duration::from_secs(15);
const CAST_NAME: &str = ".cast";
const JOURNAL_NAME: &str = "journal";
const CANONICAL_NAME: &str = "state-transition";
const LOCK_NAME: &str = "state-transition.lock";
const ROOT_ABI: [(&str, &str); 5] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib32", "usr/lib32"),
    ("lib64", "usr/lib"),
];

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
            other => panic!("invalid ActiveReblit wrapper-exchange process role {other:?}"),
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
            other => panic!("invalid ActiveReblit wrapper-exchange epoch {other:?}"),
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
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "intent" => Self::Intent,
            "exchanged" => Self::Exchanged,
            other => panic!("invalid ActiveReblit wrapper-exchange source {other:?}"),
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
    pub(super) wrapper_index: usize,
}

impl MatrixDimensions {
    pub(super) fn for_case(epoch: ProcessEpoch, source: ProcessSource) -> Self {
        match (epoch, source) {
            (ProcessEpoch::Current, ProcessSource::Intent) => Self {
                usr_outcome: RollbackActionOutcome::Applied,
                wrapper_index: 0,
            },
            (ProcessEpoch::Current, ProcessSource::Exchanged) => Self {
                usr_outcome: RollbackActionOutcome::Applied,
                wrapper_index: 13,
            },
            (ProcessEpoch::Historical, ProcessSource::Intent) => Self {
                usr_outcome: RollbackActionOutcome::AlreadySatisfied,
                wrapper_index: 13,
            },
            (ProcessEpoch::Historical, ProcessSource::Exchanged) => Self {
                usr_outcome: RollbackActionOutcome::AlreadySatisfied,
                wrapper_index: 0,
            },
        }
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
    pub(super) boundary: CandidateWrapperExchangeKillBoundary,
    pub(super) root: PathBuf,
    control: PathBuf,
}

impl ChildCase {
    pub(super) fn from_environment() -> Self {
        let role = ProcessRole::parse(&required_unicode_env(ROLE_ENV));
        let epoch = ProcessEpoch::parse(&required_unicode_env(EPOCH_ENV));
        let source = ProcessSource::parse(&required_unicode_env(SOURCE_ENV));
        let boundary = CandidateWrapperExchangeKillBoundary::parse(&required_unicode_env(BOUNDARY_ENV));
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

    pub(super) fn dimensions(&self) -> MatrixDimensions {
        MatrixDimensions::for_case(self.epoch, self.source)
    }

    pub(super) fn source_record(&self) -> TransitionRecord {
        let bytes = fs::read(self.control.join("source-record")).unwrap();
        let record = decode(&bytes).unwrap();
        assert_eq!(encode(&record).unwrap(), bytes);
        assert_candidate_source(&record, self.epoch, self.source, self.dimensions());
        let dimensions = self.dimensions();
        let expected_case = format!(
            "v1\n{}\n{}\n{}\n{:?}\n{}\n{}\n{}\n",
            self.epoch.as_str(),
            self.source.as_str(),
            self.boundary.as_str(),
            dimensions.usr_outcome,
            dimensions.wrapper_index,
            record.transition_id,
            record.generation,
        );
        assert_eq!(
            fs::read_to_string(self.control.join("case")).unwrap(),
            expected_case,
            "external ActiveReblit wrapper-exchange control does not match the source case"
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

    pub(super) fn assert_source_unchanged(self, root: &Path, expected_bytes: &[u8]) {
        assert_eq!(Self::capture(root), self);
        assert_eq!(fs::read(canonical_path(root)).unwrap(), expected_bytes);
    }

    pub(super) fn assert_same_public_anchors(self, actual: Self) {
        assert_eq!(actual.cast, self.cast);
        assert_eq!(actual.journal, self.journal);
        assert_eq!(actual.lock, self.lock);
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct ExistingCandidateDatabase {
    states: Vec<State>,
    in_flight: Option<db::state::InFlightTransition>,
    ownership: db::state::TransitionOwnership,
    provenance: db::state::MetadataProvenance,
}

impl ExistingCandidateDatabase {
    pub(super) fn capture(database: &db::state::Database, record: &TransitionRecord) -> Self {
        assert_eq!(record.candidate.id, record.previous.id);
        let candidate = Id::from(record.candidate.id.unwrap());
        let states = database.all().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(database.get(candidate).unwrap().id, candidate);
        let in_flight = database.audit_in_flight_transition().unwrap();
        assert_eq!(in_flight, None);
        let ownership = database.transition_ownership(candidate, &record.transition_id).unwrap();
        assert_eq!(ownership, db::state::TransitionOwnership::Cleared);
        let provenance = database
            .metadata_provenance(candidate)
            .unwrap()
            .expect("ActiveReblit candidate provenance must remain present");
        Self {
            states,
            in_flight,
            ownership,
            provenance,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct WrapperExchangeEvidence {
    candidate_wrapper_tree: Vec<TreeEntry>,
    reservation_wrapper_tree: Vec<TreeEntry>,
    live_tree: Vec<TreeEntry>,
    candidate_wrapper: (u64, u64),
    reservation_wrapper: (u64, u64),
    candidate: (u64, u64),
    marker: (u64, u64),
    state_id: (u64, u64),
    live: (u64, u64),
    roots: (u64, u64),
    quarantine: (u64, u64),
    root_names: Vec<OsString>,
    quarantine_names: Vec<OsString>,
    other_roots: Vec<(OsString, Vec<TreeEntry>)>,
    other_quarantine: Vec<(OsString, Vec<TreeEntry>)>,
    root_abi: Vec<(String, Option<(PathBuf, (u64, u64))>)>,
    candidate_state: String,
    live_state: String,
}

impl WrapperExchangeEvidence {
    pub(super) fn capture_staged(root: &Path, record: &TransitionRecord, wrapper_index: usize) -> Self {
        assert_staged_topology(root, record, wrapper_index);
        let candidate_wrapper_path = staging_wrapper_path(root);
        let reservation_wrapper_path = wrapper_path(root, record, wrapper_index);
        let candidate = candidate_wrapper_path.join("usr");
        let live = root.join("usr");
        let roots = roots_path(root);
        let quarantine = quarantine_path(root);
        Self {
            candidate_wrapper_tree: snapshot_tree(&candidate_wrapper_path),
            reservation_wrapper_tree: snapshot_tree(&reservation_wrapper_path),
            live_tree: snapshot_tree(&live),
            candidate_wrapper: directory_identity(&candidate_wrapper_path),
            reservation_wrapper: directory_identity(&reservation_wrapper_path),
            candidate: directory_identity(&candidate),
            marker: file_identity(&candidate.join(".cast-tree-id")),
            state_id: file_identity(&candidate.join(".stateID")),
            live: directory_identity(&live),
            roots: directory_identity(&roots),
            quarantine: directory_identity(&quarantine),
            root_names: directory_names(&roots),
            quarantine_names: directory_names(&quarantine),
            other_roots: child_trees_except(&roots, OsStr::new("staging")),
            other_quarantine: child_trees_except(&quarantine, reservation_wrapper_path.file_name().unwrap()),
            root_abi: capture_root_abi(root),
            candidate_state: fs::read_to_string(candidate.join(".stateID")).unwrap(),
            live_state: fs::read_to_string(live.join(".stateID")).unwrap(),
        }
    }

    pub(super) fn assert_preserved(&self, root: &Path, record: &TransitionRecord, wrapper_index: usize) {
        assert_preserved_topology(root, record, wrapper_index);
        let candidate_wrapper = wrapper_path(root, record, wrapper_index);
        let reservation_wrapper = staging_wrapper_path(root);
        let candidate = candidate_wrapper.join("usr");
        let live = root.join("usr");
        let roots = roots_path(root);
        let quarantine = quarantine_path(root);
        assert_eq!(snapshot_tree(&candidate_wrapper), self.candidate_wrapper_tree);
        assert_eq!(snapshot_tree(&reservation_wrapper), self.reservation_wrapper_tree);
        assert_eq!(snapshot_tree(&live), self.live_tree);
        assert_eq!(directory_identity(&candidate_wrapper), self.candidate_wrapper);
        assert_eq!(directory_identity(&reservation_wrapper), self.reservation_wrapper);
        assert_eq!(directory_identity(&candidate), self.candidate);
        assert_eq!(file_identity(&candidate.join(".cast-tree-id")), self.marker);
        assert_eq!(file_identity(&candidate.join(".stateID")), self.state_id);
        assert_eq!(directory_identity(&live), self.live);
        assert_eq!(directory_identity(&roots), self.roots);
        assert_eq!(directory_identity(&quarantine), self.quarantine);
        assert_eq!(directory_names(&roots), self.root_names);
        assert_eq!(directory_names(&quarantine), self.quarantine_names);
        assert_eq!(child_trees_except(&roots, OsStr::new("staging")), self.other_roots);
        assert_eq!(
            child_trees_except(&quarantine, candidate_wrapper.file_name().unwrap()),
            self.other_quarantine
        );
        assert_eq!(capture_root_abi(root), self.root_abi);
        assert_eq!(
            fs::read_to_string(candidate.join(".stateID")).unwrap(),
            self.candidate_state
        );
        assert_eq!(fs::read_to_string(live.join(".stateID")).unwrap(), self.live_state);
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
    assert_eq!(record.operation, Operation::ActiveReblit);
    assert_eq!(record.phase, Phase::CandidatePreserveIntent);
    assert_eq!(record.candidate.origin, JournalCandidateOrigin::ActiveReblit);
    assert_eq!(record.previous.origin, PreviousOrigin::ActiveReblitCorrupt);
    assert!(record.candidate.id.is_some());
    assert_eq!(record.candidate.id, record.previous.id);
    assert_ne!(record.candidate.tree_token, record.previous.tree_token);
    epoch.validate(record);
    let rollback = record.rollback.as_ref().unwrap();
    assert_eq!(rollback.source, source.phase());
    assert_eq!(rollback.previous_archive, RollbackAction::NotRequired);
    assert_eq!(rollback.candidate.action, RollbackAction::Pending);
    assert_eq!(rollback.candidate.disposition, AbortDisposition::Quarantine);
    assert_eq!(rollback.fresh_db, RollbackAction::NotRequired);
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
    wrapper_index: usize,
) -> Vec<ActiveReblitCandidatePreservePostExchangeDurabilityEvent> {
    let candidate_wrapper = wrapper_path(root, record, wrapper_index);
    let candidate = directory_identity(&candidate_wrapper.join("usr"));
    let candidate_wrapper = directory_identity(&candidate_wrapper);
    let reservation_wrapper = directory_identity(&staging_wrapper_path(root));
    let roots = directory_identity(&roots_path(root));
    let quarantine = directory_identity(&quarantine_path(root));
    vec![
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::CandidateSynced {
            device: candidate.0,
            inode: candidate.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::CandidateWrapperSynced {
            device: candidate_wrapper.0,
            inode: candidate_wrapper.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::ReservationWrapperSynced {
            device: reservation_wrapper.0,
            inode: reservation_wrapper.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::RootsParentSynced {
            device: roots.0,
            inode: roots.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::QuarantineParentSynced {
            device: quarantine.0,
            inode: quarantine.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::FinalPostProven,
    ]
}

pub(super) fn assert_staged_topology(root: &Path, record: &TransitionRecord, wrapper_index: usize) {
    let staging = staging_wrapper_path(root);
    let target = wrapper_path(root, record, wrapper_index);
    assert_private_directory(&staging);
    assert_private_directory(&target);
    assert_directory_names(&staging, &[OsStr::new("usr")]);
    assert_directory_names(&target, &[]);
    assert_candidate_tree(root, record, &staging.join("usr"));
}

pub(super) fn assert_preserved_topology(root: &Path, record: &TransitionRecord, wrapper_index: usize) {
    let staging = staging_wrapper_path(root);
    let target = wrapper_path(root, record, wrapper_index);
    assert_private_directory(&staging);
    assert_private_directory(&target);
    assert_directory_names(&staging, &[]);
    assert_directory_names(&target, &[OsStr::new("usr")]);
    assert_candidate_tree(root, record, &target.join("usr"));
}

fn assert_candidate_tree(root: &Path, record: &TransitionRecord, candidate: &Path) {
    assert!(candidate.is_dir());
    assert_eq!(
        fs::read_to_string(candidate.join(".stateID")).unwrap(),
        record.candidate.id.unwrap().to_string()
    );
    assert_eq!(
        fs::read_to_string(root.join("usr/.stateID")).unwrap(),
        record.previous.id.unwrap().to_string()
    );
}

fn assert_private_directory(path: &Path) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_dir(), "{} is not a directory", path.display());
    assert_eq!(metadata.mode() & 0o7777, 0o700, "{} mode drifted", path.display());
}

fn assert_directory_names(directory: &Path, expected: &[&OsStr]) {
    let actual = directory_names(directory);
    let mut expected = expected.iter().map(|name| OsString::from(*name)).collect::<Vec<_>>();
    expected.sort();
    assert_eq!(actual, expected, "unexpected entries in {}", directory.display());
}

fn directory_names(directory: &Path) -> Vec<OsString> {
    let mut names = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn child_trees_except(parent: &Path, excluded: &OsStr) -> Vec<(OsString, Vec<TreeEntry>)> {
    let mut children = fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_name() != excluded)
        .map(|entry| (entry.file_name(), snapshot_tree(&entry.path())))
        .collect::<Vec<_>>();
    children.sort_by(|left, right| left.0.cmp(&right.0));
    children
}

fn capture_root_abi(root: &Path) -> Vec<(String, Option<(PathBuf, (u64, u64))>)> {
    ROOT_ABI
        .into_iter()
        .map(|(name, expected)| {
            let path = root.join(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    assert!(metadata.file_type().is_symlink(), "{} is not a symlink", path.display());
                    let target = fs::read_link(&path).unwrap();
                    assert_eq!(target, Path::new(expected));
                    (name.to_owned(), Some((target, (metadata.dev(), metadata.ino()))))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => (name.to_owned(), None),
                Err(error) => panic!("inspect root ABI entry {}: {error}", path.display()),
            }
        })
        .collect()
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
        panic!("unexpected wrapper tree entry kind at {}", path.display());
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

pub(super) fn capture_database_at_root(root: &Path, record: &TransitionRecord) -> ExistingCandidateDatabase {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    ExistingCandidateDatabase::capture(&database, record)
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
    boundary: CandidateWrapperExchangeKillBoundary,
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
    boundary: CandidateWrapperExchangeKillBoundary,
    dimensions: MatrixDimensions,
    record: &TransitionRecord,
    source_bytes: &[u8],
) {
    fs::write(
        control.join("case"),
        format!(
            "v1\n{}\n{}\n{}\n{:?}\n{}\n{}\n{}\n",
            epoch.as_str(),
            source.as_str(),
            boundary.as_str(),
            dimensions.usr_outcome,
            dimensions.wrapper_index,
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
    assert_eq!(
        names, expected,
        "unexpected ActiveReblit wrapper-exchange journal inventory"
    );
}

pub(super) fn canonical_path(root: &Path) -> PathBuf {
    root.join(CAST_NAME).join(JOURNAL_NAME).join(CANONICAL_NAME)
}

fn roots_path(root: &Path) -> PathBuf {
    root.join(CAST_NAME).join("root")
}

fn staging_wrapper_path(root: &Path) -> PathBuf {
    roots_path(root).join("staging")
}

fn quarantine_path(root: &Path) -> PathBuf {
    root.join(CAST_NAME).join("quarantine")
}

fn wrapper_path(root: &Path, record: &TransitionRecord, wrapper_index: usize) -> PathBuf {
    quarantine_path(root).join(format!(
        "replaced-active-reblit-wrapper-{}-{}-{wrapper_index}",
        record.previous.id.unwrap(),
        record.previous.tree_token.as_str(),
    ))
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
        .unwrap_or_else(|| panic!("required ActiveReblit wrapper-exchange environment {name} is missing"))
        .into_string()
        .unwrap_or_else(|_| panic!("ActiveReblit wrapper-exchange environment {name} is not UTF-8"))
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
        "ActiveReblit wrapper-exchange control must remain outside the installation"
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

pub(super) fn kill_after_real_wrapper_exchange() {
    assert_eq!(
        active_reblit_candidate_preserve_exchange_attempt_count(),
        1,
        "SIGKILL seam must follow exactly one genuine whole-wrapper exchange attempt"
    );
    let case = ChildCase::from_environment();
    let record = case.source_record();
    assert_preserved_topology(&case.root, &record, case.dimensions().wrapper_index);
    let expected = expected_post_events(&case.root, &record, case.dimensions().wrapper_index);
    let actual = take_active_reblit_candidate_preserve_post_exchange_durability_events();
    assert_eq!(
        actual,
        expected[..case.boundary.expected_event_prefix_len()],
        "ActiveReblit crash boundary observed the wrong durability prefix"
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
