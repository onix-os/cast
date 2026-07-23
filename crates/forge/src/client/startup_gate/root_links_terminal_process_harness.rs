//! Shared re-execution and raw-evidence support for RootLinks terminal deletion.

use std::{
    env,
    ffi::OsString,
    fs, io,
    os::unix::{
        ffi::OsStrExt as _,
        fs::MetadataExt as _,
        process::ExitStatusExt as _,
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    Installation,
    transition_journal::{
        DeleteResidueRecoveryDurabilityBoundary, ForwardPhase, JournalDeleteDurabilityBoundary,
        JournalUpdateDurabilityBoundary, Operation, Phase, PublicBindingRevalidationBoundary, RuntimeEpoch,
        StorageError, TransitionJournalStore, TransitionRecord, arm_delete_residue_recovery_durability_callback,
        arm_journal_delete_durability_callback, arm_journal_update_durability_callback,
        arm_public_binding_revalidation_callback, decode, encode,
    },
};

const ROLE_ENV: &str = "CAST_FORGE_ROOT_LINKS_TERMINAL_ROLE";
const OPERATION_ENV: &str = "CAST_FORGE_ROOT_LINKS_TERMINAL_OPERATION";
const EPOCH_ENV: &str = "CAST_FORGE_ROOT_LINKS_TERMINAL_EPOCH";
const SCENARIO_ENV: &str = "CAST_FORGE_ROOT_LINKS_TERMINAL_SCENARIO";
const ROOT_ENV: &str = "CAST_FORGE_ROOT_LINKS_TERMINAL_ROOT";
const CONTROL_ENV: &str = "CAST_FORGE_ROOT_LINKS_TERMINAL_CONTROL";
const CHILD_DEADLINE: Duration = Duration::from_secs(15);
const CAST_NAME: &str = ".cast";
const JOURNAL_NAME: &str = "journal";
const CANONICAL_NAME: &str = "state-transition";
const LOCK_NAME: &str = "state-transition.lock";
const DELETE_PREFIX: &[u8] = b".state-transition.delete-";
const ROOT_ABI: [(&str, &str); 5] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib32", "usr/lib32"),
    ("lib64", "usr/lib"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminalOperation {
    NewState,
    ActivateArchived,
    ActiveReblit,
}

impl TerminalOperation {
    pub(super) fn operation(self) -> Operation {
        match self {
            Self::NewState => Operation::NewState,
            Self::ActivateArchived => Operation::ActivateArchived,
            Self::ActiveReblit => Operation::ActiveReblit,
        }
    }

    pub(super) fn generation(self) -> u64 {
        match self {
            Self::NewState => 18,
            Self::ActivateArchived => 12,
            Self::ActiveReblit => 14,
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "new-state" => Self::NewState,
            "activate-archived" => Self::ActivateArchived,
            "active-reblit" => Self::ActiveReblit,
            other => panic!("invalid RootLinks terminal operation {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::NewState => "new-state",
            Self::ActivateArchived => "activate-archived",
            Self::ActiveReblit => "active-reblit",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProcessEpoch {
    Current,
    Historical,
}

impl ProcessEpoch {
    pub(super) const ALL: [Self; 2] = [Self::Current, Self::Historical];

    fn parse(value: &str) -> Self {
        match value {
            "current" => Self::Current,
            "historical" => Self::Historical,
            other => panic!("invalid RootLinks terminal epoch {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Historical => "historical",
        }
    }

    fn validate(self, record: &TransitionRecord) {
        let current = RuntimeEpoch::capture().unwrap();
        match self {
            Self::Current => assert_eq!(record.creation_epoch, current),
            Self::Historical => assert_ne!(record.creation_epoch, current),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RootLinksDeleteScenario {
    FinalPre,
    BeforeBoundDeletePrivateUnlink,
    PrivateUnlinked,
    DeleteDirectorySynced,
    RecoveryAfterCanonicalRestored,
    RecoveryAfterJournalDirectorySynced,
}

impl RootLinksDeleteScenario {
    pub(super) const ALL: [Self; 6] = [
        Self::FinalPre,
        Self::BeforeBoundDeletePrivateUnlink,
        Self::PrivateUnlinked,
        Self::DeleteDirectorySynced,
        Self::RecoveryAfterCanonicalRestored,
        Self::RecoveryAfterJournalDirectorySynced,
    ];

    fn parse(value: &str) -> Self {
        match value {
            "final-pre" => Self::FinalPre,
            "before-bound-delete-private-unlink" => Self::BeforeBoundDeletePrivateUnlink,
            "private-unlinked" => Self::PrivateUnlinked,
            "delete-directory-synced" => Self::DeleteDirectorySynced,
            "recovery-after-canonical-restored" => Self::RecoveryAfterCanonicalRestored,
            "recovery-after-journal-directory-synced" => Self::RecoveryAfterJournalDirectorySynced,
            other => panic!("invalid RootLinks terminal scenario {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::FinalPre => "final-pre",
            Self::BeforeBoundDeletePrivateUnlink => "before-bound-delete-private-unlink",
            Self::PrivateUnlinked => "private-unlinked",
            Self::DeleteDirectorySynced => "delete-directory-synced",
            Self::RecoveryAfterCanonicalRestored => "recovery-after-canonical-restored",
            Self::RecoveryAfterJournalDirectorySynced => "recovery-after-journal-directory-synced",
        }
    }

    pub(super) fn arm_initial_bound_delete_kill(self, death: fn()) -> bool {
        match self {
            Self::FinalPre => false,
            Self::BeforeBoundDeletePrivateUnlink
            | Self::RecoveryAfterCanonicalRestored
            | Self::RecoveryAfterJournalDirectorySynced => {
                arm_public_binding_revalidation_callback(
                    PublicBindingRevalidationBoundary::BeforeBoundDeletePrivateUnlink,
                    death,
                );
                true
            }
            Self::PrivateUnlinked => {
                arm_journal_delete_durability_callback(
                    JournalDeleteDurabilityBoundary::CanonicalUnlinked,
                    death,
                );
                true
            }
            Self::DeleteDirectorySynced => {
                arm_journal_delete_durability_callback(
                    JournalDeleteDurabilityBoundary::DeleteDirectorySynced,
                    death,
                );
                true
            }
        }
    }

    pub(super) fn arm_recovery_kill(self, death: fn()) {
        let boundary = match self {
            Self::RecoveryAfterCanonicalRestored => DeleteResidueRecoveryDurabilityBoundary::CanonicalRestored,
            Self::RecoveryAfterJournalDirectorySynced => {
                DeleteResidueRecoveryDurabilityBoundary::JournalDirectorySynced
            }
            other => panic!("scenario {other:?} has no recovery-death boundary"),
        };
        arm_delete_residue_recovery_durability_callback(boundary, death);
    }

    fn needs_recovery_crash(self) -> bool {
        matches!(
            self,
            Self::RecoveryAfterCanonicalRestored | Self::RecoveryAfterJournalDirectorySynced
        )
    }

    fn after_initial_crash(self) -> RawRecordState {
        match self {
            Self::FinalPre => RawRecordState::Canonical,
            Self::BeforeBoundDeletePrivateUnlink
            | Self::RecoveryAfterCanonicalRestored
            | Self::RecoveryAfterJournalDirectorySynced => RawRecordState::DetachedResidue,
            Self::PrivateUnlinked | Self::DeleteDirectorySynced => RawRecordState::Absent,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProcessRole {
    InitialCrash,
    RecoveryCrash,
    FinalRecover,
}

impl ProcessRole {
    fn parse(value: &str) -> Self {
        match value {
            "initial-crash" => Self::InitialCrash,
            "recovery-crash" => Self::RecoveryCrash,
            "final-recover" => Self::FinalRecover,
            other => panic!("invalid RootLinks terminal process role {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::InitialCrash => "initial-crash",
            Self::RecoveryCrash => "recovery-crash",
            Self::FinalRecover => "final-recover",
        }
    }
}

#[derive(Debug)]
pub(super) struct ChildInvocation {
    pub(super) role: ProcessRole,
    pub(super) epoch: ProcessEpoch,
    pub(super) scenario: RootLinksDeleteScenario,
    pub(super) root: PathBuf,
    control: PathBuf,
    operation: TerminalOperation,
}

impl ChildInvocation {
    pub(super) fn from_environment(expected_operation: TerminalOperation) -> Option<Self> {
        let Some(_) = env::var_os(ROLE_ENV) else {
            assert_parent_environment_clean();
            return None;
        };
        let operation = TerminalOperation::parse(&required_unicode_env(OPERATION_ENV));
        assert_eq!(operation, expected_operation);
        let root = canonical_environment_path(ROOT_ENV);
        let control = canonical_environment_path(CONTROL_ENV);
        assert_host_temporary_path(&root);
        assert_host_temporary_path(&control);
        assert_separate_control_path(&root, &control);
        Some(Self {
            role: ProcessRole::parse(&required_unicode_env(ROLE_ENV)),
            epoch: ProcessEpoch::parse(&required_unicode_env(EPOCH_ENV)),
            scenario: RootLinksDeleteScenario::parse(&required_unicode_env(SCENARIO_ENV)),
            root,
            control,
            operation,
        })
    }

    pub(super) fn terminal_record(&self, expected_dimensions: &str) -> TransitionRecord {
        let frame = fs::read(self.control.join("terminal-record")).unwrap();
        let record = decode(&frame).unwrap();
        assert_eq!(encode(&record).unwrap(), frame);
        assert_eq!(record.operation, self.operation.operation());
        assert_eq!(record.phase, Phase::RollbackComplete);
        assert_eq!(record.generation, self.operation.generation());
        assert_eq!(record.rollback.as_ref().unwrap().source, ForwardPhase::RootLinksComplete);
        self.epoch.validate(&record);
        let expected_case = format!(
            "v1\n{}\n{}\n{}\n{}\n{}\n{}\n",
            self.operation.as_str(),
            self.epoch.as_str(),
            self.scenario.as_str(),
            expected_dimensions,
            record.transition_id,
            record.generation,
        );
        assert_eq!(fs::read_to_string(self.control.join("case")).unwrap(), expected_case);
        record
    }

    pub(super) fn journal_expectation(&self) -> JournalExpectation {
        JournalExpectation::from_control(&self.root, &self.control)
    }

    pub(super) fn expected_entry_state(&self) -> RawRecordState {
        match self.role {
            ProcessRole::InitialCrash => RawRecordState::Canonical,
            ProcessRole::RecoveryCrash => RawRecordState::DetachedResidue,
            ProcessRole::FinalRecover => {
                if self.scenario.needs_recovery_crash() || self.scenario == RootLinksDeleteScenario::FinalPre {
                    RawRecordState::Canonical
                } else {
                    self.scenario.after_initial_crash()
                }
            }
        }
    }
}

pub(super) struct ControlDirectory {
    directory: tempfile::TempDir,
}

impl ControlDirectory {
    pub(super) fn new(
        root: &Path,
        operation: TerminalOperation,
        epoch: ProcessEpoch,
        scenario: RootLinksDeleteScenario,
        record: &TransitionRecord,
        dimensions: &str,
        journal: &JournalExpectation,
    ) -> Self {
        assert_eq!(record.operation, operation.operation());
        assert_eq!(record.generation, operation.generation());
        let directory = tempfile::tempdir().unwrap();
        let control = fs::canonicalize(directory.path()).unwrap();
        assert_host_temporary_path(root);
        assert_host_temporary_path(&control);
        assert_separate_control_path(root, &control);
        fs::write(
            control.join("case"),
            format!(
                "v1\n{}\n{}\n{}\n{}\n{}\n{}\n",
                operation.as_str(),
                epoch.as_str(),
                scenario.as_str(),
                dimensions,
                record.transition_id,
                record.generation,
            ),
        )
        .unwrap();
        fs::write(control.join("terminal-record"), &journal.frame).unwrap();
        journal.write_control(&control);
        Self { directory }
    }

    pub(super) fn path(&self) -> PathBuf {
        fs::canonicalize(self.directory.path()).unwrap()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct JournalExpectation {
    cast: Identity,
    journal: Identity,
    lock: Identity,
    record: Identity,
    frame: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RawRecordState {
    Canonical,
    DetachedResidue,
    Absent,
}

impl JournalExpectation {
    pub(super) fn capture(root: &Path, record: &TransitionRecord) -> Self {
        let canonical = canonical_path(root);
        let frame = fs::read(&canonical).unwrap();
        assert_eq!(frame, encode(record).unwrap());
        let expectation = Self {
            cast: directory_identity(&root.join(CAST_NAME)),
            journal: directory_identity(&journal_path(root)),
            lock: file_identity(&journal_path(root).join(LOCK_NAME)),
            record: file_identity(&canonical),
            frame,
        };
        expectation.assert_raw(root, RawRecordState::Canonical);
        expectation
    }

    fn from_control(root: &Path, control: &Path) -> Self {
        let values = fs::read_to_string(control.join("journal-evidence")).unwrap();
        let mut lines = values.lines();
        assert_eq!(lines.next(), Some("v1"));
        let expectation = Self {
            cast: parse_identity(lines.next().unwrap()),
            journal: parse_identity(lines.next().unwrap()),
            lock: parse_identity(lines.next().unwrap()),
            record: parse_identity(lines.next().unwrap()),
            frame: fs::read(control.join("terminal-record")).unwrap(),
        };
        assert_eq!(lines.next(), None);
        expectation.assert_public_anchors(root);
        expectation
    }

    fn write_control(&self, control: &Path) {
        fs::write(
            control.join("journal-evidence"),
            format!(
                "v1\n{} {}\n{} {}\n{} {}\n{} {}\n",
                self.cast.device,
                self.cast.inode,
                self.journal.device,
                self.journal.inode,
                self.lock.device,
                self.lock.inode,
                self.record.device,
                self.record.inode,
            ),
        )
        .unwrap();
    }

    pub(super) fn assert_raw(&self, root: &Path, state: RawRecordState) {
        self.assert_public_anchors(root);
        let mut names = fs::read_dir(journal_path(root))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        assert!(remove_name(&mut names, LOCK_NAME), "journal lock is missing");
        match state {
            RawRecordState::Canonical => {
                assert!(remove_name(&mut names, CANONICAL_NAME));
                assert_exact_record_file(&canonical_path(root), self.record, &self.frame);
            }
            RawRecordState::DetachedResidue => {
                assert!(!canonical_path(root).exists());
                let residues = names
                    .iter()
                    .filter(|name| valid_delete_residue_name(name.as_os_str().as_bytes()))
                    .cloned()
                    .collect::<Vec<_>>();
                assert_eq!(residues.len(), 1, "expected one exact bound-delete residue");
                assert_exact_record_file(&journal_path(root).join(&residues[0]), self.record, &self.frame);
                assert!(remove_os_name(&mut names, &residues[0]));
            }
            RawRecordState::Absent => {
                assert!(!canonical_path(root).exists());
            }
        }
        assert!(names.is_empty(), "unexpected raw journal inventory: {names:?}");
    }

    fn assert_public_anchors(&self, root: &Path) {
        assert_eq!(directory_identity(&root.join(CAST_NAME)), self.cast);
        assert_eq!(directory_identity(&journal_path(root)), self.journal);
        assert_eq!(file_identity(&journal_path(root).join(LOCK_NAME)), self.lock);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RootLinkEvidence {
    name: &'static str,
    target: PathBuf,
    identity: Identity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RootLinksSnapshot(Vec<RootLinkEvidence>);

impl RootLinksSnapshot {
    pub(super) fn capture(root: &Path) -> Self {
        Self(
            ROOT_ABI
                .into_iter()
                .map(|(name, expected)| {
                    let path = root.join(name);
                    let metadata = fs::symlink_metadata(&path).unwrap();
                    assert!(metadata.file_type().is_symlink(), "{} is not a symlink", path.display());
                    let target = fs::read_link(&path).unwrap();
                    assert_eq!(target, Path::new(expected));
                    RootLinkEvidence {
                        name,
                        target,
                        identity: Identity {
                            device: metadata.dev(),
                            inode: metadata.ino(),
                        },
                    }
                })
                .collect(),
        )
    }

    pub(super) fn assert_unchanged(&self, root: &Path) {
        assert_eq!(&Self::capture(root), self);
    }
}

pub(super) struct ParentCase<'a> {
    pub(super) test_name: &'a str,
    pub(super) operation: TerminalOperation,
    pub(super) epoch: ProcessEpoch,
    pub(super) scenario: RootLinksDeleteScenario,
    pub(super) root: &'a Path,
    pub(super) control: &'a Path,
}

impl ParentCase<'_> {
    pub(super) fn run(
        &self,
        journal: &JournalExpectation,
        root_links: &RootLinksSnapshot,
        mut assert_operation_evidence: impl FnMut(),
    ) {
        self.expect_sigkill(ProcessRole::InitialCrash);
        journal.assert_raw(self.root, self.scenario.after_initial_crash());
        root_links.assert_unchanged(self.root);
        assert_operation_evidence();

        if self.scenario.needs_recovery_crash() {
            self.expect_sigkill(ProcessRole::RecoveryCrash);
            journal.assert_raw(self.root, RawRecordState::Canonical);
            root_links.assert_unchanged(self.root);
            assert_operation_evidence();
        }

        let status = DeadlineChild::new(self.spawn(ProcessRole::FinalRecover), "RootLinks final recovery child")
            .wait(CHILD_DEADLINE);
        assert!(status.success(), "RootLinks final recovery failed: {status:?}");
        assert_eq!(status.signal(), None);
        journal.assert_raw(self.root, RawRecordState::Absent);
        root_links.assert_unchanged(self.root);
        assert_operation_evidence();
    }

    fn expect_sigkill(&self, role: ProcessRole) {
        let status = DeadlineChild::new(self.spawn(role), "RootLinks terminal crash child").wait(CHILD_DEADLINE);
        assert_eq!(
            status.signal(),
            Some(nix::libc::SIGKILL),
            "{role:?} missed {:?} for {:?}: {status:?}",
            self.scenario,
            self.operation,
        );
    }

    fn spawn(&self, role: ProcessRole) -> Child {
        Command::new(env::current_exe().unwrap())
            .arg(self.test_name)
            .arg("--exact")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(ROLE_ENV, role.as_str())
            .env(OPERATION_ENV, self.operation.as_str())
            .env(EPOCH_ENV, self.epoch.as_str())
            .env(SCENARIO_ENV, self.scenario.as_str())
            .env(ROOT_ENV, self.root)
            .env(CONTROL_ENV, self.control)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap()
    }
}

pub(super) fn forbid_journal_update(context: &'static str) {
    arm_journal_update_durability_callback(JournalUpdateDurabilityBoundary::TemporaryFullySynced, move || {
        panic!("{context} attempted a journal update")
    });
}

pub(super) fn assert_clean_holds_journal_lock(installation: &Installation, root: &Path) {
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let competing = TransitionJournalStore::try_open_in_retained_cast(cast, root).unwrap_err();
    assert!(matches!(competing, StorageError::AcquireLock { .. }), "{competing:?}");
}

pub(super) fn assert_clean_store_reopens(installation: &Installation, root: &Path) {
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let reopened = TransitionJournalStore::try_open_in_retained_cast(cast, root).unwrap();
    assert_eq!(reopened.load().unwrap(), None);
}

fn assert_exact_record_file(path: &Path, expected: Identity, frame: &[u8]) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_file(), "{} is not a regular record", path.display());
    assert_eq!(metadata.nlink(), 1);
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    assert_eq!(metadata.uid(), nix::unistd::Uid::current().as_raw());
    assert_eq!(
        Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        expected,
    );
    assert_eq!(metadata.len(), frame.len() as u64);
    assert_eq!(fs::read(path).unwrap(), frame);
}

fn parse_identity(line: &str) -> Identity {
    let mut values = line.split_ascii_whitespace();
    let identity = Identity {
        device: values.next().unwrap().parse().unwrap(),
        inode: values.next().unwrap().parse().unwrap(),
    };
    assert_eq!(values.next(), None);
    identity
}

fn journal_path(root: &Path) -> PathBuf {
    root.join(CAST_NAME).join(JOURNAL_NAME)
}

fn canonical_path(root: &Path) -> PathBuf {
    journal_path(root).join(CANONICAL_NAME)
}

fn directory_identity(path: &Path) -> Identity {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_dir(), "{} is not a directory", path.display());
    Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn file_identity(path: &Path) -> Identity {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_file(), "{} is not a regular file", path.display());
    Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn remove_name(names: &mut Vec<OsString>, expected: &str) -> bool {
    let Some(index) = names.iter().position(|name| name == expected) else {
        return false;
    };
    names.remove(index);
    true
}

fn remove_os_name(names: &mut Vec<OsString>, expected: &OsString) -> bool {
    let Some(index) = names.iter().position(|name| name == expected) else {
        return false;
    };
    names.remove(index);
    true
}

fn valid_delete_residue_name(name: &[u8]) -> bool {
    let Some(tail) = name.strip_prefix(DELETE_PREFIX) else {
        return false;
    };
    tail.len() == 8 + 1 + 16
        && tail[8] == b'-'
        && tail[..8]
            .iter()
            .chain(&tail[9..])
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn required_unicode_env(name: &str) -> String {
    env::var_os(name)
        .unwrap_or_else(|| panic!("required RootLinks terminal environment {name} is missing"))
        .into_string()
        .unwrap_or_else(|_| panic!("RootLinks terminal environment {name} is not UTF-8"))
}

fn canonical_environment_path(name: &str) -> PathBuf {
    let supplied = PathBuf::from(required_unicode_env(name));
    assert!(supplied.is_absolute(), "{name} must be absolute");
    let canonical = fs::canonicalize(&supplied).unwrap();
    assert_eq!(supplied, canonical, "{name} must already be canonical");
    canonical
}

fn assert_host_temporary_path(path: &Path) {
    let temporary = fs::canonicalize(env::temp_dir()).unwrap();
    assert!(path.starts_with(&temporary), "{} is outside the host temporary root", path.display());
}

fn assert_separate_control_path(root: &Path, control: &Path) {
    assert_ne!(root, control);
    assert!(!root.starts_with(control) && !control.starts_with(root));
}

fn assert_parent_environment_clean() {
    for name in [ROLE_ENV, OPERATION_ENV, EPOCH_ENV, SCENARIO_ENV, ROOT_ENV, CONTROL_ENV] {
        assert!(env::var_os(name).is_none(), "parent inherited child-only environment {name}");
    }
}

pub(super) fn kill_self() {
    // Genuine same-boot process death only: neither a historical record epoch
    // nor SIGKILL proves reboot or power-loss persistence.
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

    fn wait(mut self, deadline_after: Duration) -> ExitStatus {
        let deadline = Instant::now() + deadline_after;
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
                    "{} exceeded {deadline_after:?}; killed and reaped with {status:?}",
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
