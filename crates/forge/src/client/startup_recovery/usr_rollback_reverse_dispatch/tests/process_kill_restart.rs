use std::{
    env, fs, io,
    os::unix::{fs::MetadataExt as _, process::ExitStatusExt as _},
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
        startup_reconciliation::{
            arm_before_reverse_exchange_reconciliation_capture,
            arm_before_usr_rollback_reverse_namespace_final_pre_capture,
            arm_before_usr_rollback_reverse_namespace_installation_root_sync,
        },
        startup_recovery::arm_before_usr_rollback_reverse_persistence_final_revalidation,
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{Operation, Phase, RollbackActionOutcome, TransitionRecord, decode},
};

use super::support::{
    Fixture, OperationKind, ReverseLayout, assert_candidate_preserve_intent_pending, assert_layout_reversed,
    assert_layout_unchanged, assert_usr_restored_pending, expected_candidate_preserve_intent, expected_usr_restored,
    open_layout_database, open_state_database, persistent_state_database, release_fixture_handles, usr_layout,
    usr_layout_at,
};

const TEST_NAME: &str = concat!(
    "client::startup_recovery::usr_rollback_reverse_dispatch::tests::process_kill_restart::",
    "startup_usr_rollback_reverse_dispatch_process_kills_restart_to_exact_already_satisfied",
);
const ROLE_ENV: &str = "CAST_FORGE_REVERSE_PROCESS_KILL_ROLE";
const OPERATION_ENV: &str = "CAST_FORGE_REVERSE_PROCESS_KILL_OPERATION";
const KILL_POINT_ENV: &str = "CAST_FORGE_REVERSE_PROCESS_KILL_POINT";
const ROOT_ENV: &str = "CAST_FORGE_REVERSE_PROCESS_KILL_ROOT";
const CONTROL_ENV: &str = "CAST_FORGE_REVERSE_PROCESS_KILL_CONTROL";
const CHILD_DEADLINE: Duration = Duration::from_secs(15);

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
            other => panic!("invalid reverse process-kill role {other:?}"),
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
            other => panic!("invalid reverse process-kill operation {other:?}"),
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
enum KillPoint {
    AfterReverseExchange,
    BeforeInstallationRootSync,
    BeforeFinalPreCapture,
    BeforePersistenceFinalRevalidation,
}

impl KillPoint {
    const ALL: [Self; 4] = [
        Self::AfterReverseExchange,
        Self::BeforeInstallationRootSync,
        Self::BeforeFinalPreCapture,
        Self::BeforePersistenceFinalRevalidation,
    ];

    fn parse(value: &str) -> Self {
        match value {
            "after-reverse-exchange" => Self::AfterReverseExchange,
            "before-installation-root-sync" => Self::BeforeInstallationRootSync,
            "before-final-pre-capture" => Self::BeforeFinalPreCapture,
            "before-persistence-final-revalidation" => Self::BeforePersistenceFinalRevalidation,
            other => panic!("invalid reverse process-kill point {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::AfterReverseExchange => "after-reverse-exchange",
            Self::BeforeInstallationRootSync => "before-installation-root-sync",
            Self::BeforeFinalPreCapture => "before-final-pre-capture",
            Self::BeforePersistenceFinalRevalidation => "before-persistence-final-revalidation",
        }
    }

    fn arm_crash(self) {
        match self {
            Self::AfterReverseExchange => arm_before_reverse_exchange_reconciliation_capture(kill_self),
            Self::BeforeInstallationRootSync => {
                arm_before_usr_rollback_reverse_namespace_installation_root_sync(kill_self)
            }
            Self::BeforeFinalPreCapture => arm_before_usr_rollback_reverse_namespace_final_pre_capture(kill_self),
            Self::BeforePersistenceFinalRevalidation => {
                arm_before_usr_rollback_reverse_persistence_final_revalidation(kill_self)
            }
        }
    }
}

#[derive(Debug)]
struct ChildCase {
    role: ProcessRole,
    operation: ProcessOperation,
    kill_point: KillPoint,
    root: PathBuf,
    control: PathBuf,
}

impl ChildCase {
    fn from_environment() -> Self {
        let role = ProcessRole::parse(&required_unicode_env(ROLE_ENV));
        let operation = ProcessOperation::parse(&required_unicode_env(OPERATION_ENV));
        let kill_point = KillPoint::parse(&required_unicode_env(KILL_POINT_ENV));
        let root = canonical_environment_path(ROOT_ENV);
        let control = canonical_environment_path(CONTROL_ENV);
        assert_separate_control_path(&root, &control);
        Self {
            role,
            operation,
            kill_point,
            root,
            control,
        }
    }

    fn validate_control(&self, record: &TransitionRecord) -> RawUsrLayout {
        assert_eq!(record.phase, Phase::ReverseExchangeIntent);
        assert_eq!(record.operation, self.operation.journal_operation());
        let expected_case = format!(
            "v1\n{}\n{}\n{}\n",
            self.operation.as_str(),
            self.kill_point.as_str(),
            record.transition_id
        );
        assert_eq!(
            fs::read_to_string(self.control.join("case")).unwrap(),
            expected_case,
            "external process-kill control does not match the exact journal case"
        );
        RawUsrLayout::read(&self.control.join("post-layout"))
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

    fn reversed(self) -> Self {
        Self {
            live: self.staged,
            staged: self.live,
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
fn startup_usr_rollback_reverse_dispatch_process_kills_restart_to_exact_already_satisfied() {
    match env::var_os(ROLE_ENV) {
        Some(_) => run_child(ChildCase::from_environment()),
        None => run_parent(),
    }
}

fn run_parent() {
    assert_parent_environment_clean();
    for kind in OperationKind::ALL {
        for kill_point in KillPoint::ALL {
            run_parent_case(kind, kill_point);
        }
    }
}

fn run_parent_case(kind: OperationKind, kill_point: KillPoint) {
    let operation = ProcessOperation::from_fixture(kind);
    let mut fixture = Fixture::for_effect(kind, ReverseLayout::Post);
    let root = fs::canonicalize(&fixture.fixture.installation.root).unwrap();
    let source = fixture.record.clone();
    let restored = expected_usr_restored(&fixture, RollbackActionOutcome::AlreadySatisfied);
    let preserve_intent = expected_candidate_preserve_intent(&restored);
    let post_layout = usr_layout(&fixture);
    let raw_post_layout = RawUsrLayout::capture(&root);
    let state_database = persistent_state_database(&fixture, kind);
    let states_before = state_database.all().unwrap();
    let in_flight_before = state_database.audit_in_flight_transition().unwrap();
    let control = tempfile::tempdir().unwrap();
    let control_path = fs::canonicalize(control.path()).unwrap();
    assert_separate_control_path(&root, &control_path);
    fs::write(
        control_path.join("case"),
        format!(
            "v1\n{}\n{}\n{}\n",
            operation.as_str(),
            kill_point.as_str(),
            source.transition_id
        ),
    )
    .unwrap();
    raw_post_layout.write(&control_path.join("post-layout"));
    drop(state_database);
    let replacement_root = release_fixture_handles(&mut fixture);

    let crash = spawn_child(ProcessRole::Crash, operation, kill_point, &root, &control_path);
    let crash_status = DeadlineChild::new(crash, "rollback-reverse crash child").wait(CHILD_DEADLINE);
    assert_eq!(
        crash_status.signal(),
        Some(nix::libc::SIGKILL),
        "crash child for {operation:?} {kill_point:?} was not killed at its armed boundary: {crash_status:?}"
    );
    assert_eq!(canonical_record(&root), source, "{operation:?} {kill_point:?}");
    assert_layout_reversed(post_layout, usr_layout_at(&root));
    assert_eq!(RawUsrLayout::capture(&root), raw_post_layout.reversed());
    assert_root_links_absent_at(&root);
    assert_database_unchanged(&root, &states_before, &in_flight_before);
    let pre_layout = usr_layout_at(&root);

    let recovery = spawn_child(ProcessRole::Recover, operation, kill_point, &root, &control_path);
    let recovery_status = DeadlineChild::new(recovery, "rollback-reverse recovery child").wait(CHILD_DEADLINE);
    assert!(
        recovery_status.success(),
        "recovery child failed for {operation:?} {kill_point:?}: {recovery_status:?}"
    );
    assert_eq!(recovery_status.signal(), None);
    assert_eq!(canonical_record(&root), preserve_intent, "{operation:?} {kill_point:?}");
    assert_layout_unchanged(pre_layout, usr_layout_at(&root));
    assert_eq!(RawUsrLayout::capture(&root), raw_post_layout.reversed());
    assert_root_links_absent_at(&root);
    assert_database_unchanged(&root, &states_before, &in_flight_before);

    drop(fixture);
    drop(replacement_root);
    drop(control);
}

fn run_child(case: ChildCase) {
    let installation = Installation::open(&case.root, None).unwrap();
    assert_eq!(installation.root, case.root);
    let state_database = open_state_database(&installation);
    let layout_database = open_layout_database(&installation);
    let source = canonical_record(&case.root);
    let post_layout = case.validate_control(&source);
    match case.role {
        ProcessRole::Crash => run_crash_child(&case, &installation, &state_database, &layout_database, post_layout),
        ProcessRole::Recover => run_recovery_child(
            &case,
            &installation,
            &state_database,
            &layout_database,
            source,
            post_layout,
        ),
    }
}

fn run_crash_child(
    case: &ChildCase,
    installation: &Installation,
    state_database: &crate::db::state::Database,
    layout_database: &crate::db::layout::Database,
    post_layout: RawUsrLayout,
) {
    assert_eq!(RawUsrLayout::capture(&case.root), post_layout);
    case.kill_point.arm_crash();
    let error = enter_with_handles(installation, state_database, layout_database);
    panic!(
        "crash child escaped armed boundary {:?} with startup result {error:?}",
        case.kill_point
    );
}

fn run_recovery_child(
    case: &ChildCase,
    installation: &Installation,
    state_database: &crate::db::state::Database,
    layout_database: &crate::db::layout::Database,
    source: TransitionRecord,
    post_layout: RawUsrLayout,
) {
    assert_eq!(RawUsrLayout::capture(&case.root), post_layout.reversed());
    let expected = source
        .rollback_successor(Some(RollbackActionOutcome::AlreadySatisfied))
        .expect("exact reverse intent must admit AlreadySatisfied UsrRestored");
    assert_eq!(expected.phase, Phase::UsrRestored);
    let states_before = state_database.all().unwrap();
    let in_flight_before = state_database.audit_in_flight_transition().unwrap();
    let pre_layout = usr_layout_at(&case.root);
    reset_retained_exchange_syscall_count();
    arm_before_reverse_exchange_reconciliation_capture(|| {
        panic!("PRE recovery attempted a second retained /usr exchange")
    });

    let recovered = enter_with_handles(installation, state_database, layout_database);

    assert_usr_restored_pending(&recovered);
    assert_eq!(canonical_record(&case.root), expected);
    assert_eq!(retained_exchange_syscall_count(), 0);
    assert_layout_unchanged(pre_layout, usr_layout_at(&case.root));
    assert_eq!(RawUsrLayout::capture(&case.root), post_layout.reversed());
    assert_eq!(state_database.all().unwrap(), states_before);
    assert_eq!(state_database.audit_in_flight_transition().unwrap(), in_flight_before);
    assert_root_links_absent_at(&case.root);
    drop(recovered);

    let preserve_intent = expected_candidate_preserve_intent(&expected);
    let stable = enter_with_handles(installation, state_database, layout_database);

    assert_candidate_preserve_intent_pending(&stable);
    assert_eq!(canonical_record(&case.root), preserve_intent);
    assert_eq!(retained_exchange_syscall_count(), 0);
    assert_layout_unchanged(pre_layout, usr_layout_at(&case.root));
    assert_eq!(RawUsrLayout::capture(&case.root), post_layout.reversed());
    assert_eq!(state_database.all().unwrap(), states_before);
    assert_eq!(state_database.audit_in_flight_transition().unwrap(), in_flight_before);
    assert_root_links_absent_at(&case.root);
}

fn spawn_child(
    role: ProcessRole,
    operation: ProcessOperation,
    kill_point: KillPoint,
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
        .env(KILL_POINT_ENV, kill_point.as_str())
        .env(ROOT_ENV, root)
        .env(CONTROL_ENV, control)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

fn enter_with_handles(
    installation: &Installation,
    state_database: &crate::db::state::Database,
    layout_database: &crate::db::layout::Database,
) -> startup_gate::Error {
    let reservation = ActiveStateReservation::acquire().unwrap();
    match CleanSystemStartup::enter(installation, state_database, layout_database, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted an unresolved rollback"),
        Err(error) => error,
    }
}

fn canonical_record(root: &Path) -> TransitionRecord {
    decode(&fs::read(root.join(".cast/journal/state-transition")).unwrap()).unwrap()
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
            "rollback-reverse process recovery unexpectedly published root link {name}"
        );
    }
}

fn directory_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_dir(), "{} is not a directory", path.display());
    (metadata.dev(), metadata.ino())
}

fn required_unicode_env(name: &str) -> String {
    env::var_os(name)
        .unwrap_or_else(|| panic!("required reverse process-kill environment {name} is missing"))
        .into_string()
        .unwrap_or_else(|_| panic!("reverse process-kill environment {name} is not UTF-8"))
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
    for name in [OPERATION_ENV, KILL_POINT_ENV, ROOT_ENV, CONTROL_ENV] {
        assert!(
            env::var_os(name).is_none(),
            "parent process inherited child-only environment {name}"
        );
    }
}

fn kill_self() {
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
