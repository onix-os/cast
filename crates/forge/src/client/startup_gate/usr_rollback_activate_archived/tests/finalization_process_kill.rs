//! Real-process restart proof for terminal ActivateArchived journal deletion.

use std::{
    env,
    ffi::OsString,
    fs, io,
    os::unix::{fs::MetadataExt as _, process::ExitStatusExt as _},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    Installation, State,
    client::{
        MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal, active_state_snapshot::ActiveStateReservation,
        snapshot_startup_recovery_namespace, startup_gate::CleanSystemStartup,
        startup_recovery::arm_before_usr_rollback_activate_archived_finalization_final_revalidation,
    },
    db,
    state::Id,
    transition_journal::{
        AbortDisposition, BootRollback, CandidateOrigin, ForwardPhase, JournalDeleteDurabilityBoundary,
        JournalUpdateDurabilityBoundary, Operation, Phase, PreviousOrigin, RollbackAction, RollbackActionOutcome,
        StorageError, TransitionJournalStore, TransitionRecord, arm_journal_delete_durability_callback,
        arm_journal_update_durability_callback, decode, encode,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOutcome, Epoch, RouteFixture, candidate_move_count, install_persistent_route_database,
        open_layout_database, open_state_database, persist_rollback_complete, release_route_handles,
        reset_candidate_observers,
    },
};

const TEST_NAME: &str = concat!(
    "client::startup_gate::usr_rollback_activate_archived::tests::finalization_process_kill::",
    "startup_activate_archived_finalization_process_kills_restart_cleanly",
);
const ROLE_ENV: &str = "CAST_FORGE_ACTIVATE_ARCHIVED_FINALIZATION_KILL_ROLE";
const EPOCH_ENV: &str = "CAST_FORGE_ACTIVATE_ARCHIVED_FINALIZATION_KILL_EPOCH";
const SOURCE_ENV: &str = "CAST_FORGE_ACTIVATE_ARCHIVED_FINALIZATION_KILL_SOURCE";
const BOUNDARY_ENV: &str = "CAST_FORGE_ACTIVATE_ARCHIVED_FINALIZATION_KILL_BOUNDARY";
const ROOT_ENV: &str = "CAST_FORGE_ACTIVATE_ARCHIVED_FINALIZATION_KILL_ROOT";
const CONTROL_ENV: &str = "CAST_FORGE_ACTIVATE_ARCHIVED_FINALIZATION_KILL_CONTROL";
const CHILD_DEADLINE: Duration = Duration::from_secs(15);
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
enum ProcessRole {
    Crash,
    Recover,
}

impl ProcessRole {
    fn parse(value: &str) -> Self {
        match value {
            "crash" => Self::Crash,
            "recover" => Self::Recover,
            other => panic!("invalid ActivateArchived finalization process role {other:?}"),
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
            other => panic!("invalid ActivateArchived finalization epoch {other:?}"),
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
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "intent" => Self::Intent,
            "exchanged" => Self::Exchanged,
            other => panic!("invalid ActivateArchived finalization source {other:?}"),
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
enum FinalizationKillBoundary {
    FinalPreRevalidation,
    CanonicalUnlinked,
    DeleteDirectorySynced,
}

impl FinalizationKillBoundary {
    const ALL: [Self; 3] = [
        Self::FinalPreRevalidation,
        Self::CanonicalUnlinked,
        Self::DeleteDirectorySynced,
    ];

    fn parse(value: &str) -> Self {
        match value {
            "final-pre-revalidation" => Self::FinalPreRevalidation,
            "canonical-unlinked" => Self::CanonicalUnlinked,
            "delete-directory-synced" => Self::DeleteDirectorySynced,
            other => panic!("invalid ActivateArchived finalization kill boundary {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::FinalPreRevalidation => "final-pre-revalidation",
            Self::CanonicalUnlinked => "canonical-unlinked",
            Self::DeleteDirectorySynced => "delete-directory-synced",
        }
    }

    fn canonical_survives(self) -> bool {
        self == Self::FinalPreRevalidation
    }

    fn arm_kill(self) {
        match self {
            Self::FinalPreRevalidation => {
                arm_before_usr_rollback_activate_archived_finalization_final_revalidation(kill_self)
            }
            Self::CanonicalUnlinked => {
                arm_journal_delete_durability_callback(JournalDeleteDurabilityBoundary::CanonicalUnlinked, kill_self)
            }
            Self::DeleteDirectorySynced => arm_journal_delete_durability_callback(
                JournalDeleteDurabilityBoundary::DeleteDirectorySynced,
                kill_self,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MatrixDimensions {
    usr_outcome: RollbackActionOutcome,
    candidate_outcome: CandidateOutcome,
}

impl MatrixDimensions {
    fn for_case(epoch: ProcessEpoch, source: ProcessSource) -> Self {
        match (epoch, source) {
            (ProcessEpoch::Current, ProcessSource::Intent) => Self {
                usr_outcome: RollbackActionOutcome::Applied,
                candidate_outcome: CandidateOutcome::Applied,
            },
            (ProcessEpoch::Current, ProcessSource::Exchanged) => Self {
                usr_outcome: RollbackActionOutcome::Applied,
                candidate_outcome: CandidateOutcome::AlreadySatisfied,
            },
            (ProcessEpoch::Historical, ProcessSource::Intent) => Self {
                usr_outcome: RollbackActionOutcome::AlreadySatisfied,
                candidate_outcome: CandidateOutcome::Applied,
            },
            (ProcessEpoch::Historical, ProcessSource::Exchanged) => Self {
                usr_outcome: RollbackActionOutcome::AlreadySatisfied,
                candidate_outcome: CandidateOutcome::AlreadySatisfied,
            },
        }
    }

    fn validate(self, record: &TransitionRecord) {
        let rollback = record.rollback.as_ref().unwrap();
        assert_eq!(rollback.usr_exchange, recorded_action(self.usr_outcome));
        assert_eq!(
            rollback.candidate.action,
            recorded_action(self.candidate_outcome.outcome())
        );
    }
}

#[derive(Debug)]
struct ChildCase {
    role: ProcessRole,
    epoch: ProcessEpoch,
    source: ProcessSource,
    boundary: FinalizationKillBoundary,
    root: PathBuf,
    control: PathBuf,
}

impl ChildCase {
    fn from_environment() -> Self {
        let role = ProcessRole::parse(&required_unicode_env(ROLE_ENV));
        let epoch = ProcessEpoch::parse(&required_unicode_env(EPOCH_ENV));
        let source = ProcessSource::parse(&required_unicode_env(SOURCE_ENV));
        let boundary = FinalizationKillBoundary::parse(&required_unicode_env(BOUNDARY_ENV));
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

    fn terminal_record(&self) -> TransitionRecord {
        let bytes = fs::read(self.control.join("terminal-record")).unwrap();
        let record = decode(&bytes).unwrap();
        assert_eq!(encode(&record).unwrap(), bytes);
        assert_eq!(record.operation, Operation::ActivateArchived);
        assert_eq!(record.phase, Phase::RollbackComplete);
        assert_eq!(record.candidate.origin, CandidateOrigin::Archived);
        assert_eq!(record.previous.origin, PreviousOrigin::ActiveState);
        assert!(record.candidate.id.is_some());
        assert!(record.previous.id.is_some());
        assert_ne!(record.candidate.id, record.previous.id);
        self.epoch.validate(&record);
        let dimensions = self.dimensions();
        let rollback = record.rollback.as_ref().unwrap();
        assert_eq!(rollback.source, self.source.phase());
        assert_eq!(rollback.previous_archive, RollbackAction::NotRequired);
        assert_eq!(rollback.candidate.disposition, AbortDisposition::Rearchive);
        assert_eq!(rollback.fresh_db, RollbackAction::NotRequired);
        assert_eq!(rollback.boot, BootRollback::NotRequired);
        assert!(!rollback.external_effects_may_remain);
        dimensions.validate(&record);
        let expected_case = format!(
            "v1\n{}\n{}\n{}\n{:?}\n{:?}\n{}\n{}\n",
            self.epoch.as_str(),
            self.source.as_str(),
            self.boundary.as_str(),
            dimensions.usr_outcome,
            dimensions.candidate_outcome,
            record.transition_id,
            record.generation,
        );
        assert_eq!(
            fs::read_to_string(self.control.join("case")).unwrap(),
            expected_case,
            "external ActivateArchived process-kill control does not match the terminal case"
        );
        assert_archived_topology(&self.root, &record);
        record
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PublicJournalIdentity {
    cast: (u64, u64),
    journal: (u64, u64),
    lock: (u64, u64),
    canonical: Option<(u64, u64)>,
}

impl PublicJournalIdentity {
    fn capture(root: &Path, canonical_present: bool) -> Self {
        let cast = root.join(CAST_NAME);
        let journal = cast.join(JOURNAL_NAME);
        let canonical = journal.join(CANONICAL_NAME);
        assert_journal_inventory(root, canonical_present);
        Self {
            cast: directory_identity(&cast),
            journal: directory_identity(&journal),
            lock: file_identity(&journal.join(LOCK_NAME)),
            canonical: canonical_present.then(|| file_identity(&canonical)),
        }
    }

    fn assert_same_public_anchors(self, actual: Self) {
        assert_eq!(actual.cast, self.cast);
        assert_eq!(actual.journal, self.journal);
        assert_eq!(actual.lock, self.lock);
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ExistingArchivedDatabase {
    states: Vec<State>,
    in_flight: Option<db::state::InFlightTransition>,
    candidate_ownership: db::state::TransitionOwnership,
    previous_ownership: db::state::TransitionOwnership,
    candidate_provenance: db::state::MetadataProvenance,
    previous_provenance: Option<db::state::MetadataProvenance>,
}

impl ExistingArchivedDatabase {
    fn capture(database: &db::state::Database, record: &TransitionRecord) -> Self {
        assert_ne!(record.candidate.id, record.previous.id);
        let candidate = Id::from(record.candidate.id.unwrap());
        let previous = Id::from(record.previous.id.unwrap());
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

#[test]
fn startup_activate_archived_finalization_process_kills_restart_cleanly() {
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
            for boundary in FinalizationKillBoundary::ALL {
                run_parent_case(epoch, source, boundary);
                cases += 1;
            }
        }
    }
    assert_eq!(
        cases, 12,
        "ActivateArchived finalization SIGKILL matrix must remain exactly 2 x 2 x 3"
    );
}

fn run_parent_case(epoch: Epoch, source: CandidateSource, boundary: FinalizationKillBoundary) {
    let process_epoch = ProcessEpoch::from_fixture(epoch);
    let process_source = ProcessSource::from_fixture(source);
    let dimensions = MatrixDimensions::for_case(process_epoch, process_source);
    let mut fixture = RouteFixture::new(epoch, source, dimensions.usr_outcome, dimensions.candidate_outcome);
    let terminal = persist_rollback_complete(&fixture);
    dimensions.validate(&terminal);
    install_persistent_route_database(&mut fixture);

    let root = fs::canonicalize(&fixture.fixture.fixture.installation.root).unwrap();
    let terminal_bytes = fs::read(canonical_path(&root)).unwrap();
    assert_eq!(terminal_bytes, encode(&terminal).unwrap());
    let public_before = PublicJournalIdentity::capture(&root, true);
    let database_before = ExistingArchivedDatabase::capture(&fixture.fixture.fixture.database, &terminal);
    let namespace_before = snapshot_startup_recovery_namespace(&root);
    fixture.assert_exact_archived_topology();
    assert_archived_topology(&root, &terminal);

    let control = tempfile::tempdir().unwrap();
    let control_path = fs::canonicalize(control.path()).unwrap();
    assert_separate_control_path(&root, &control_path);
    fs::write(
        control_path.join("case"),
        format!(
            "v1\n{}\n{}\n{}\n{:?}\n{:?}\n{}\n{}\n",
            process_epoch.as_str(),
            process_source.as_str(),
            boundary.as_str(),
            dimensions.usr_outcome,
            dimensions.candidate_outcome,
            terminal.transition_id,
            terminal.generation,
        ),
    )
    .unwrap();
    fs::write(control_path.join("terminal-record"), &terminal_bytes).unwrap();
    let retained_root = release_route_handles(fixture);

    let crash = spawn_child(
        ProcessRole::Crash,
        process_epoch,
        process_source,
        boundary,
        &root,
        &control_path,
    );
    let crash_status = DeadlineChild::new(crash, "ActivateArchived finalization crash child").wait(CHILD_DEADLINE);
    assert_eq!(
        crash_status.signal(),
        Some(nix::libc::SIGKILL),
        "crash child for {process_epoch:?} {process_source:?} {boundary:?} missed its boundary: {crash_status:?}"
    );
    assert_journal_after_crash(&root, boundary, &terminal_bytes, public_before);
    assert_eq!(snapshot_startup_recovery_namespace(&root), namespace_before);
    assert_eq!(capture_database_at_root(&root, &terminal), database_before);
    assert_archived_topology(&root, &terminal);

    let recovery = spawn_child(
        ProcessRole::Recover,
        process_epoch,
        process_source,
        boundary,
        &root,
        &control_path,
    );
    let recovery_status =
        DeadlineChild::new(recovery, "ActivateArchived finalization recovery child").wait(CHILD_DEADLINE);
    assert!(
        recovery_status.success(),
        "recovery child failed for {process_epoch:?} {process_source:?} {boundary:?}: {recovery_status:?}"
    );
    assert_eq!(recovery_status.signal(), None);

    let final_public = PublicJournalIdentity::capture(&root, false);
    public_before.assert_same_public_anchors(final_public);
    assert_eq!(snapshot_startup_recovery_namespace(&root), namespace_before);
    assert_eq!(capture_database_at_root(&root, &terminal), database_before);
    assert_archived_topology(&root, &terminal);

    drop(retained_root);
    drop(control);
}

fn run_child(case: ChildCase) {
    let terminal = case.terminal_record();
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
    ExistingArchivedDatabase::capture(system.state_db(), &terminal);
    match case.role {
        ProcessRole::Crash => run_crash_child(&case, &system, &terminal),
        ProcessRole::Recover => run_recovery_child(&case, &system, &terminal),
    }
}

fn run_crash_child(case: &ChildCase, system: &MutableSystemCapabilities, terminal: &TransitionRecord) {
    assert_eq!(
        decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(),
        *terminal
    );
    assert_journal_inventory(&case.root, true);
    assert_archived_topology(&case.root, terminal);
    reset_candidate_observers();
    assert_eq!(candidate_move_count(), 0);
    arm_journal_update_durability_callback(JournalUpdateDurabilityBoundary::TemporaryFullySynced, || {
        panic!("ActivateArchived terminal crash path attempted a journal update")
    });
    case.boundary.arm_kill();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let result = CleanSystemStartup::enter(system, &reservation);
    panic!(
        "crash child escaped ActivateArchived finalization boundary {:?} with startup success={} error={:?}",
        case.boundary,
        result.is_ok(),
        result.err(),
    );
}

fn run_recovery_child(case: &ChildCase, system: &MutableSystemCapabilities, terminal: &TransitionRecord) {
    let installation = system.installation();
    let database = system.state_db();
    if case.boundary.canonical_survives() {
        assert_eq!(
            decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(),
            *terminal
        );
    } else {
        assert!(!canonical_path(&case.root).exists());
    }
    let database_before = ExistingArchivedDatabase::capture(database, terminal);
    let namespace_before = snapshot_startup_recovery_namespace(&case.root);
    assert_archived_topology(&case.root, terminal);
    reset_candidate_observers();
    assert_eq!(candidate_move_count(), 0);
    arm_journal_update_durability_callback(JournalUpdateDurabilityBoundary::TemporaryFullySynced, || {
        panic!("ActivateArchived terminal recovery attempted a journal update")
    });

    let reservation = ActiveStateReservation::acquire().unwrap();
    let clean = CleanSystemStartup::enter(system, &reservation)
        .unwrap_or_else(|error| panic!("ActivateArchived terminal restart did not admit clean startup: {error:?}"));

    assert!(!canonical_path(&case.root).exists());
    assert_eq!(candidate_move_count(), 0);
    assert_eq!(ExistingArchivedDatabase::capture(database, terminal), database_before);
    assert_eq!(snapshot_startup_recovery_namespace(&case.root), namespace_before);
    assert_archived_topology(&case.root, terminal);

    let cast = installation.retained_mutable_cast_directory().unwrap();
    let competing = TransitionJournalStore::try_open_in_retained_cast(cast, &case.root).unwrap_err();
    assert!(matches!(competing, StorageError::AcquireLock { .. }), "{competing:?}");
    drop(clean);

    let reopened = TransitionJournalStore::try_open_in_retained_cast(cast, &case.root).unwrap();
    assert_eq!(reopened.load().unwrap(), None);
    drop(reopened);
    assert_journal_inventory(&case.root, false);
    assert_eq!(candidate_move_count(), 0);
    assert_eq!(ExistingArchivedDatabase::capture(database, terminal), database_before);
    assert_eq!(snapshot_startup_recovery_namespace(&case.root), namespace_before);
    assert_archived_topology(&case.root, terminal);
}

fn capture_database_at_root(root: &Path, terminal: &TransitionRecord) -> ExistingArchivedDatabase {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    ExistingArchivedDatabase::capture(&database, terminal)
}

fn assert_journal_after_crash(
    root: &Path,
    boundary: FinalizationKillBoundary,
    terminal_bytes: &[u8],
    expected: PublicJournalIdentity,
) {
    let actual = PublicJournalIdentity::capture(root, boundary.canonical_survives());
    expected.assert_same_public_anchors(actual);
    if boundary.canonical_survives() {
        assert_eq!(actual.canonical, expected.canonical);
        assert_eq!(fs::read(canonical_path(root)).unwrap(), terminal_bytes);
    } else {
        assert_eq!(actual.canonical, None);
        assert!(!canonical_path(root).exists());
    }
}

fn assert_archived_topology(root: &Path, record: &TransitionRecord) {
    assert_eq!(record.operation, Operation::ActivateArchived);
    assert_eq!(record.phase, Phase::RollbackComplete);
    let candidate = record.candidate.id.unwrap();
    let previous = record.previous.id.unwrap();
    assert_ne!(candidate, previous);

    let wrapper = root.join(CAST_NAME).join("root").join(candidate.to_string());
    let wrapper_metadata = fs::symlink_metadata(&wrapper).unwrap();
    assert!(
        wrapper_metadata.is_dir(),
        "{} is not the archived wrapper",
        wrapper.display()
    );
    let usr = wrapper.join("usr");
    assert!(fs::symlink_metadata(&usr).unwrap().is_dir());
    let marker = usr.join(".cast-tree-id");
    let slot = wrapper.join(format!(
        ".cast-state-slot-{candidate}-{}",
        record.candidate.tree_token.as_str()
    ));
    let marker_metadata = fs::symlink_metadata(&marker).unwrap();
    let slot_metadata = fs::symlink_metadata(&slot).unwrap();
    assert!(marker_metadata.is_file());
    assert!(slot_metadata.is_file());
    assert_eq!(
        (slot_metadata.dev(), slot_metadata.ino()),
        (marker_metadata.dev(), marker_metadata.ino())
    );
    assert_eq!(marker_metadata.nlink(), 2);
    assert_eq!(slot_metadata.nlink(), 2);

    let mut wrapper_names = fs::read_dir(&wrapper)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    wrapper_names.sort();
    let mut expected_names = vec![OsString::from("usr"), slot.file_name().unwrap().to_owned()];
    expected_names.sort();
    assert_eq!(wrapper_names, expected_names);
    assert!(
        fs::read_dir(root.join(CAST_NAME).join("root/staging"))
            .unwrap()
            .next()
            .is_none()
    );
    assert!(
        !root
            .join(CAST_NAME)
            .join("quarantine")
            .join(record.quarantine_name.as_str())
            .exists()
    );
    assert_eq!(
        fs::read_to_string(root.join("usr/.stateID")).unwrap(),
        previous.to_string()
    );
    for (name, target) in ROOT_ABI {
        let link = root.join(name);
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(link).unwrap(), Path::new(target));
    }
}

fn recorded_action(outcome: RollbackActionOutcome) -> RollbackAction {
    match outcome {
        RollbackActionOutcome::Applied => RollbackAction::Applied,
        RollbackActionOutcome::AlreadySatisfied => RollbackAction::AlreadySatisfied,
    }
}

fn spawn_child(
    role: ProcessRole,
    epoch: ProcessEpoch,
    source: ProcessSource,
    boundary: FinalizationKillBoundary,
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

fn assert_journal_inventory(root: &Path, canonical_present: bool) {
    let mut names = fs::read_dir(root.join(CAST_NAME).join(JOURNAL_NAME))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    let mut expected = vec![OsString::from(LOCK_NAME)];
    if canonical_present {
        expected.push(OsString::from(CANONICAL_NAME));
        expected.sort();
    }
    assert_eq!(names, expected, "unexpected ActivateArchived journal inventory");
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

fn required_unicode_env(name: &str) -> String {
    env::var_os(name)
        .unwrap_or_else(|| panic!("required ActivateArchived finalization environment {name} is missing"))
        .into_string()
        .unwrap_or_else(|_| panic!("ActivateArchived finalization environment {name} is not UTF-8"))
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
        "ActivateArchived process-kill control must remain outside the installation"
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

fn kill_self() {
    // This is real same-boot process death, not a reboot or power-loss oracle.
    // A historical record epoch is likewise not a reboot simulation.
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
