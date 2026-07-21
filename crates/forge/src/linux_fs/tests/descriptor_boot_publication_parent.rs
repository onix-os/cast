use std::{
    cell::RefCell,
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use sha2::{Digest as _, Sha256};
use xxhash_rust::xxh3::xxh3_128;

use crate::linux_fs::{
    descriptor_boot_namespace::RetainedBootNamespaceExpectedSource,
    mount_namespace::{
        FixtureRetainedBootPublicationParentCheckpoint as ParentCheckpoint,
        FixtureRetainedBootPublicationParentFault as ParentFault, PreparedMountNamespaceAnchor,
        PreparedTaskRootedAttachment, RetainedBootFilePublicationLimits, RetainedBootFilePublicationOutcome,
        RetainedBootFilePublicationRequest, RetainedBootPublicationParentError,
        arm_retained_boot_publication_parent_checkpoint_hook, arm_retained_boot_publication_parent_fault,
        validate_fixture_boot_publication_parent_identity, validate_fixture_boot_publication_parent_policy,
    },
};

const LEAF: &str = "vmlinuz-test";

struct ParentFixture {
    temporary: tempfile::TempDir,
    root: PathBuf,
    anchor: PreparedMountNamespaceAnchor,
    attachment: PreparedTaskRootedAttachment,
}

impl ParentFixture {
    fn new(prefix: &str) -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("forge manifest has one workspace parent")
            .join("target");
        fs::create_dir_all(&target).unwrap();
        let temporary = tempfile::Builder::new().prefix(prefix).tempdir_in(target).unwrap();
        let root = temporary.path().join("boot-root");
        fs::create_dir(&root).unwrap();
        set_mode(&root, 0o755);
        let anchor = PreparedMountNamespaceAnchor::prepare().unwrap();
        let attachment = anchor
            .revalidate()
            .unwrap()
            .prepare_task_rooted_attachment(root.to_str().unwrap())
            .unwrap();
        Self {
            temporary,
            root,
            anchor,
            attachment,
        }
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn create_safe_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    set_mode(path, 0o755);
}

#[test]
fn existing_parent_chain_is_reused_without_inode_replacement() {
    let fixture = ParentFixture::new("forge-boot-parent-existing-");
    create_safe_directory(&fixture.root.join("EFI"));
    create_safe_directory(&fixture.root.join("EFI/Linux"));
    let first = fs::metadata(fixture.root.join("EFI")).unwrap().ino();
    let second = fs::metadata(fixture.root.join("EFI/Linux")).unwrap().ino();
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();

    let parent = view
        .retain_boot_publication_parent_until(&["EFI", "Linux"], deadline())
        .unwrap();

    assert_eq!(parent.component_count(), 2);
    assert_eq!(parent.root_device(), view.destination_device());
    assert_eq!(parent.root_inode(), view.destination_inode());
    assert_eq!(parent.root_mount_id(), view.destination_mount_id());
    assert_eq!(parent.destination_inode(), second);
    assert_eq!(fs::metadata(fixture.root.join("EFI")).unwrap().ino(), first);
    assert_eq!(fs::metadata(fixture.root.join("EFI/Linux")).unwrap().ino(), second);
    assert!(fs::read_dir(&fixture.root).unwrap().all(|entry| entry.unwrap().file_name() == "EFI"));
}

#[test]
fn multi_component_chain_is_created_retained_and_same_root_bound() {
    let fixture = ParentFixture::new("forge-boot-parent-create-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();

    let parent = view
        .retain_boot_publication_parent_until(&["EFI", "Linux", "generations"], deadline())
        .unwrap();

    assert_eq!(parent.component_count(), 3);
    assert_eq!(parent.destination_device(), parent.root_device());
    assert_eq!(parent.destination_mount_id(), parent.root_mount_id());
    assert_ne!(parent.destination_inode(), parent.root_inode());
    for path in ["EFI", "EFI/Linux", "EFI/Linux/generations"] {
        let metadata = fs::metadata(fixture.root.join(path)).unwrap();
        assert!(metadata.file_type().is_dir());
        assert_eq!(metadata.permissions().mode() & 0o022, 0);
    }
}

#[test]
fn mkdir_error_report_after_applied_is_reconciled_without_second_attempt() {
    let fixture = ParentFixture::new("forge-boot-parent-mkdir-report-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    arm_retained_boot_publication_parent_fault(ParentFault::MkdirReportsErrorAfterApplied {
        component_index: 0,
    });

    let parent = view
        .retain_boot_publication_parent_until(&["EFI"], deadline())
        .unwrap();

    assert_eq!(parent.component_count(), 1);
    assert!(fixture.root.join("EFI").is_dir());
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 1);
}

#[test]
fn interrupted_creation_residue_is_re_admitted_with_the_same_inode() {
    let fixture = ParentFixture::new("forge-boot-parent-residue-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    arm_retained_boot_publication_parent_fault(ParentFault::AfterCreationBeforeDurability {
        component_index: 0,
    });

    assert!(matches!(
        view.retain_boot_publication_parent_until(&["EFI"], deadline()),
        Err(RetainedBootPublicationParentError::InjectedFault { index: 0 })
    ));
    let residue_inode = fs::metadata(fixture.root.join("EFI")).unwrap().ino();

    let admitted = view
        .retain_boot_publication_parent_until(&["EFI"], deadline())
        .unwrap();
    assert_eq!(admitted.destination_inode(), residue_inode);
}

#[test]
fn directory_durability_runs_deepest_child_to_root_before_filesystem_and_terminal_checks() {
    let fixture = ParentFixture::new("forge-boot-parent-sync-order-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let observed = Rc::new(RefCell::new(Vec::new()));
    let hook_output = Rc::clone(&observed);
    let _hook = arm_retained_boot_publication_parent_checkpoint_hook(move |point| {
        hook_output.borrow_mut().push(point);
    });

    view.retain_boot_publication_parent_until(&["EFI", "Linux", "generations"], deadline())
        .unwrap();

    let observed = observed.borrow();
    let before_sync: Vec<_> = observed
        .iter()
        .filter_map(|point| match point {
            ParentCheckpoint::BeforeDirectorySync { depth } => Some(*depth),
            _ => None,
        })
        .collect();
    assert_eq!(before_sync, [3, 2, 1, 0]);
    let filesystem = observed
        .iter()
        .position(|point| *point == ParentCheckpoint::BeforeFilesystemSync)
        .unwrap();
    let terminal = observed
        .iter()
        .position(|point| *point == ParentCheckpoint::BeforeTerminalRevalidation)
        .unwrap();
    assert!(filesystem < terminal);
    assert!(observed[..filesystem]
        .iter()
        .any(|point| *point == ParentCheckpoint::AfterDirectorySync { depth: 0 }));
}

#[test]
fn terminal_name_substitution_is_preserved_but_refused() {
    let fixture = ParentFixture::new("forge-boot-parent-substitution-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let canonical = fixture.root.join("EFI");
    let displaced = fixture.root.join("EFI-displaced");
    let canonical_for_hook = canonical.clone();
    let displaced_for_hook = displaced.clone();
    let _hook = arm_retained_boot_publication_parent_checkpoint_hook(move |point| {
        if point == ParentCheckpoint::BeforeTerminalRevalidation {
            fs::rename(&canonical_for_hook, &displaced_for_hook).unwrap();
            create_safe_directory(&canonical_for_hook);
        }
    });

    assert!(view
        .retain_boot_publication_parent_until(&["EFI"], deadline())
        .is_err());
    assert!(canonical.is_dir());
    assert!(displaced.is_dir());
    assert_ne!(fs::metadata(canonical).unwrap().ino(), fs::metadata(displaced).unwrap().ino());
}

#[test]
fn intermediate_parent_substitution_is_refused_before_deeper_creation() {
    let fixture = ParentFixture::new("forge-boot-parent-intermediate-substitution-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let canonical = fixture.root.join("EFI");
    let displaced = fixture.root.join("EFI-displaced");
    let canonical_for_hook = canonical.clone();
    let displaced_for_hook = displaced.clone();
    let _hook = arm_retained_boot_publication_parent_checkpoint_hook(move |point| {
        if matches!(point, ParentCheckpoint::DirectoryRetained { depth: 1, .. }) {
            fs::rename(&canonical_for_hook, &displaced_for_hook).unwrap();
            create_safe_directory(&canonical_for_hook);
        }
    });

    assert!(view
        .retain_boot_publication_parent_until(&["EFI", "Linux"], deadline())
        .is_err());
    assert!(canonical.is_dir());
    assert!(displaced.is_dir());
    assert!(!canonical.join("Linux").exists());
    assert!(!displaced.join("Linux").exists());
}

#[test]
fn regular_symlink_and_writable_directory_components_are_refused_without_replacement() {
    let fixture = ParentFixture::new("forge-boot-parent-types-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let component = fixture.root.join("EFI");
    fs::write(&component, b"foreign regular file").unwrap();
    assert!(view
        .retain_boot_publication_parent_until(&["EFI"], deadline())
        .is_err());
    assert_eq!(fs::read(&component).unwrap(), b"foreign regular file");

    fs::remove_file(&component).unwrap();
    symlink(fixture.temporary.path(), &component).unwrap();
    assert!(view
        .retain_boot_publication_parent_until(&["EFI"], deadline())
        .is_err());
    assert!(fs::symlink_metadata(&component).unwrap().file_type().is_symlink());

    fs::remove_file(&component).unwrap();
    create_safe_directory(&component);
    set_mode(&component, 0o777);
    assert!(matches!(
        view.retain_boot_publication_parent_until(&["EFI"], deadline()),
        Err(RetainedBootPublicationParentError::UnsafeDirectoryPolicy { index: 0 })
    ));
    assert_eq!(fs::metadata(&component).unwrap().permissions().mode() & 0o7777, 0o777);
}

#[test]
fn foreign_device_and_mount_id_are_rejected_by_the_closed_identity_policy() {
    validate_fixture_boot_publication_parent_identity(11, 22, 11, 22).unwrap();
    assert!(validate_fixture_boot_publication_parent_identity(11, 22, 12, 22).is_err());
    assert!(validate_fixture_boot_publication_parent_identity(11, 22, 11, 23).is_err());
    assert!(validate_fixture_boot_publication_parent_identity(11, 22, 12, 23).is_err());
}

#[test]
fn root_credentials_and_child_owner_group_mode_drift_are_rejected() {
    validate_fixture_boot_publication_parent_policy(1000, 100, 1000, 100, 0o755, 1000, 100, 0o750)
        .unwrap();
    for policy in [
        (1001, 100, 1000, 100, 0o755, 1000, 100, 0o755),
        (1000, 101, 1000, 100, 0o755, 1000, 100, 0o755),
        (1000, 100, 1000, 100, 0o775, 1000, 100, 0o755),
        (1000, 100, 1000, 100, 0o1755, 1000, 100, 0o755),
        (1000, 100, 1000, 100, 0o755, 1001, 100, 0o755),
        (1000, 100, 1000, 100, 0o755, 1000, 101, 0o755),
        (1000, 100, 1000, 100, 0o755, 1000, 100, 0o770),
        (1000, 100, 1000, 100, 0o755, 1000, 100, 0o2755),
        (1000, 100, 1000, 100, 0o755, 1000, 100, 0o655),
    ] {
        assert!(validate_fixture_boot_publication_parent_policy(
            policy.0, policy.1, policy.2, policy.3, policy.4, policy.5, policy.6, policy.7,
        )
        .is_err());
    }
}

#[test]
fn only_nonempty_bounded_raw_parent_components_reach_the_syscall_boundary() {
    let fixture = ParentFixture::new("forge-boot-parent-components-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    for components in [Vec::<&str>::new(), vec![""], vec!["."], vec![".."], vec!["EFI/Linux"]] {
        assert!(view
            .retain_boot_publication_parent_until(&components, deadline())
            .is_err());
    }
    let too_many = vec!["safe"; 16];
    assert!(matches!(
        view.retain_boot_publication_parent_until(&too_many, deadline()),
        Err(RetainedBootPublicationParentError::ComponentLimit { limit: 15, actual: 16 })
    ));
    assert!(fs::read_dir(&fixture.root).unwrap().next().is_none());
}

#[test]
fn nested_parent_consumes_the_leaf_engine_and_binds_reusable_source_evidence() {
    let fixture = ParentFixture::new("forge-boot-parent-leaf-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let parent = view
        .retain_boot_publication_parent_until(&["EFI", "Linux"], deadline())
        .unwrap();
    let bytes = b"nested retained boot payload\n";
    let source = RetainedBootNamespaceExpectedSource::generated(bytes);
    let request = RetainedBootFilePublicationRequest::new(
        LEAF,
        bytes.len() as u64,
        xxh3_128(bytes),
        Sha256::digest(bytes).into(),
    );

    let published = parent
        .publish_immutable_boot_file_until(
            request,
            &source,
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    assert_eq!(published.outcome(), RetainedBootFilePublicationOutcome::Published);
    assert!(parent.matches_leaf_evidence(&published));
    assert_eq!(published.destination_inode(), parent.destination_inode());
    assert_eq!(published.destination_device(), parent.root_device());
    assert_eq!(published.destination_mount_id(), parent.root_mount_id());
    assert_eq!(fs::read(fixture.root.join("EFI/Linux").join(LEAF)).unwrap(), bytes);

    let exact = parent
        .publish_immutable_boot_file_until(
            request,
            &source,
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    assert_eq!(exact.outcome(), RetainedBootFilePublicationOutcome::AlreadyExact);
    assert!(parent.matches_leaf_evidence(&exact));
    assert_eq!(published.file_inode(), exact.file_inode());
}
