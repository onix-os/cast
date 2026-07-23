//! Re-execution control for RootLinks fresh-database invalidation death tests.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    client::startup_reconciliation::fresh_db_invalidation_removal_call_count,
    db::state::exact_fresh_transition_removal_transaction_attempts,
    transition_journal::{
        AbortDisposition, BootRollback, CandidateOrigin, ForwardPhase, Operation, Phase,
        PreviousOrigin, RollbackAction, RollbackActionOutcome, TransitionRecord, decode, encode,
    },
};

use super::{
    fresh_db_invalidation_process_boundaries::FreshDbInvalidationProcessBoundary,
    support::{CandidateOutcome, Epoch},
};

const TEST_NAME: &str = concat!(
    "client::startup_gate::usr_rollback_new_state::tests::fresh_db_invalidation_process_kill::",
    "startup_root_links_new_state_fresh_db_invalidation_process_kills_recover_exactly",
);
pub(super) const ROLE_ENV: &str = "CAST_FORGE_ROOT_LINKS_INVALIDATION_KILL_ROLE";
const EPOCH_ENV: &str = "CAST_FORGE_ROOT_LINKS_INVALIDATION_KILL_EPOCH";
const BOUNDARY_ENV: &str = "CAST_FORGE_ROOT_LINKS_INVALIDATION_KILL_BOUNDARY";
const ROOT_ENV: &str = "CAST_FORGE_ROOT_LINKS_INVALIDATION_KILL_ROOT";
const CONTROL_ENV: &str = "CAST_FORGE_ROOT_LINKS_INVALIDATION_KILL_CONTROL";
const RECOVERY_OPENER_MARKER: &str = "recovery-next-database-opener";
pub(super) const CHILD_DEADLINE: Duration = Duration::from_secs(15);

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
            other => panic!("invalid RootLinks invalidation process role {other:?}"),
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
            other => panic!("invalid RootLinks invalidation process epoch {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Historical => "historical",
        }
    }

    fn validate(self, record: &TransitionRecord) {
        let runtime = crate::transition_journal::RuntimeEpoch::capture().unwrap();
        match self {
            Self::Current => assert_eq!(record.creation_epoch, runtime),
            Self::Historical => assert_ne!(record.creation_epoch, runtime),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MatrixDimensions {
    pub(super) usr_outcome: RollbackActionOutcome,
    pub(super) candidate_outcome: CandidateOutcome,
}

impl MatrixDimensions {
    pub(super) fn for_epoch(epoch: ProcessEpoch) -> Self {
        match epoch {
            ProcessEpoch::Current => Self {
                usr_outcome: RollbackActionOutcome::Applied,
                candidate_outcome: CandidateOutcome::AlreadySatisfied,
            },
            ProcessEpoch::Historical => Self {
                usr_outcome: RollbackActionOutcome::AlreadySatisfied,
                candidate_outcome: CandidateOutcome::Applied,
            },
        }
    }

    fn validate(self, record: &TransitionRecord) {
        let rollback = record.rollback.as_ref().unwrap();
        assert_eq!(rollback.usr_exchange, recorded_action(self.usr_outcome));
        assert_eq!(
            rollback.candidate.action,
            recorded_action(self.candidate_outcome.journal())
        );
    }
}

#[derive(Debug)]
pub(super) struct ChildCase {
    pub(super) role: ProcessRole,
    epoch: ProcessEpoch,
    pub(super) boundary: FreshDbInvalidationProcessBoundary,
    pub(super) root: PathBuf,
    control: PathBuf,
}

impl ChildCase {
    pub(super) fn from_environment() -> Self {
        let role = ProcessRole::parse(&required_unicode_env(ROLE_ENV));
        let epoch = ProcessEpoch::parse(&required_unicode_env(EPOCH_ENV));
        let boundary = FreshDbInvalidationProcessBoundary::parse(&required_unicode_env(BOUNDARY_ENV));
        let root = canonical_environment_path(ROOT_ENV);
        let control = canonical_environment_path(CONTROL_ENV);
        assert_separate_control_path(&root, &control);
        Self {
            role,
            epoch,
            boundary,
            root,
            control,
        }
    }

    pub(super) fn source_record(&self) -> TransitionRecord {
        let bytes = fs::read(self.control.join("source-record")).unwrap();
        let record = decode(&bytes).unwrap();
        assert_eq!(encode(&record).unwrap(), bytes);
        let dimensions = MatrixDimensions::for_epoch(self.epoch);
        assert_exact_root_links_source(&record, self.epoch, dimensions);
        let expected_case = format!(
            "v1\n{}\nroot-links-complete\n{}\n{:?}\n{:?}\n{}\n{}\n",
            self.epoch.as_str(),
            self.boundary.as_str(),
            dimensions.usr_outcome,
            dimensions.candidate_outcome,
            record.transition_id,
            record.generation,
        );
        assert_eq!(
            fs::read_to_string(self.control.join("case")).unwrap(),
            expected_case,
            "external v1 RootLinks invalidation control does not match the source record bytes"
        );
        record
    }

    pub(super) fn claim_recovery_first_database_open(&self) {
        assert_eq!(self.role, ProcessRole::Recover);
        let marker = self.control.join(RECOVERY_OPENER_MARKER);
        assert_eq!(
            fs::read(&marker).unwrap(),
            b"v1\nrecovery-child-is-next-database-opener\n"
        );
        fs::remove_file(marker).unwrap();
    }
}

pub(super) fn assert_exact_root_links_source(
    record: &TransitionRecord,
    epoch: ProcessEpoch,
    dimensions: MatrixDimensions,
) {
    assert_eq!(record.operation, Operation::NewState);
    assert_eq!(record.phase, Phase::FreshDbInvalidationIntent);
    assert_eq!(record.generation, 16);
    assert_eq!(record.candidate.origin, CandidateOrigin::Fresh);
    assert_eq!(record.previous.origin, PreviousOrigin::ActiveState);
    assert!(record.candidate.id.is_some());
    assert!(record.previous.id.is_some());
    assert_ne!(record.candidate.id, record.previous.id);
    epoch.validate(record);
    let rollback = record.rollback.as_ref().unwrap();
    assert_eq!(rollback.source, ForwardPhase::RootLinksComplete);
    assert_eq!(rollback.previous_archive, RollbackAction::NotRequired);
    assert_eq!(rollback.fresh_db, RollbackAction::Pending);
    assert_eq!(rollback.boot, BootRollback::NotRequired);
    assert_eq!(rollback.candidate.disposition, AbortDisposition::Quarantine);
    assert!(rollback.external_effects_may_remain);
    dimensions.validate(record);
}

pub(super) fn expected_fresh_db_invalidated(
    source: &TransitionRecord,
    outcome: RollbackActionOutcome,
) -> TransitionRecord {
    let successor = source.rollback_successor(Some(outcome)).unwrap();
    assert_eq!(successor.operation, Operation::NewState);
    assert_eq!(successor.phase, Phase::FreshDbInvalidated);
    assert_eq!(successor.generation, 17);
    assert_eq!(successor.rollback.as_ref().unwrap().source, ForwardPhase::RootLinksComplete);
    assert_eq!(successor.rollback.as_ref().unwrap().fresh_db, recorded_action(outcome));
    successor
}

pub(super) fn write_control_case(
    control: &Path,
    epoch: ProcessEpoch,
    boundary: FreshDbInvalidationProcessBoundary,
    dimensions: MatrixDimensions,
    record: &TransitionRecord,
    source_bytes: &[u8],
) {
    fs::write(
        control.join("case"),
        format!(
            "v1\n{}\nroot-links-complete\n{}\n{:?}\n{:?}\n{}\n{}\n",
            epoch.as_str(),
            boundary.as_str(),
            dimensions.usr_outcome,
            dimensions.candidate_outcome,
            record.transition_id,
            record.generation,
        ),
    )
    .unwrap();
    fs::write(control.join("source-record"), source_bytes).unwrap();
}

pub(super) fn mark_recovery_as_next_database_opener(control: &Path) {
    fs::write(
        control.join(RECOVERY_OPENER_MARKER),
        b"v1\nrecovery-child-is-next-database-opener\n",
    )
    .unwrap();
}

pub(super) fn spawn_child(
    role: ProcessRole,
    epoch: ProcessEpoch,
    boundary: FreshDbInvalidationProcessBoundary,
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
        .env(BOUNDARY_ENV, boundary.as_str())
        .env(ROOT_ENV, root)
        .env(CONTROL_ENV, control)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

pub(super) fn assert_parent_environment_clean() {
    for name in [ROLE_ENV, EPOCH_ENV, BOUNDARY_ENV, ROOT_ENV, CONTROL_ENV] {
        assert!(
            env::var_os(name).is_none(),
            "parent process inherited child-only environment {name}"
        );
    }
}

pub(super) fn kill_after_real_invalidation_attempt() {
    assert_eq!(
        fresh_db_invalidation_removal_call_count(),
        1,
        "SIGKILL boundary must follow exactly one production invalidation call"
    );
    assert_eq!(
        exact_fresh_transition_removal_transaction_attempts(),
        1,
        "SIGKILL boundary must follow exactly one real SQLite transaction attempt"
    );
    // This is genuine same-boot process death. It is not a reboot simulation
    // or a power-loss durability oracle, including for historical epochs.
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

fn recorded_action(outcome: RollbackActionOutcome) -> RollbackAction {
    match outcome {
        RollbackActionOutcome::Applied => RollbackAction::Applied,
        RollbackActionOutcome::AlreadySatisfied => RollbackAction::AlreadySatisfied,
    }
}

fn required_unicode_env(name: &str) -> String {
    env::var_os(name)
        .unwrap_or_else(|| panic!("required RootLinks invalidation environment {name} is missing"))
        .into_string()
        .unwrap_or_else(|_| panic!("RootLinks invalidation environment {name} is not UTF-8"))
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
        "v1 invalidation control must remain outside the installation"
    );
}
