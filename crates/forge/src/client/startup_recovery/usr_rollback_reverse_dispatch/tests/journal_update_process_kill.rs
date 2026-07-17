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
    Installation,
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{self, CleanSystemStartup},
        startup_reconciliation::arm_before_reverse_exchange_reconciliation_capture,
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{
        JournalUpdateDurabilityBoundary, Operation, Phase, RollbackActionOutcome, TransitionRecord,
        arm_journal_update_durability_callback, decode, encode,
    },
};

use super::support::{
    Fixture, OperationKind, ReverseLayout, assert_usr_restored_pending, open_state_database, persistent_state_database,
    release_fixture_handles,
};

const TEST_NAME: &str = concat!(
    "client::startup_recovery::usr_rollback_reverse_dispatch::tests::journal_update_process_kill::",
    "startup_usr_rollback_reverse_dispatch_journal_update_process_kills_restart_exactly",
);
const ROLE_ENV: &str = "CAST_FORGE_REVERSE_JOURNAL_KILL_ROLE";
const OPERATION_ENV: &str = "CAST_FORGE_REVERSE_JOURNAL_KILL_OPERATION";
const LAYOUT_ENV: &str = "CAST_FORGE_REVERSE_JOURNAL_KILL_LAYOUT";
const BOUNDARY_ENV: &str = "CAST_FORGE_REVERSE_JOURNAL_KILL_BOUNDARY";
const ROOT_ENV: &str = "CAST_FORGE_REVERSE_JOURNAL_KILL_ROOT";
const CONTROL_ENV: &str = "CAST_FORGE_REVERSE_JOURNAL_KILL_CONTROL";
const CHILD_DEADLINE: Duration = Duration::from_secs(15);
const CANONICAL_NAME: &str = "state-transition";
const LOCK_NAME: &str = "state-transition.lock";
const TEMPORARY_PREFIX: &str = ".state-transition.tmp-";

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
            other => panic!("invalid reverse journal process-kill role {other:?}"),
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
enum ProcessOperation {
    NewState,
    Archived,
    ActiveReblit,
}

impl ProcessOperation {
    fn from_fixture(kind: OperationKind) -> Self {
        match kind {
            OperationKind::NewState => Self::NewState,
            OperationKind::Archived => Self::Archived,
            OperationKind::ActiveReblit => Self::ActiveReblit,
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "new-state" => Self::NewState,
            "archived" => Self::Archived,
            "active-reblit" => Self::ActiveReblit,
            other => panic!("invalid reverse journal process-kill operation {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::NewState => "new-state",
            Self::Archived => "archived",
            Self::ActiveReblit => "active-reblit",
        }
    }

    fn journal_operation(self) -> Operation {
        match self {
            Self::NewState => Operation::NewState,
            Self::Archived => Operation::ActivateArchived,
            Self::ActiveReblit => Operation::ActiveReblit,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessLayout {
    Post,
    Pre,
}

impl ProcessLayout {
    const ALL: [Self; 2] = [Self::Post, Self::Pre];

    fn fixture(self) -> ReverseLayout {
        match self {
            Self::Post => ReverseLayout::Post,
            Self::Pre => ReverseLayout::Pre,
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "post" => Self::Post,
            "pre" => Self::Pre,
            other => panic!("invalid reverse journal process-kill layout {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Post => "post",
            Self::Pre => "pre",
        }
    }

    fn initial_outcome(self) -> RollbackActionOutcome {
        match self {
            Self::Post => RollbackActionOutcome::Applied,
            Self::Pre => RollbackActionOutcome::AlreadySatisfied,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JournalKillBoundary {
    TemporaryFullySynced,
    CanonicalExchanged,
    UpdateFirstDirectorySynced,
    DisplacedUnlinked,
    UpdateFinalDirectorySynced,
}

impl JournalKillBoundary {
    const ALL: [Self; 5] = [
        Self::TemporaryFullySynced,
        Self::CanonicalExchanged,
        Self::UpdateFirstDirectorySynced,
        Self::DisplacedUnlinked,
        Self::UpdateFinalDirectorySynced,
    ];

    fn parse(value: &str) -> Self {
        match value {
            "temporary-fully-synced" => Self::TemporaryFullySynced,
            "canonical-exchanged" => Self::CanonicalExchanged,
            "update-first-directory-synced" => Self::UpdateFirstDirectorySynced,
            "displaced-unlinked" => Self::DisplacedUnlinked,
            "update-final-directory-synced" => Self::UpdateFinalDirectorySynced,
            other => panic!("invalid reverse journal process-kill boundary {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::TemporaryFullySynced => "temporary-fully-synced",
            Self::CanonicalExchanged => "canonical-exchanged",
            Self::UpdateFirstDirectorySynced => "update-first-directory-synced",
            Self::DisplacedUnlinked => "displaced-unlinked",
            Self::UpdateFinalDirectorySynced => "update-final-directory-synced",
        }
    }

    fn durability_boundary(self) -> JournalUpdateDurabilityBoundary {
        match self {
            Self::TemporaryFullySynced => JournalUpdateDurabilityBoundary::TemporaryFullySynced,
            Self::CanonicalExchanged => JournalUpdateDurabilityBoundary::CanonicalExchanged,
            Self::UpdateFirstDirectorySynced => JournalUpdateDurabilityBoundary::UpdateFirstDirectorySynced,
            Self::DisplacedUnlinked => JournalUpdateDurabilityBoundary::DisplacedUnlinked,
            Self::UpdateFinalDirectorySynced => JournalUpdateDurabilityBoundary::UpdateFinalDirectorySynced,
        }
    }

    fn canonical_is_source(self) -> bool {
        self == Self::TemporaryFullySynced
    }

    fn temporary_contains(self) -> Option<TemporaryContents> {
        match self {
            Self::TemporaryFullySynced => Some(TemporaryContents::Successor),
            Self::CanonicalExchanged | Self::UpdateFirstDirectorySynced => Some(TemporaryContents::Source),
            Self::DisplacedUnlinked | Self::UpdateFinalDirectorySynced => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TemporaryContents {
    Source,
    Successor,
}

#[derive(Debug)]
struct ChildCase {
    role: ProcessRole,
    operation: ProcessOperation,
    layout: ProcessLayout,
    boundary: JournalKillBoundary,
    root: PathBuf,
    control: PathBuf,
}

impl ChildCase {
    fn from_environment() -> Self {
        let role = ProcessRole::parse(&required_unicode_env(ROLE_ENV));
        let operation = ProcessOperation::parse(&required_unicode_env(OPERATION_ENV));
        let layout = ProcessLayout::parse(&required_unicode_env(LAYOUT_ENV));
        let boundary = JournalKillBoundary::parse(&required_unicode_env(BOUNDARY_ENV));
        let root = canonical_environment_path(ROOT_ENV);
        let control = canonical_environment_path(CONTROL_ENV);
        assert_separate_control_path(&root, &control);
        Self {
            role,
            operation,
            layout,
            boundary,
            root,
            control,
        }
    }

    fn validate_control(&self, source: &TransitionRecord) -> RawUsrLayout {
        assert_eq!(source.phase, Phase::ReverseExchangeIntent);
        assert_eq!(source.operation, self.operation.journal_operation());
        let expected_case = format!(
            "v1\n{}\n{}\n{}\n{}\n{}\n",
            self.operation.as_str(),
            self.layout.as_str(),
            self.boundary.as_str(),
            source.transition_id,
            source.generation,
        );
        assert_eq!(
            fs::read_to_string(self.control.join("case")).unwrap(),
            expected_case,
            "external process-kill control does not match the exact journal-update case"
        );
        RawUsrLayout::read(&self.control.join("starting-layout"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RawUsrLayout {
    live: (u64, u64),
    staged: (u64, u64),
}

impl RawUsrLayout {
    fn capture(root: &Path) -> Self {
        Self {
            live: directory_identity(&root.join("usr")),
            staged: directory_identity(&root.join(".cast/root/staging/usr")),
        }
    }

    fn after_reverse(self, layout: ProcessLayout) -> Self {
        match layout {
            ProcessLayout::Post => Self {
                live: self.staged,
                staged: self.live,
            },
            ProcessLayout::Pre => self,
        }
    }

    fn write(self, path: &Path) {
        fs::write(path, self.encoded()).unwrap();
    }

    fn read(path: &Path) -> Self {
        let encoded = fs::read_to_string(path).unwrap();
        let values = encoded
            .split_ascii_whitespace()
            .map(|value| {
                value
                    .parse::<u64>()
                    .expect("layout identity must be an unsigned integer")
            })
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 4, "layout identity must contain exactly four fields");
        let layout = Self {
            live: (values[0], values[1]),
            staged: (values[2], values[3]),
        };
        assert_eq!(encoded, layout.encoded(), "layout identity must use canonical encoding");
        layout
    }

    fn encoded(self) -> String {
        format!("{} {}\n{} {}\n", self.live.0, self.live.1, self.staged.0, self.staged.1)
    }
}

#[test]
fn startup_usr_rollback_reverse_dispatch_journal_update_process_kills_restart_exactly() {
    match env::var_os(ROLE_ENV) {
        Some(_) => run_child(ChildCase::from_environment()),
        None => run_parent(),
    }
}

fn run_parent() {
    assert_parent_environment_clean();
    for kind in OperationKind::ALL {
        for layout in ProcessLayout::ALL {
            for boundary in JournalKillBoundary::ALL {
                run_parent_case(kind, layout, boundary);
            }
        }
    }
}

fn run_parent_case(kind: OperationKind, layout: ProcessLayout, boundary: JournalKillBoundary) {
    let operation = ProcessOperation::from_fixture(kind);
    let mut fixture = Fixture::for_effect(kind, layout.fixture());
    let root = fs::canonicalize(&fixture.fixture.installation.root).unwrap();
    let source = fixture.record.clone();
    let published_successor = source
        .rollback_successor(Some(layout.initial_outcome()))
        .expect("reverse intent must admit its exact initial UsrRestored successor");
    let restart_successor = source
        .rollback_successor(Some(RollbackActionOutcome::AlreadySatisfied))
        .expect("PRE restart must admit its exact AlreadySatisfied UsrRestored successor");
    let starting_layout = RawUsrLayout::capture(&root);
    let expected_pre_layout = starting_layout.after_reverse(layout);
    let source_inode = file_identity(&root.join(".cast/journal").join(CANONICAL_NAME));
    let state_database = persistent_state_database(&fixture, kind);
    let states_before = state_database.all().unwrap();
    let in_flight_before = state_database.audit_in_flight_transition().unwrap();
    let control = tempfile::tempdir().unwrap();
    let control_path = fs::canonicalize(control.path()).unwrap();
    assert_separate_control_path(&root, &control_path);
    fs::write(
        control_path.join("case"),
        format!(
            "v1\n{}\n{}\n{}\n{}\n{}\n",
            operation.as_str(),
            layout.as_str(),
            boundary.as_str(),
            source.transition_id,
            source.generation,
        ),
    )
    .unwrap();
    starting_layout.write(&control_path.join("starting-layout"));
    fs::write(control_path.join("source-record"), encode(&source).unwrap()).unwrap();
    drop(state_database);
    let replacement_root = release_fixture_handles(&mut fixture);

    let crash = spawn_child(ProcessRole::Crash, operation, layout, boundary, &root, &control_path);
    let crash_status = DeadlineChild::new(crash, "rollback-reverse journal-update crash child").wait(CHILD_DEADLINE);
    assert_eq!(
        crash_status.signal(),
        Some(nix::libc::SIGKILL),
        "crash child for {operation:?} {layout:?} {boundary:?} was not killed at its armed boundary: {crash_status:?}"
    );

    assert_eq!(RawUsrLayout::capture(&root), expected_pre_layout);
    assert_root_links_absent_at(&root);
    assert_database_unchanged(&root, &states_before, &in_flight_before);
    assert_journal_after_kill(&root, boundary, &source, &published_successor, source_inode);

    let recovery = spawn_child(ProcessRole::Recover, operation, layout, boundary, &root, &control_path);
    let recovery_status =
        DeadlineChild::new(recovery, "rollback-reverse journal-update recovery child").wait(CHILD_DEADLINE);
    assert!(
        recovery_status.success(),
        "recovery child failed for {operation:?} {layout:?} {boundary:?}: {recovery_status:?}"
    );
    assert_eq!(recovery_status.signal(), None);

    let final_successor = if boundary.canonical_is_source() {
        &restart_successor
    } else {
        &published_successor
    };
    assert_eq!(canonical_record(&root), *final_successor);
    assert_clean_journal_directory(&root);
    assert_eq!(RawUsrLayout::capture(&root), expected_pre_layout);
    assert_root_links_absent_at(&root);
    assert_database_unchanged(&root, &states_before, &in_flight_before);

    drop(fixture);
    drop(replacement_root);
    drop(control);
}

fn run_child(case: ChildCase) {
    let source = source_record_for_child(&case);
    let starting_layout = case.validate_control(&source);
    let installation = Installation::open(&case.root, None).unwrap();
    assert_eq!(installation.root, case.root);
    let state_database = open_state_database(&installation);
    match case.role {
        ProcessRole::Crash => run_crash_child(&case, &installation, &state_database, starting_layout),
        ProcessRole::Recover => run_recovery_child(&case, &installation, &state_database, source, starting_layout),
    }
}

fn run_crash_child(
    case: &ChildCase,
    installation: &Installation,
    state_database: &crate::db::state::Database,
    starting_layout: RawUsrLayout,
) {
    assert_eq!(RawUsrLayout::capture(&case.root), starting_layout);
    arm_journal_update_durability_callback(case.boundary.durability_boundary(), kill_self);
    let error = enter_with_handles(installation, state_database);
    panic!(
        "crash child escaped armed journal-update boundary {:?} with startup result {error:?}",
        case.boundary
    );
}

fn run_recovery_child(
    case: &ChildCase,
    installation: &Installation,
    state_database: &crate::db::state::Database,
    source: TransitionRecord,
    starting_layout: RawUsrLayout,
) {
    let published_successor = source
        .rollback_successor(Some(case.layout.initial_outcome()))
        .expect("reverse intent must admit its exact initial UsrRestored successor");
    let restart_successor = source
        .rollback_successor(Some(RollbackActionOutcome::AlreadySatisfied))
        .expect("PRE restart must admit its exact AlreadySatisfied UsrRestored successor");
    let canonical_before = if case.boundary.canonical_is_source() {
        &source
    } else {
        &published_successor
    };
    assert_eq!(canonical_record(&case.root), *canonical_before);
    assert_eq!(
        RawUsrLayout::capture(&case.root),
        starting_layout.after_reverse(case.layout)
    );
    let states_before = state_database.all().unwrap();
    let in_flight_before = state_database.audit_in_flight_transition().unwrap();
    reset_retained_exchange_syscall_count();
    arm_before_reverse_exchange_reconciliation_capture(|| {
        panic!("PRE journal-update recovery attempted a second retained /usr exchange")
    });
    if !case.boundary.canonical_is_source() {
        arm_journal_update_durability_callback(JournalUpdateDurabilityBoundary::TemporaryFullySynced, || {
            panic!("published UsrRestored recovery attempted another journal update")
        });
    }

    let recovered = enter_with_handles(installation, state_database);

    let expected = if case.boundary.canonical_is_source() {
        restart_successor
    } else {
        published_successor
    };
    assert_usr_restored_pending(&recovered);
    assert_eq!(canonical_record(&case.root), expected);
    assert_eq!(retained_exchange_syscall_count(), 0);
    assert_eq!(
        RawUsrLayout::capture(&case.root),
        starting_layout.after_reverse(case.layout)
    );
    assert_eq!(state_database.all().unwrap(), states_before);
    assert_eq!(state_database.audit_in_flight_transition().unwrap(), in_flight_before);
    assert_root_links_absent_at(&case.root);
    assert_clean_journal_directory(&case.root);
    drop(recovered);

    if case.boundary.canonical_is_source() {
        arm_journal_update_durability_callback(JournalUpdateDurabilityBoundary::TemporaryFullySynced, || {
            panic!("stable UsrRestored recovery attempted another journal update")
        });
    }
    let stable = enter_with_handles(installation, state_database);

    assert_usr_restored_pending(&stable);
    assert_eq!(canonical_record(&case.root), expected);
    assert_eq!(retained_exchange_syscall_count(), 0);
    assert_eq!(
        RawUsrLayout::capture(&case.root),
        starting_layout.after_reverse(case.layout)
    );
    assert_eq!(state_database.all().unwrap(), states_before);
    assert_eq!(state_database.audit_in_flight_transition().unwrap(), in_flight_before);
    assert_root_links_absent_at(&case.root);
    assert_clean_journal_directory(&case.root);
}

fn source_record_for_child(case: &ChildCase) -> TransitionRecord {
    let source = decode(&fs::read(case.control.join("source-record")).unwrap()).unwrap();
    if case.role == ProcessRole::Crash || case.boundary.canonical_is_source() {
        assert_eq!(canonical_record(&case.root), source);
    }
    source
}

fn assert_journal_after_kill(
    root: &Path,
    boundary: JournalKillBoundary,
    source: &TransitionRecord,
    successor: &TransitionRecord,
    source_inode: (u64, u64),
) {
    let journal = root.join(".cast/journal");
    let canonical = journal.join(CANONICAL_NAME);
    let expected_canonical = if boundary.canonical_is_source() {
        source
    } else {
        successor
    };
    assert_eq!(decode(&fs::read(&canonical).unwrap()).unwrap(), *expected_canonical);
    let canonical_inode = file_identity(&canonical);
    if boundary.canonical_is_source() {
        assert_eq!(canonical_inode, source_inode);
    } else {
        assert_ne!(canonical_inode, source_inode);
    }

    match boundary.temporary_contains() {
        Some(contents) => {
            let temporary = single_temporary_path(root).expect("journal update must retain one temporary name");
            let expected_temporary = match contents {
                TemporaryContents::Source => source,
                TemporaryContents::Successor => successor,
            };
            assert_eq!(decode(&fs::read(&temporary).unwrap()).unwrap(), *expected_temporary);
            let temporary_inode = file_identity(&temporary);
            match contents {
                TemporaryContents::Source => assert_eq!(temporary_inode, source_inode),
                TemporaryContents::Successor => assert_ne!(temporary_inode, source_inode),
            }
            assert_ne!(temporary_inode, canonical_inode);
            assert_journal_inventory(root, 1);
        }
        None => {
            assert!(single_temporary_path(root).is_none());
            assert_journal_inventory(root, 0);
        }
    }
}

fn spawn_child(
    role: ProcessRole,
    operation: ProcessOperation,
    layout: ProcessLayout,
    boundary: JournalKillBoundary,
    root: &Path,
    control: &Path,
) -> Child {
    Command::new(env::current_exe().unwrap())
        .arg(TEST_NAME)
        .arg("--exact")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(ROLE_ENV, role.as_str())
        .env(OPERATION_ENV, operation.as_str())
        .env(LAYOUT_ENV, layout.as_str())
        .env(BOUNDARY_ENV, boundary.as_str())
        .env(ROOT_ENV, root)
        .env(CONTROL_ENV, control)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

fn enter_with_handles(installation: &Installation, state_database: &crate::db::state::Database) -> startup_gate::Error {
    let reservation = ActiveStateReservation::acquire().unwrap();
    match CleanSystemStartup::enter(installation, state_database, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted an unresolved rollback"),
        Err(error) => error,
    }
}

fn canonical_record(root: &Path) -> TransitionRecord {
    decode(&fs::read(root.join(".cast/journal").join(CANONICAL_NAME)).unwrap()).unwrap()
}

fn single_temporary_path(root: &Path) -> Option<PathBuf> {
    let paths = fs::read_dir(root.join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.file_name().is_some_and(valid_temporary_name))
        .collect::<Vec<_>>();
    assert!(
        paths.len() <= 1,
        "expected at most one journal temporary, got {paths:?}"
    );
    paths.into_iter().next()
}

fn assert_clean_journal_directory(root: &Path) {
    assert_journal_inventory(root, 0);
}

fn assert_journal_inventory(root: &Path, temporary_count: usize) {
    let mut names = fs::read_dir(root.join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names.len(),
        2 + temporary_count,
        "unexpected journal inventory: {names:?}"
    );
    assert!(names.contains(&OsString::from(CANONICAL_NAME)));
    assert!(names.contains(&OsString::from(LOCK_NAME)));
    assert_eq!(
        names.iter().filter(|name| valid_temporary_name(name)).count(),
        temporary_count,
        "unexpected journal temporary inventory: {names:?}"
    );
}

fn valid_temporary_name(name: &OsStr) -> bool {
    let Some(tail) = name.as_bytes().strip_prefix(TEMPORARY_PREFIX.as_bytes()) else {
        return false;
    };
    tail.len() == 8 + 1 + 16
        && tail[8] == b'-'
        && tail[..8]
            .iter()
            .chain(&tail[9..])
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn assert_database_unchanged(
    root: &Path,
    states_before: &[crate::State],
    in_flight_before: &Option<crate::db::state::InFlightTransition>,
) {
    let installation = Installation::open(root, None).unwrap();
    let state_database = open_state_database(&installation);
    assert_eq!(state_database.all().unwrap(), states_before);
    assert_eq!(&state_database.audit_in_flight_transition().unwrap(), in_flight_before);
}

fn assert_root_links_absent_at(root: &Path) {
    for name in ["bin", "sbin", "lib", "lib32", "lib64"] {
        assert!(
            fs::symlink_metadata(root.join(name)).is_err(),
            "rollback-reverse journal process recovery unexpectedly published root link {name}"
        );
    }
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
        .unwrap_or_else(|| panic!("required reverse journal process-kill environment {name} is missing"))
        .into_string()
        .unwrap_or_else(|_| panic!("reverse journal process-kill environment {name} is not UTF-8"))
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
        "process-kill control must remain outside the installation"
    );
}

fn assert_parent_environment_clean() {
    for name in [OPERATION_ENV, LAYOUT_ENV, BOUNDARY_ENV, ROOT_ENV, CONTROL_ENV] {
        assert!(
            env::var_os(name).is_none(),
            "parent process inherited child-only environment {name}"
        );
    }
}

fn kill_self() {
    // This is a real process death with kernel-visible namespace state, not a
    // power-loss oracle. In particular, pre-fsync rename survival across a
    // reboot remains intentionally outside this SIGKILL contract.
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
            let status = self.child.as_mut().unwrap().try_wait().unwrap();
            if let Some(status) = status {
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
