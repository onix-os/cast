use std::{
    fs,
    io::Write as _,
    os::{
        fd::{AsFd as _, AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use sha2::{Digest as _, Sha256};
use xxhash_rust::xxh3::xxh3_128;

use crate::linux_fs::{
    descriptor_boot_namespace::RetainedBootNamespaceExpectedSource,
    mount_namespace::{
        PreparedMountNamespaceAnchor, PreparedTaskRootedAttachment,
        RetainedBootFileAppliedSidecarCleanupState, RetainedBootFileMutationFingerprint,
        RetainedBootFilePublicationLimits, RetainedBootFileRestoredSidecarCleanupState,
        RetainedBootFilePublicationRequest, RetainedBootFileReplacementError,
        RetainedBootFileReplacementRequest, RetainedBootFileSidecarCleanupOutcome,
        RetainedBootFileStaleCleanupRequest,
        RetainedBootFileStaleCleanupState, arm_boot_file_exchange_error_after_applied,
        arm_boot_file_replacement_stop_before_exchange, arm_boot_file_sidecar_stop_after_unlink,
        arm_stale_boot_file_detach_error_after_applied,
        arm_stale_boot_file_stop_after_detach,
    },
};

const LEAF: &str = "vmlinuz-replacement";
const INSTALLED: &[u8] = b"installed boot payload\n";
const REPLACEMENT: &[u8] = b"replacement boot payload\n";

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    anchor: PreparedMountNamespaceAnchor,
    attachment: PreparedTaskRootedAttachment,
}

impl Fixture {
    fn new(prefix: &str) -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join("target");
        fs::create_dir_all(&target).unwrap();
        let temporary = tempfile::Builder::new().prefix(prefix).tempdir_in(target).unwrap();
        let root = temporary.path().join("boot-root");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let anchor = PreparedMountNamespaceAnchor::prepare().unwrap();
        let attachment = anchor
            .revalidate()
            .unwrap()
            .prepare_task_rooted_attachment(root.to_str().unwrap())
            .unwrap();
        Self {
            _temporary: temporary,
            root,
            anchor,
            attachment,
        }
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn exact(bytes: &'static [u8]) -> RetainedBootFilePublicationRequest<'static> {
    RetainedBootFilePublicationRequest::new(
        LEAF,
        bytes.len() as u64,
        xxh3_128(bytes),
        Sha256::digest(bytes).into(),
    )
}

fn replacement_request() -> RetainedBootFileReplacementRequest<'static> {
    RetainedBootFileReplacementRequest::new(
        exact(INSTALLED),
        exact(REPLACEMENT),
        RetainedBootFileMutationFingerprint::new([0x51; 32]),
    )
}

fn sealed_source(bytes: &[u8]) -> fs::File {
    // SAFETY: the static name remains valid for the one memfd_create call.
    let raw = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_memfd_create,
            c"forge-boot-replacement-source".as_ptr(),
            nix::libc::MFD_CLOEXEC | nix::libc::MFD_ALLOW_SEALING,
        )
    };
    assert!(raw >= 0);
    // SAFETY: successful memfd_create returned a fresh owned descriptor.
    let owned = unsafe { OwnedFd::from_raw_fd(i32::try_from(raw).unwrap()) };
    let mut file = fs::File::from(owned);
    file.write_all(bytes).unwrap();
    assert_eq!(unsafe { nix::libc::fchmod(file.as_raw_fd(), 0o400) }, 0);
    let seals = nix::libc::F_SEAL_WRITE
        | nix::libc::F_SEAL_GROW
        | nix::libc::F_SEAL_SHRINK
        | nix::libc::F_SEAL_SEAL;
    assert_eq!(unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_ADD_SEALS, seals) }, 0);
    file
}

fn publish_installed(
    parent: &crate::linux_fs::mount_namespace::RetainedBootPublicationParent<'_, '_>,
) {
    parent
        .publish_immutable_boot_file_until(
            exact(INSTALLED),
            &RetainedBootNamespaceExpectedSource::generated(INSTALLED),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
}

#[test]
fn exact_exchange_retains_predecessor_and_fresh_authority_cleans_it() {
    let fixture = Fixture::new("forge-boot-replace-apply-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let parent = view.retain_boot_publication_parent_until(&["EFI"], deadline()).unwrap();
    publish_installed(&parent);
    let canonical = fixture.root.join("EFI").join(LEAF);
    let installed_inode = fs::metadata(&canonical).unwrap().ino();
    let source = sealed_source(REPLACEMENT);

    let applied = parent
        .replace_exact_boot_file_until(
            replacement_request(),
            &RetainedBootNamespaceExpectedSource::sealed_descriptor(source.as_fd()),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let sidecar = fixture.root.join("EFI").join(applied.sidecar_leaf());
    assert_eq!(fs::read(&canonical).unwrap(), REPLACEMENT);
    assert_eq!(fs::read(&sidecar).unwrap(), INSTALLED);
    assert_eq!(fs::metadata(&sidecar).unwrap().ino(), installed_inode);
    assert_eq!(fs::metadata(&canonical).unwrap().ino(), applied.replacement_file_inode());
    drop(applied);

    let recovered = parent
        .authenticate_applied_boot_file_replacement_until(
            replacement_request(),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    arm_boot_file_sidecar_stop_after_unlink();
    assert!(matches!(
        parent.cleanup_replaced_boot_file_sidecar_until(recovered, deadline()),
        Err(RetainedBootFileReplacementError::InjectedFault { .. })
    ));
    assert!(matches!(
        parent
            .reconcile_replaced_boot_file_sidecar_cleanup_until(
                replacement_request(),
                RetainedBootFilePublicationLimits::default(),
                deadline(),
            )
            .unwrap(),
        RetainedBootFileAppliedSidecarCleanupState::AlreadyClean
    ));
    assert_eq!(fs::read(&canonical).unwrap(), REPLACEMENT);
    assert!(!sidecar.exists());
}

#[test]
fn borrowing_applied_pair_validation_accepts_the_exact_bound_pair() {
    let fixture = Fixture::new("forge-boot-replace-validate-exact-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let parent = view.retain_boot_publication_parent_until(&["EFI"], deadline()).unwrap();
    publish_installed(&parent);

    let applied = parent
        .replace_exact_boot_file_until(
            replacement_request(),
            &RetainedBootNamespaceExpectedSource::generated(REPLACEMENT),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();

    parent
        .validate_applied_boot_file_replacement_until(&applied, deadline())
        .unwrap();
    assert_eq!(fs::read(fixture.root.join("EFI").join(LEAF)).unwrap(), REPLACEMENT);
    assert_eq!(
        fs::read(fixture.root.join("EFI").join(applied.sidecar_leaf())).unwrap(),
        INSTALLED,
    );
}

#[test]
fn borrowing_applied_pair_validation_rejects_a_missing_sidecar() {
    let fixture = Fixture::new("forge-boot-replace-validate-missing-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let parent = view.retain_boot_publication_parent_until(&["EFI"], deadline()).unwrap();
    publish_installed(&parent);

    let applied = parent
        .replace_exact_boot_file_until(
            replacement_request(),
            &RetainedBootNamespaceExpectedSource::generated(REPLACEMENT),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    fs::remove_file(fixture.root.join("EFI").join(applied.sidecar_leaf())).unwrap();

    assert!(parent
        .validate_applied_boot_file_replacement_until(&applied, deadline())
        .is_err());
}

#[test]
fn borrowing_applied_pair_validation_rejects_same_bytes_on_another_inode() {
    let fixture = Fixture::new("forge-boot-replace-validate-inode-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let parent = view.retain_boot_publication_parent_until(&["EFI"], deadline()).unwrap();
    publish_installed(&parent);

    let applied = parent
        .replace_exact_boot_file_until(
            replacement_request(),
            &RetainedBootNamespaceExpectedSource::generated(REPLACEMENT),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let sidecar = fixture.root.join("EFI").join(applied.sidecar_leaf());
    let displaced = fixture.root.join("EFI/displaced-installed-sidecar");
    fs::rename(&sidecar, &displaced).unwrap();
    fs::write(&sidecar, INSTALLED).unwrap();
    fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o644)).unwrap();
    assert_ne!(fs::metadata(&sidecar).unwrap().ino(), applied.installed_file_inode());

    assert!(matches!(
        parent.validate_applied_boot_file_replacement_until(&applied, deadline()),
        Err(RetainedBootFileReplacementError::ExchangeAmbiguous)
    ));
    assert_eq!(fs::read(&sidecar).unwrap(), INSTALLED);
    assert_eq!(fs::read(&displaced).unwrap(), INSTALLED);
}

#[test]
fn applied_error_report_is_reconciled_then_one_reverse_exchange_restores_predecessor() {
    let fixture = Fixture::new("forge-boot-replace-restore-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let parent = view.retain_boot_publication_parent_until(&["EFI"], deadline()).unwrap();
    publish_installed(&parent);
    let canonical = fixture.root.join("EFI").join(LEAF);
    arm_boot_file_exchange_error_after_applied();

    let applied = parent
        .replace_exact_boot_file_until(
            replacement_request(),
            &RetainedBootNamespaceExpectedSource::generated(REPLACEMENT),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let sidecar = fixture.root.join("EFI").join(applied.sidecar_leaf());
    let restored = parent
        .restore_exact_boot_file_replacement_until(applied, deadline())
        .unwrap();
    assert_eq!(fs::read(&canonical).unwrap(), INSTALLED);
    assert_eq!(fs::read(&sidecar).unwrap(), REPLACEMENT);
    assert_eq!(
        parent.cleanup_restored_boot_file_sidecar_until(restored, deadline()).unwrap(),
        RetainedBootFileSidecarCleanupOutcome::RemovedDisplacedReplacement,
    );
    assert_eq!(fs::read(&canonical).unwrap(), INSTALLED);
    assert!(!sidecar.exists());
}

#[test]
fn wrong_predecessor_and_foreign_sidecar_replacement_fail_closed() {
    let fixture = Fixture::new("forge-boot-replace-foreign-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let parent = view.retain_boot_publication_parent_until(&["EFI"], deadline()).unwrap();
    publish_installed(&parent);
    let wrong = RetainedBootFileReplacementRequest::new(
        exact(b"not installed\n"),
        exact(REPLACEMENT),
        RetainedBootFileMutationFingerprint::new([0x51; 32]),
    );
    assert!(parent
        .replace_exact_boot_file_until(
            wrong,
            &RetainedBootNamespaceExpectedSource::generated(REPLACEMENT),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .is_err());

    let applied = parent
        .replace_exact_boot_file_until(
            replacement_request(),
            &RetainedBootNamespaceExpectedSource::generated(REPLACEMENT),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let sidecar = fixture.root.join("EFI").join(applied.sidecar_leaf());
    let preserved = fixture.root.join("EFI/preserved-installed");
    fs::rename(&sidecar, &preserved).unwrap();
    fs::write(&sidecar, b"foreign sidecar\n").unwrap();
    fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(matches!(
        parent.cleanup_replaced_boot_file_sidecar_until(applied, deadline()),
        Err(RetainedBootFileReplacementError::Publication { .. })
    ));
    assert_eq!(fs::read(&sidecar).unwrap(), b"foreign sidecar\n");
    assert_eq!(fs::read(&preserved).unwrap(), INSTALLED);
    assert_eq!(fs::read(fixture.root.join("EFI").join(LEAF)).unwrap(), REPLACEMENT);
}

#[test]
fn synced_pre_exchange_stage_is_freshly_authenticated_and_cleaned_after_restart() {
    let fixture = Fixture::new("forge-boot-replace-staged-prefix-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let parent = view.retain_boot_publication_parent_until(&["EFI"], deadline()).unwrap();
    publish_installed(&parent);
    arm_boot_file_replacement_stop_before_exchange();
    assert!(matches!(
        parent.replace_exact_boot_file_until(
            replacement_request(),
            &RetainedBootNamespaceExpectedSource::generated(REPLACEMENT),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        ),
        Err(RetainedBootFileReplacementError::InjectedFault { .. })
    ));
    assert_eq!(fs::read(fixture.root.join("EFI").join(LEAF)).unwrap(), INSTALLED);

    let restored = parent
        .authenticate_restored_boot_file_replacement_until(
            replacement_request(),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let sidecar = fixture.root.join("EFI").join(restored.sidecar_leaf());
    assert_eq!(fs::read(&sidecar).unwrap(), REPLACEMENT);
    arm_boot_file_sidecar_stop_after_unlink();
    assert!(matches!(
        parent.cleanup_restored_boot_file_sidecar_until(restored, deadline()),
        Err(RetainedBootFileReplacementError::InjectedFault { .. })
    ));
    assert!(matches!(
        parent
            .reconcile_restored_boot_file_sidecar_cleanup_until(
                replacement_request(),
                RetainedBootFilePublicationLimits::default(),
                deadline(),
            )
            .unwrap(),
        RetainedBootFileRestoredSidecarCleanupState::AlreadyClean
    ));
    assert_eq!(fs::read(fixture.root.join("EFI").join(LEAF)).unwrap(), INSTALLED);
    assert!(!sidecar.exists());
}

#[test]
fn exact_stale_output_detaches_once_and_foreign_replacement_is_preserved() {
    let fixture = Fixture::new("forge-boot-replace-stale-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let parent = view.retain_boot_publication_parent_until(&["EFI"], deadline()).unwrap();
    publish_installed(&parent);
    let request = RetainedBootFileStaleCleanupRequest::new(
        exact(INSTALLED),
        RetainedBootFileMutationFingerprint::new([0x61; 32]),
    );
    let authority = parent
        .authenticate_stale_boot_file_cleanup_until(
            request,
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let private = fixture.root.join("EFI").join(authority.private_leaf());
    arm_stale_boot_file_detach_error_after_applied();
    arm_stale_boot_file_stop_after_detach();
    assert!(matches!(
        parent.cleanup_authenticated_stale_boot_file_until(authority, deadline()),
        Err(RetainedBootFileReplacementError::InjectedFault { .. })
    ));
    let detached = match parent
        .reconcile_stale_boot_file_cleanup_until(
            request,
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap()
    {
        RetainedBootFileStaleCleanupState::Detached(authority) => authority,
        state => panic!("unexpected stale recovery state: {state:?}"),
    };
    arm_boot_file_sidecar_stop_after_unlink();
    assert!(matches!(
        parent.cleanup_authenticated_stale_boot_file_until(detached, deadline()),
        Err(RetainedBootFileReplacementError::InjectedFault { .. })
    ));
    assert!(matches!(
        parent
            .reconcile_stale_boot_file_cleanup_until(
                request,
                RetainedBootFilePublicationLimits::default(),
                deadline(),
            )
            .unwrap(),
        RetainedBootFileStaleCleanupState::AlreadyClean
    ));
    assert!(!fixture.root.join("EFI").join(LEAF).exists());
    assert!(!private.exists());

    fs::write(fixture.root.join("EFI").join(LEAF), INSTALLED).unwrap();
    fs::set_permissions(
        fixture.root.join("EFI").join(LEAF),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let authority = parent
        .authenticate_stale_boot_file_cleanup_until(
            request,
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let displaced = fixture.root.join("EFI/displaced-exact-stale");
    fs::rename(fixture.root.join("EFI").join(LEAF), &displaced).unwrap();
    fs::write(fixture.root.join("EFI").join(LEAF), b"foreign replacement\n").unwrap();
    fs::set_permissions(
        fixture.root.join("EFI").join(LEAF),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(parent.cleanup_authenticated_stale_boot_file_until(authority, deadline()).is_err());
    assert_eq!(fs::read(fixture.root.join("EFI").join(LEAF)).unwrap(), b"foreign replacement\n");
    assert_eq!(fs::read(displaced).unwrap(), INSTALLED);
}
