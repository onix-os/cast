//! Genuine same-boot process-death proof for low-level owned-cleanup components.
//!
//! This harness calls the component reconciliation helpers directly. It does
//! not exercise receipt validation or a production startup entry point.

use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Write as _},
    os::unix::process::ExitStatusExt as _,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    client::active_reblit_mounted_boot_topology::ActiveReblitBootOwnedCleanupOutcome,
    linux_fs::{
        descriptor_boot_namespace::RetainedBootNamespaceExpectedSource,
        mount_namespace::{
            PreparedMountNamespaceAnchor,
            RetainedBootFilePublicationLimits,
            RetainedBootFileStaleCleanupRequest,
            arm_after_boot_file_sidecar_unlink_callback,
            arm_after_stale_boot_file_detach_callback,
        },
    },
};

use super::{
    CleanupFixture, INSTALLED_BYTES, PREDECESSOR_BYTES, REPLACEMENT_PATH,
    STALE_BYTES, STALE_PATH, deadline, fingerprint, output, output_request,
    receipt_owner, replacement_request, retain_parent,
};
use super::super::{
    OwnedCleanupTargetIdentity, reconcile_restart_replacement_with_attachment,
    reconcile_restart_stale_with_attachment, split_cleanup_path,
};

const TEST_NAME: &str = concat!(
    "client::active_reblit_mounted_boot_topology::capture::publication_targets::",
    "owned_cleanup::restart::tests::component_process_kill::",
    "owned_cleanup_components_process_kills_recover_exactly",
);
const ROLE_ENV: &str = "CAST_FORGE_OWNED_CLEANUP_KILL_ROLE";
const CASE_ENV: &str = "CAST_FORGE_OWNED_CLEANUP_KILL_CASE";
const ROOT_ENV: &str = "CAST_FORGE_OWNED_CLEANUP_KILL_ROOT";
const RESIDUE_ENV: &str = "CAST_FORGE_OWNED_CLEANUP_KILL_RESIDUE";
const NONCE_ENV: &str = "CAST_FORGE_OWNED_CLEANUP_KILL_NONCE";
const CONTROL_PREFIX: &str = ".forge-owned-cleanup-control-";
const CONTROL_NONCE_LENGTH: usize = 16;
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
            other => panic!("invalid owned-cleanup process role {other:?}"),
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
enum CleanupKillCase {
    ReplacementSidecarUnlinked,
    StaleDetached,
    StaleUnlinked,
}

impl CleanupKillCase {
    const ALL: [Self; 3] = [
        Self::ReplacementSidecarUnlinked,
        Self::StaleDetached,
        Self::StaleUnlinked,
    ];

    fn parse(value: &str) -> Self {
        match value {
            "replacement-sidecar-unlinked" => {
                Self::ReplacementSidecarUnlinked
            }
            "stale-detached" => Self::StaleDetached,
            "stale-unlinked" => Self::StaleUnlinked,
            other => panic!("invalid owned-cleanup kill case {other:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::ReplacementSidecarUnlinked => {
                "replacement-sidecar-unlinked"
            }
            Self::StaleDetached => "stale-detached",
            Self::StaleUnlinked => "stale-unlinked",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::ReplacementSidecarUnlinked => {
                "forge-owned-cleanup-process-replacement-"
            }
            Self::StaleDetached => "forge-owned-cleanup-process-stale-detach-",
            Self::StaleUnlinked => "forge-owned-cleanup-process-stale-unlink-",
        }
    }
}

struct ChildCase {
    role: ProcessRole,
    case: CleanupKillCase,
    root: PathBuf,
    residue_leaf: String,
    nonce: String,
}

impl ChildCase {
    fn from_environment() -> Self {
        let role = ProcessRole::parse(&required_unicode_env(ROLE_ENV));
        let case = CleanupKillCase::parse(&required_unicode_env(CASE_ENV));
        let root = canonical_environment_path(ROOT_ENV);
        assert_safe_fixture_root(&root);
        let residue_leaf = required_unicode_env(RESIDUE_ENV);
        assert_single_component(&residue_leaf);
        let nonce = required_unicode_env(NONCE_ENV);
        assert_control_nonce(&nonce);
        Self {
            role,
            case,
            root,
            residue_leaf,
            nonce,
        }
    }
}

struct ParentControl {
    file: tempfile::NamedTempFile,
    nonce: String,
}

impl ParentControl {
    fn create(
        root: &Path,
        case: CleanupKillCase,
        residue_leaf: &str,
    ) -> Self {
        let mut file = tempfile::Builder::new()
            .prefix(CONTROL_PREFIX)
            .rand_bytes(CONTROL_NONCE_LENGTH)
            .tempfile_in(root)
            .unwrap();
        let leaf = file
            .path()
            .file_name()
            .and_then(|leaf| leaf.to_str())
            .unwrap();
        let nonce = leaf.strip_prefix(CONTROL_PREFIX).unwrap().to_owned();
        assert_control_nonce(&nonce);
        file.as_file_mut()
            .write_all(control_contents(case, &nonce, residue_leaf).as_bytes())
            .unwrap();
        file.as_file().sync_all().unwrap();
        Self { file, nonce }
    }

    fn nonce(&self) -> &str {
        &self.nonce
    }

    fn leaf(&self) -> &str {
        self.file
            .path()
            .file_name()
            .and_then(|leaf| leaf.to_str())
            .unwrap()
    }

    fn assert_exact(&self, case: CleanupKillCase, residue_leaf: &str) {
        assert_regular_file_bytes(
            self.file.path(),
            control_contents(case, self.nonce(), residue_leaf).as_bytes(),
        );
    }
}

struct ParentFixture {
    control: ParentControl,
    _temporary: tempfile::TempDir,
    root: PathBuf,
    residue_leaf: String,
}

impl ParentFixture {
    fn stage(case: CleanupKillCase) -> Self {
        let fixture = CleanupFixture::new(case.prefix());
        let view = fixture
            .attachment
            .revalidate_against(&fixture.anchor)
            .unwrap();
        let residue_leaf = match case {
            CleanupKillCase::ReplacementSidecarUnlinked => {
                stage_replacement(&fixture, &view)
            }
            CleanupKillCase::StaleDetached
            | CleanupKillCase::StaleUnlinked => {
                stage_stale(&view)
            }
        };
        drop(view);
        let CleanupFixture {
            _temporary,
            root,
            anchor,
            attachment,
        } = fixture;
        drop(attachment);
        drop(anchor);
        let root = fs::canonicalize(root).unwrap();
        assert_safe_fixture_root(&root);
        let control = ParentControl::create(&root, case, &residue_leaf);
        let retained = Self {
            control,
            _temporary,
            root,
            residue_leaf,
        };
        retained.assert_staged(case);
        retained
    }

    fn assert_staged(&self, case: CleanupKillCase) {
        self.assert_controlled_root(case);
        match case {
            CleanupKillCase::ReplacementSidecarUnlinked => {
                assert_eq!(
                    fs::read(self.root.join(REPLACEMENT_PATH)).unwrap(),
                    INSTALLED_BYTES,
                );
                assert_eq!(
                    fs::read(parent_path(&self.root).join(&self.residue_leaf))
                        .unwrap(),
                    PREDECESSOR_BYTES,
                );
                assert_parent_names(
                    &self.root,
                    &["restart-replacement.efi", &self.residue_leaf],
                );
            }
            CleanupKillCase::StaleDetached
            | CleanupKillCase::StaleUnlinked => {
                assert_eq!(
                    fs::read(self.root.join(STALE_PATH)).unwrap(),
                    STALE_BYTES,
                );
                assert_parent_names(&self.root, &["restart-stale.efi"]);
            }
        }
    }

    fn assert_after_crash(&self, case: CleanupKillCase) {
        self.assert_controlled_root(case);
        match case {
            CleanupKillCase::ReplacementSidecarUnlinked => {
                assert_eq!(
                    fs::read(self.root.join(REPLACEMENT_PATH)).unwrap(),
                    INSTALLED_BYTES,
                );
                assert!(!parent_path(&self.root).join(&self.residue_leaf).exists());
                assert_parent_names(&self.root, &["restart-replacement.efi"]);
            }
            CleanupKillCase::StaleDetached => {
                assert!(!self.root.join(STALE_PATH).exists());
                assert_eq!(
                    fs::read(parent_path(&self.root).join(&self.residue_leaf))
                        .unwrap(),
                    STALE_BYTES,
                );
                assert_parent_names(&self.root, &[&self.residue_leaf]);
            }
            CleanupKillCase::StaleUnlinked => {
                assert!(!self.root.join(STALE_PATH).exists());
                assert!(!parent_path(&self.root).join(&self.residue_leaf).exists());
                assert_parent_names(&self.root, &[]);
            }
        }
    }

    fn assert_recovered(&self, case: CleanupKillCase) {
        self.assert_controlled_root(case);
        match case {
            CleanupKillCase::ReplacementSidecarUnlinked => {
                assert_eq!(
                    fs::read(self.root.join(REPLACEMENT_PATH)).unwrap(),
                    INSTALLED_BYTES,
                );
                assert!(!parent_path(&self.root).join(&self.residue_leaf).exists());
                assert_parent_names(&self.root, &["restart-replacement.efi"]);
            }
            CleanupKillCase::StaleDetached
            | CleanupKillCase::StaleUnlinked => {
                assert!(!self.root.join(STALE_PATH).exists());
                assert!(!parent_path(&self.root).join(&self.residue_leaf).exists());
                assert_parent_names(&self.root, &[]);
            }
        }
    }

    fn assert_controlled_root(&self, case: CleanupKillCase) {
        assert_boot_root_scaffold(&self.root, self.control.leaf());
        self.control.assert_exact(case, &self.residue_leaf);
    }

    fn cleanup(self) {
        let Self {
            control,
            _temporary,
            root: _,
            residue_leaf: _,
        } = self;
        let control_path = control.file.path().to_owned();
        drop(control);
        assert!(
            !control_path.exists(),
            "parent cleanup retained the owned-cleanup control file",
        );
        drop(_temporary);
    }
}

#[test]
fn owned_cleanup_components_process_kills_recover_exactly() {
    match env::var_os(ROLE_ENV) {
        Some(_) => run_child(ChildCase::from_environment()),
        None => run_parent(),
    }
}

fn run_parent() {
    assert_parent_environment_clean();
    let mut cases = 0;
    for case in CleanupKillCase::ALL {
        run_parent_case(case);
        cases += 1;
    }
    assert_eq!(cases, 3, "owned-cleanup SIGKILL matrix must remain exact");
}

fn run_parent_case(case: CleanupKillCase) {
    let fixture = ParentFixture::stage(case);
    let crash = spawn_child(ProcessRole::Crash, case, &fixture);
    let crash_status =
        DeadlineChild::new(crash, "owned-cleanup component crash child")
            .wait(CHILD_DEADLINE);
    assert_eq!(
        crash_status.signal(),
        Some(nix::libc::SIGKILL),
        "crash child missed {case:?}: {crash_status:?}",
    );
    fixture.assert_after_crash(case);

    let recovery = spawn_child(ProcessRole::Recover, case, &fixture);
    let recovery_status =
        DeadlineChild::new(recovery, "owned-cleanup component recovery child")
            .wait(CHILD_DEADLINE);
    assert!(
        recovery_status.success(),
        "recovery child failed for {case:?}: {recovery_status:?}",
    );
    assert_eq!(recovery_status.signal(), None);
    fixture.assert_recovered(case);
    fixture.cleanup();
}

fn run_child(case: ChildCase) {
    verify_child_fixture(&case);
    let anchor = PreparedMountNamespaceAnchor::prepare().unwrap();
    let attachment = anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment(case.root.to_str().unwrap())
        .unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let identity = OwnedCleanupTargetIdentity::from_attachment(&view);
    match case.case {
        CleanupKillCase::ReplacementSidecarUnlinked => {
            run_replacement_child(&case, &view, identity)
        }
        CleanupKillCase::StaleDetached
        | CleanupKillCase::StaleUnlinked => {
            run_stale_child(&case, &view, identity)
        }
    }
}

fn run_replacement_child(
    case: &ChildCase,
    view: &crate::linux_fs::mount_namespace::RevalidatedTaskRootedAttachment<'_>,
    identity: OwnedCleanupTargetIdentity,
) {
    let predecessor = output(REPLACEMENT_PATH, PREDECESSOR_BYTES);
    let installed = output(REPLACEMENT_PATH, INSTALLED_BYTES);
    let path = split_cleanup_path(REPLACEMENT_PATH, "owned cleanup replacement", 0)
        .unwrap();
    if case.role == ProcessRole::Crash {
        arm_after_boot_file_sidecar_unlink_callback(kill_self);
    }
    let result = reconcile_restart_replacement_with_attachment(
        view,
        identity,
        deadline(),
        0,
        &path,
        &predecessor,
        &installed,
        receipt_owner(fingerprint(0x41)),
    );
    match case.role {
        ProcessRole::Crash => panic!(
            "replacement crash child escaped the post-unlink boundary: {result:?}",
        ),
        ProcessRole::Recover => assert_eq!(
            result.unwrap(),
            ActiveReblitBootOwnedCleanupOutcome::AlreadyClean,
        ),
    }
}

fn run_stale_child(
    case: &ChildCase,
    view: &crate::linux_fs::mount_namespace::RevalidatedTaskRootedAttachment<'_>,
    identity: OwnedCleanupTargetIdentity,
) {
    let stale = output(STALE_PATH, STALE_BYTES);
    let path = split_cleanup_path(STALE_PATH, "owned cleanup stale", 0).unwrap();
    if case.role == ProcessRole::Crash {
        match case.case {
            CleanupKillCase::StaleDetached => {
                arm_after_stale_boot_file_detach_callback(kill_self)
            }
            CleanupKillCase::StaleUnlinked => {
                arm_after_boot_file_sidecar_unlink_callback(kill_self)
            }
            CleanupKillCase::ReplacementSidecarUnlinked => unreachable!(),
        }
    }
    let result = reconcile_restart_stale_with_attachment(
        view,
        identity,
        deadline(),
        0,
        &path,
        &stale,
        receipt_owner(fingerprint(0x51)),
    );
    match case.role {
        ProcessRole::Crash => panic!(
            "stale crash child escaped {:?}: {result:?}",
            case.case,
        ),
        ProcessRole::Recover => assert_eq!(
            result.unwrap(),
            match case.case {
                CleanupKillCase::StaleDetached => {
                    ActiveReblitBootOwnedCleanupOutcome::RemovedOwnedStale
                }
                CleanupKillCase::StaleUnlinked => {
                    ActiveReblitBootOwnedCleanupOutcome::AlreadyClean
                }
                CleanupKillCase::ReplacementSidecarUnlinked => unreachable!(),
            },
        ),
    }
}

fn stage_replacement(
    fixture: &CleanupFixture,
    view: &crate::linux_fs::mount_namespace::RevalidatedTaskRootedAttachment<'_>,
) -> String {
    let predecessor = output(REPLACEMENT_PATH, PREDECESSOR_BYTES);
    let installed = output(REPLACEMENT_PATH, INSTALLED_BYTES);
    let parent = retain_parent(view);
    parent
        .publish_immutable_boot_file_until(
            output_request("restart-replacement.efi", &predecessor),
            &RetainedBootNamespaceExpectedSource::generated(PREDECESSOR_BYTES),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let applied = parent
        .replace_exact_boot_file_until(
            replacement_request(
                "restart-replacement.efi",
                &predecessor,
                &installed,
                receipt_owner(fingerprint(0x41)),
            ),
            &RetainedBootNamespaceExpectedSource::generated(INSTALLED_BYTES),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let residue_leaf = applied.sidecar_leaf().to_owned();
    assert_eq!(
        fs::read(fixture.root.join(REPLACEMENT_PATH)).unwrap(),
        INSTALLED_BYTES,
    );
    drop(applied);
    drop(parent);
    residue_leaf
}

fn stage_stale(
    view: &crate::linux_fs::mount_namespace::RevalidatedTaskRootedAttachment<'_>,
) -> String {
    let stale = output(STALE_PATH, STALE_BYTES);
    let owner = receipt_owner(fingerprint(0x51));
    let parent = retain_parent(view);
    parent
        .publish_immutable_boot_file_until(
            output_request("restart-stale.efi", &stale),
            &RetainedBootNamespaceExpectedSource::generated(STALE_BYTES),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let authority = parent
        .authenticate_stale_boot_file_cleanup_until(
            RetainedBootFileStaleCleanupRequest::new(
                output_request("restart-stale.efi", &stale),
                owner,
            ),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let residue_leaf = authority.private_leaf().to_owned();
    drop(authority);
    drop(parent);
    residue_leaf
}

fn spawn_child(
    role: ProcessRole,
    case: CleanupKillCase,
    fixture: &ParentFixture,
) -> Child {
    Command::new(env::current_exe().unwrap())
        .arg(TEST_NAME)
        .arg("--exact")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(ROLE_ENV, role.as_str())
        .env(CASE_ENV, case.as_str())
        .env(ROOT_ENV, &fixture.root)
        .env(RESIDUE_ENV, &fixture.residue_leaf)
        .env(NONCE_ENV, fixture.control.nonce())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

fn verify_child_fixture(case: &ChildCase) {
    let control_leaf = control_leaf(&case.nonce);
    assert_boot_root_scaffold(&case.root, &control_leaf);
    assert_regular_file_bytes(
        &case.root.join(&control_leaf),
        control_contents(case.case, &case.nonce, &case.residue_leaf).as_bytes(),
    );
    match (case.role, case.case) {
        (ProcessRole::Crash, CleanupKillCase::ReplacementSidecarUnlinked) => {
            assert_regular_file_bytes(
                &case.root.join(REPLACEMENT_PATH),
                INSTALLED_BYTES,
            );
            assert_regular_file_bytes(
                &parent_path(&case.root).join(&case.residue_leaf),
                PREDECESSOR_BYTES,
            );
            assert_parent_names(
                &case.root,
                &["restart-replacement.efi", &case.residue_leaf],
            );
        }
        (ProcessRole::Recover, CleanupKillCase::ReplacementSidecarUnlinked) => {
            assert_regular_file_bytes(
                &case.root.join(REPLACEMENT_PATH),
                INSTALLED_BYTES,
            );
            assert_parent_names(&case.root, &["restart-replacement.efi"]);
        }
        (ProcessRole::Crash, CleanupKillCase::StaleDetached)
        | (ProcessRole::Crash, CleanupKillCase::StaleUnlinked) => {
            assert_regular_file_bytes(&case.root.join(STALE_PATH), STALE_BYTES);
            assert_parent_names(&case.root, &["restart-stale.efi"]);
        }
        (ProcessRole::Recover, CleanupKillCase::StaleDetached) => {
            assert_regular_file_bytes(
                &parent_path(&case.root).join(&case.residue_leaf),
                STALE_BYTES,
            );
            assert_parent_names(&case.root, &[&case.residue_leaf]);
        }
        (ProcessRole::Recover, CleanupKillCase::StaleUnlinked) => {
            assert_parent_names(&case.root, &[]);
        }
    }
}

fn control_contents(
    case: CleanupKillCase,
    nonce: &str,
    residue_leaf: &str,
) -> String {
    format!(
        "owned-cleanup-process-control-v1\ncase={}\nnonce={nonce}\nresidue={residue_leaf}\n",
        case.as_str(),
    )
}

fn control_leaf(nonce: &str) -> String {
    format!("{CONTROL_PREFIX}{nonce}")
}

fn parent_path(root: &Path) -> PathBuf {
    root.join("EFI/Linux")
}

fn assert_boot_root_scaffold(root: &Path, control_leaf: &str) {
    assert_directory(root);
    assert_names(root, &[control_leaf, "EFI"]);
    let efi = root.join("EFI");
    assert_directory(&efi);
    assert_names(&efi, &["Linux"]);
    assert_directory(&parent_path(root));
}

fn assert_parent_names(root: &Path, expected: &[&str]) {
    assert_names(&parent_path(root), expected);
}

fn assert_names(path: &Path, expected: &[&str]) {
    let mut actual = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<OsString>>();
    actual.sort();
    let mut expected = expected
        .iter()
        .map(OsString::from)
        .collect::<Vec<OsString>>();
    expected.sort();
    assert_eq!(actual, expected);
}

fn assert_directory(path: &Path) {
    assert!(fs::symlink_metadata(path).unwrap().file_type().is_dir());
}

fn assert_regular_file_bytes(path: &Path, expected: &[u8]) {
    assert!(fs::symlink_metadata(path).unwrap().file_type().is_file());
    assert_eq!(fs::read(path).unwrap(), expected);
}

fn assert_safe_fixture_root(root: &Path) {
    assert!(root.is_absolute());
    assert_eq!(fs::canonicalize(root).unwrap(), root);
    let target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("target");
    let target = fs::canonicalize(target).unwrap();
    assert!(
        root.starts_with(&target),
        "owned-cleanup process root escaped the repository target directory: {}",
        root.display(),
    );
}

fn assert_control_nonce(nonce: &str) {
    assert_eq!(nonce.len(), CONTROL_NONCE_LENGTH);
    assert!(nonce.bytes().all(|byte| byte.is_ascii_alphanumeric()));
}

fn assert_single_component(value: &str) {
    let path = Path::new(value);
    assert_eq!(path.file_name(), Some(path.as_os_str()));
    assert_eq!(path.components().count(), 1);
}

fn assert_parent_environment_clean() {
    for name in [ROLE_ENV, CASE_ENV, ROOT_ENV, RESIDUE_ENV, NONCE_ENV] {
        assert!(
            env::var_os(name).is_none(),
            "parent inherited child-only environment {name}",
        );
    }
}

fn required_unicode_env(name: &str) -> String {
    env::var_os(name)
        .unwrap_or_else(|| panic!("required owned-cleanup environment {name} is missing"))
        .into_string()
        .unwrap_or_else(|_| panic!("owned-cleanup environment {name} is not UTF-8"))
}

fn canonical_environment_path(name: &str) -> PathBuf {
    let supplied = PathBuf::from(required_unicode_env(name));
    assert!(supplied.is_absolute(), "{name} must be absolute");
    let canonical = fs::canonicalize(&supplied).unwrap();
    assert_eq!(supplied, canonical, "{name} must already be canonical");
    canonical
}

fn kill_self() {
    // Genuine same-boot process death only. This is neither a reboot nor a
    // power-loss durability oracle.
    let result = unsafe {
        nix::libc::kill(nix::libc::getpid(), nix::libc::SIGKILL)
    };
    panic!(
        "SIGKILL self-injection unexpectedly returned {result}: {}",
        io::Error::last_os_error(),
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
                    self.description,
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
