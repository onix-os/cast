//! Real-process restart proof for terminal NewState journal deletion.

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
        active_state_snapshot::ActiveStateReservation, snapshot_startup_recovery_namespace,
        startup_gate::CleanSystemStartup, startup_recovery::arm_before_usr_rollback_finalization_final_revalidation,
    },
    db,
    state::{Id, TransitionId},
    transition_journal::{
        ForwardPhase, JournalDeleteDurabilityBoundary, JournalUpdateDurabilityBoundary, Operation, Phase,
        RollbackActionOutcome, StorageError, TransitionJournalStore, TransitionRecord,
        arm_journal_delete_durability_callback, arm_journal_update_durability_callback, decode, encode,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOutcome, Epoch, FreshOutcome, build_fresh_invalidation, effect_counts,
        install_persistent_joint_absence_database, persist_fresh_invalidated, persist_rollback_complete,
        release_invalidation_fixture_handles, reopen_persistent_state_database, reset_namespace_effect_counts,
    },
};

const TEST_NAME: &str = concat!(
    "client::startup_gate::usr_rollback_new_state::tests::terminal_delete_process_kill::",
    "startup_new_state_suffix_terminal_delete_process_kills_restart_cleanly",
);
const ROLE_ENV: &str = "CAST_FORGE_TERMINAL_DELETE_KILL_ROLE";
const EPOCH_ENV: &str = "CAST_FORGE_TERMINAL_DELETE_KILL_EPOCH";
const SOURCE_ENV: &str = "CAST_FORGE_TERMINAL_DELETE_KILL_SOURCE";
const BOUNDARY_ENV: &str = "CAST_FORGE_TERMINAL_DELETE_KILL_BOUNDARY";
const ROOT_ENV: &str = "CAST_FORGE_TERMINAL_DELETE_KILL_ROOT";
const CONTROL_ENV: &str = "CAST_FORGE_TERMINAL_DELETE_KILL_CONTROL";
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
            other => panic!("invalid terminal-delete process role {other:?}"),
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
            other => panic!("invalid terminal-delete process epoch {other:?}"),
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
            other => panic!("invalid terminal-delete process source {other:?}"),
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
enum TerminalDeleteKillBoundary {
    FinalPreRevalidation,
    CanonicalUnlinked,
    DeleteDirectorySynced,
}

impl TerminalDeleteKillBoundary {
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
            other => panic!("invalid terminal-delete process boundary {other:?}"),
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
            Self::FinalPreRevalidation => arm_before_usr_rollback_finalization_final_revalidation(kill_self),
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

#[derive(Debug)]
struct ChildCase {
    role: ProcessRole,
    epoch: ProcessEpoch,
    source: ProcessSource,
    boundary: TerminalDeleteKillBoundary,
    root: PathBuf,
    control: PathBuf,
}

impl ChildCase {
    fn from_environment() -> Self {
        let role = ProcessRole::parse(&required_unicode_env(ROLE_ENV));
        let epoch = ProcessEpoch::parse(&required_unicode_env(EPOCH_ENV));
        let source = ProcessSource::parse(&required_unicode_env(SOURCE_ENV));
        let boundary = TerminalDeleteKillBoundary::parse(&required_unicode_env(BOUNDARY_ENV));
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

    fn terminal_record(&self) -> TransitionRecord {
        let record = decode(&fs::read(self.control.join("terminal-record")).unwrap()).unwrap();
        assert_eq!(record.operation, Operation::NewState);
        assert_eq!(record.phase, Phase::RollbackComplete);
        self.epoch.validate(&record);
        assert_eq!(record.rollback.as_ref().unwrap().source, self.source.phase());
        let expected_case = format!(
            "v1\n{}\n{}\n{}\n{}\n{}\n",
            self.epoch.as_str(),
            self.source.as_str(),
            self.boundary.as_str(),
            record.transition_id,
            record.generation,
        );
        assert_eq!(
            fs::read_to_string(self.control.join("case")).unwrap(),
            expected_case,
            "external process-kill control does not match the terminal rollback case"
        );
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

#[test]
fn startup_new_state_suffix_terminal_delete_process_kills_restart_cleanly() {
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
            for boundary in TerminalDeleteKillBoundary::ALL {
                run_parent_case(epoch, source, boundary);
                cases += 1;
            }
        }
    }
    assert_eq!(
        cases, 12,
        "terminal-delete SIGKILL matrix must remain exactly 2 x 2 x 3"
    );
}

fn run_parent_case(epoch: Epoch, source: CandidateSource, boundary: TerminalDeleteKillBoundary) {
    let process_epoch = ProcessEpoch::from_fixture(epoch);
    let process_source = ProcessSource::from_fixture(source);
    let mut fixture = build_fresh_invalidation(
        epoch,
        source,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
        FreshOutcome::AlreadySatisfied,
    );
    let invalidated = persist_fresh_invalidated(&fixture, FreshOutcome::Applied);
    let terminal = persist_rollback_complete(&fixture, &invalidated);
    install_persistent_joint_absence_database(&mut fixture);

    let root = fs::canonicalize(&fixture.fixture.fixture.installation.root).unwrap();
    let terminal_bytes = fs::read(canonical_path(&root)).unwrap();
    assert_eq!(terminal_bytes, encode(&terminal).unwrap());
    let public_before = PublicJournalIdentity::capture(&root, true);
    let states_before = fixture.fixture.fixture.database.all().unwrap();
    let in_flight_before = fixture.fixture.fixture.database.audit_in_flight_transition().unwrap();
    let candidate = Id::from(terminal.candidate.id.unwrap());
    assert_joint_absence(&fixture.fixture.fixture.database, candidate, &terminal.transition_id);
    let namespace_before = snapshot_startup_recovery_namespace(&root);

    let control = tempfile::tempdir().unwrap();
    let control_path = fs::canonicalize(control.path()).unwrap();
    assert_separate_control_path(&root, &control_path);
    fs::write(
        control_path.join("case"),
        format!(
            "v1\n{}\n{}\n{}\n{}\n{}\n",
            process_epoch.as_str(),
            process_source.as_str(),
            boundary.as_str(),
            terminal.transition_id,
            terminal.generation,
        ),
    )
    .unwrap();
    fs::write(control_path.join("terminal-record"), &terminal_bytes).unwrap();
    let retained_root = release_invalidation_fixture_handles(fixture);

    let crash = spawn_child(
        ProcessRole::Crash,
        process_epoch,
        process_source,
        boundary,
        &root,
        &control_path,
    );
    let crash_status = DeadlineChild::new(crash, "terminal-delete crash child").wait(CHILD_DEADLINE);
    assert_eq!(
        crash_status.signal(),
        Some(nix::libc::SIGKILL),
        "crash child for {process_epoch:?} {process_source:?} {boundary:?} missed its armed boundary: {crash_status:?}"
    );
    assert_journal_after_crash(&root, boundary, &terminal_bytes, public_before);
    assert_eq!(snapshot_startup_recovery_namespace(&root), namespace_before);
    assert_database_unchanged(
        &root,
        candidate,
        &terminal.transition_id,
        &states_before,
        &in_flight_before,
    );
    assert_eq!(snapshot_startup_recovery_namespace(&root), namespace_before);

    let recovery = spawn_child(
        ProcessRole::Recover,
        process_epoch,
        process_source,
        boundary,
        &root,
        &control_path,
    );
    let recovery_status = DeadlineChild::new(recovery, "terminal-delete recovery child").wait(CHILD_DEADLINE);
    assert!(
        recovery_status.success(),
        "recovery child failed for {process_epoch:?} {process_source:?} {boundary:?}: {recovery_status:?}"
    );
    assert_eq!(recovery_status.signal(), None);

    let final_public = PublicJournalIdentity::capture(&root, false);
    public_before.assert_same_public_anchors(final_public);
    assert_eq!(snapshot_startup_recovery_namespace(&root), namespace_before);
    assert_database_unchanged(
        &root,
        candidate,
        &terminal.transition_id,
        &states_before,
        &in_flight_before,
    );
    assert_eq!(snapshot_startup_recovery_namespace(&root), namespace_before);

    drop(retained_root);
    drop(control);
}

fn run_child(case: ChildCase) {
    let terminal = case.terminal_record();
    let installation = Installation::open(&case.root, None).unwrap();
    assert_eq!(installation.root, case.root);
    let database = reopen_persistent_state_database(&installation);
    let layout_database = super::support::open_layout_database(&installation);
    let candidate = Id::from(terminal.candidate.id.unwrap());
    assert_joint_absence(&database, candidate, &terminal.transition_id);
    match case.role {
        ProcessRole::Crash => run_crash_child(&case, &installation, &database, &layout_database, &terminal),
        ProcessRole::Recover => run_recovery_child(&case, &installation, &database, &layout_database, &terminal),
    }
}

fn run_crash_child(
    case: &ChildCase,
    installation: &Installation,
    database: &db::state::Database,
    layout_database: &db::layout::Database,
    terminal: &TransitionRecord,
) {
    assert_eq!(
        decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(),
        *terminal
    );
    assert_journal_inventory(&case.root, true);
    reset_namespace_effect_counts();
    assert_zero_effects();
    arm_journal_update_durability_callback(JournalUpdateDurabilityBoundary::TemporaryFullySynced, || {
        panic!("terminal-delete crash path attempted a journal update")
    });
    case.boundary.arm_kill();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let result = CleanSystemStartup::enter(installation, database, layout_database, &reservation);
    panic!(
        "crash child escaped terminal-delete boundary {:?} with startup success={} error={:?}",
        case.boundary,
        result.is_ok(),
        result.err(),
    );
}

fn run_recovery_child(
    case: &ChildCase,
    installation: &Installation,
    database: &db::state::Database,
    layout_database: &db::layout::Database,
    terminal: &TransitionRecord,
) {
    if case.boundary.canonical_survives() {
        assert_eq!(
            decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(),
            *terminal
        );
    } else {
        assert!(!canonical_path(&case.root).exists());
    }
    let states_before = database.all().unwrap();
    let in_flight_before = database.audit_in_flight_transition().unwrap();
    let namespace_before = snapshot_startup_recovery_namespace(&case.root);
    reset_namespace_effect_counts();
    assert_zero_effects();
    arm_journal_update_durability_callback(JournalUpdateDurabilityBoundary::TemporaryFullySynced, || {
        panic!("terminal-delete recovery attempted a journal update")
    });

    let reservation = ActiveStateReservation::acquire().unwrap();
    let clean = CleanSystemStartup::enter(installation, database, layout_database, &reservation)
        .unwrap_or_else(|error| panic!("terminal-delete restart did not admit clean startup: {error:?}"));

    assert!(!canonical_path(&case.root).exists());
    assert_zero_effects();
    assert_eq!(database.all().unwrap(), states_before);
    assert_eq!(database.audit_in_flight_transition().unwrap(), in_flight_before);
    assert_eq!(snapshot_startup_recovery_namespace(&case.root), namespace_before);

    let cast = installation.retained_mutable_cast_directory().unwrap();
    let competing = TransitionJournalStore::try_open_in_retained_cast(cast, &case.root).unwrap_err();
    assert!(matches!(competing, StorageError::AcquireLock { .. }), "{competing:?}");
    drop(clean);

    let reopened = TransitionJournalStore::try_open_in_retained_cast(cast, &case.root).unwrap();
    assert_eq!(reopened.load().unwrap(), None);
    drop(reopened);
    assert_journal_inventory(&case.root, false);
    assert_zero_effects();
    assert_eq!(database.all().unwrap(), states_before);
    assert_eq!(database.audit_in_flight_transition().unwrap(), in_flight_before);
    assert_eq!(snapshot_startup_recovery_namespace(&case.root), namespace_before);
}

fn assert_journal_after_crash(
    root: &Path,
    boundary: TerminalDeleteKillBoundary,
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

fn assert_database_unchanged(
    root: &Path,
    candidate: Id,
    transition: &TransitionId,
    states: &[State],
    in_flight: &Option<db::state::InFlightTransition>,
) {
    let installation = Installation::open(root, None).unwrap();
    let database = reopen_persistent_state_database(&installation);
    assert_eq!(database.all().unwrap(), states);
    assert_eq!(&database.audit_in_flight_transition().unwrap(), in_flight);
    assert_joint_absence(&database, candidate, transition);
}

fn assert_joint_absence(database: &db::state::Database, candidate: Id, transition: &TransitionId) {
    assert!(matches!(
        database.inspect_exact_fresh_transition(candidate, transition),
        Ok(db::state::ExactFreshTransitionObservation::JointlyAbsent(_))
    ));
}

fn assert_zero_effects() {
    let effects = effect_counts();
    assert_eq!(effects.create, 0);
    assert_eq!(effects.normalize, 0);
    assert_eq!(effects.candidate_move, 0);
    assert_eq!(effects.fresh_removal, 0);
}

fn spawn_child(
    role: ProcessRole,
    epoch: ProcessEpoch,
    source: ProcessSource,
    boundary: TerminalDeleteKillBoundary,
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
    assert_eq!(names, expected, "unexpected public journal inventory");
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
        .unwrap_or_else(|| panic!("required terminal-delete environment {name} is missing"))
        .into_string()
        .unwrap_or_else(|_| panic!("terminal-delete environment {name} is not UTF-8"))
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
        "terminal-delete control must remain outside the installation"
    );
}

fn assert_parent_environment_clean() {
    for name in [EPOCH_ENV, SOURCE_ENV, BOUNDARY_ENV, ROOT_ENV, CONTROL_ENV] {
        assert!(
            env::var_os(name).is_none(),
            "parent process inherited child-only environment {name}"
        );
    }
}

fn kill_self() {
    // This is real process death on the live filesystem, not a reboot or
    // power-loss oracle. Pre-fsync persistence across power loss remains
    // intentionally outside this contract.
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
