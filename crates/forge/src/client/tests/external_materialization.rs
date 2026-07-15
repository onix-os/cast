use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use fs_err as fs;
use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use super::*;
use crate::client::external_materialization::{
    ExternalMaterializationAdmission, RetainedExternalMaterializationTarget, arm_after_parent_retained,
    arm_before_absent_target_creation, arm_before_external_fill, arm_before_external_final_proof,
};

#[test]
fn exact_empty_external_target_keeps_its_inode_and_becomes_a_sealed_empty_root() {
    let fixture = ExternalFixture::new();
    fs::create_dir(&fixture.target).unwrap();
    fs::set_permissions(&fixture.target, Permissions::from_mode(0o700)).unwrap();
    let before = directory_identity(&fixture.target);

    blit_root(&fixture.client.installation, &empty_tree(), &fixture.target).unwrap();

    assert_eq!(directory_identity(&fixture.target), before);
    assert_eq!(mode(&fixture.target), 0o755);
    assert!(fs::read_dir(&fixture.target).unwrap().next().is_none());
}

#[test]
fn an_absent_empty_closure_publishes_one_empty_directory_instead_of_deleting_a_path() {
    let fixture = ExternalFixture::new();
    assert!(!fixture.target.exists());

    blit_root(&fixture.client.installation, &empty_tree(), &fixture.target).unwrap();

    assert!(fixture.target.is_dir());
    assert_eq!(mode(&fixture.target), 0o755);
    assert!(fs::read_dir(&fixture.target).unwrap().next().is_none());
}

#[test]
fn parent_path_replacement_after_retention_cannot_redirect_target_creation() {
    let fixture = ExternalFixture::new();
    let detached = fixture.parent.with_extension("retained");
    let parent = fixture.parent.clone();
    let hook_parent = parent.clone();
    let hook_detached = detached.clone();
    arm_after_parent_retained(move || {
        fs::rename(&hook_parent, &hook_detached).unwrap();
        fs::create_dir(&hook_parent).unwrap();
        fs::set_permissions(&hook_parent, Permissions::from_mode(0o700)).unwrap();
    });

    let result = blit_root(&fixture.client.installation, &empty_tree(), &fixture.target);

    assert!(matches!(result, Err(Error::InitialMaterializationTargetChanged { .. })));
    assert!(!detached.join("root").exists());
    assert!(!parent.join("root").exists());
}

#[test]
fn directory_replacement_between_admission_and_preparation_is_rejected() {
    let fixture = ExternalFixture::new();
    fs::write(fixture.parent.join("original-sentinel"), b"original").unwrap();
    let admission = ExternalMaterializationAdmission::admit(&fixture.client.installation, &fixture.target).unwrap();
    let detached = fixture.parent.with_extension("admitted");
    fs::rename(&fixture.parent, &detached).unwrap();
    fs::create_dir(&fixture.parent).unwrap();
    fs::set_permissions(&fixture.parent, Permissions::from_mode(0o700)).unwrap();
    fs::write(fixture.parent.join("replacement-sentinel"), b"replacement").unwrap();

    let result = RetainedExternalMaterializationTarget::prepare_from(&fixture.client.installation, &admission);

    assert!(matches!(result, Err(Error::InitialMaterializationTargetChanged { .. })));
    assert!(!detached.join("root").exists());
    assert!(!fixture.target.exists());
    assert_eq!(fs::read(detached.join("original-sentinel")).unwrap(), b"original");
    assert_eq!(
        fs::read(fixture.parent.join("replacement-sentinel")).unwrap(),
        b"replacement"
    );
}

#[test]
fn symlink_replacement_between_admission_and_preparation_cannot_reach_a_safe_victim() {
    let fixture = ExternalFixture::new();
    fs::write(fixture.parent.join("original-sentinel"), b"original").unwrap();
    let admission = ExternalMaterializationAdmission::admit(&fixture.client.installation, &fixture.target).unwrap();
    let detached = fixture.parent.with_extension("admitted");
    let victim = fixture._temporary.path().join("safe-victim");
    fs::create_dir(&victim).unwrap();
    fs::set_permissions(&victim, Permissions::from_mode(0o700)).unwrap();
    fs::write(victim.join("proof"), b"untouched").unwrap();
    fs::rename(&fixture.parent, &detached).unwrap();
    symlink(&victim, &fixture.parent).unwrap();

    let result = RetainedExternalMaterializationTarget::prepare_from(&fixture.client.installation, &admission);

    assert!(matches!(result, Err(Error::InitialMaterializationTargetChanged { .. })));
    assert!(fs::symlink_metadata(&fixture.parent).unwrap().file_type().is_symlink());
    assert_eq!(fs::read(victim.join("proof")).unwrap(), b"untouched");
    assert!(!victim.join("root").exists());
    assert!(!detached.join("root").exists());
    assert_eq!(fs::read(detached.join("original-sentinel")).unwrap(), b"original");
}

#[test]
fn absent_admitted_target_rejects_an_inserted_empty_directory_untouched() {
    let fixture = ExternalFixture::new();
    fs::write(fixture.parent.join("sentinel"), b"parent").unwrap();
    let admission = ExternalMaterializationAdmission::admit(&fixture.client.installation, &fixture.target).unwrap();
    fs::create_dir(&fixture.target).unwrap();
    fs::set_permissions(&fixture.target, Permissions::from_mode(0o700)).unwrap();
    let inserted = directory_identity(&fixture.target);

    let result = RetainedExternalMaterializationTarget::prepare_from(&fixture.client.installation, &admission);

    assert!(matches!(result, Err(Error::InitialMaterializationTargetChanged { .. })));
    assert_eq!(directory_identity(&fixture.target), inserted);
    assert!(fs::read_dir(&fixture.target).unwrap().next().is_none());
    assert_eq!(fs::read(fixture.parent.join("sentinel")).unwrap(), b"parent");
}

#[test]
fn present_admitted_target_rejects_an_empty_inode_replacement_untouched() {
    let fixture = ExternalFixture::new();
    fs::create_dir(&fixture.target).unwrap();
    fs::set_permissions(&fixture.target, Permissions::from_mode(0o700)).unwrap();
    fs::write(fixture.parent.join("sentinel"), b"parent").unwrap();
    let original = directory_identity(&fixture.target);
    let admission = ExternalMaterializationAdmission::admit(&fixture.client.installation, &fixture.target).unwrap();
    let detached = fixture.target.with_extension("admitted");
    fs::rename(&fixture.target, &detached).unwrap();
    fs::create_dir(&fixture.target).unwrap();
    fs::set_permissions(&fixture.target, Permissions::from_mode(0o700)).unwrap();
    let replacement = directory_identity(&fixture.target);

    let result = RetainedExternalMaterializationTarget::prepare_from(&fixture.client.installation, &admission);

    assert!(matches!(result, Err(Error::InitialMaterializationTargetChanged { .. })));
    assert_eq!(directory_identity(&detached), original);
    assert_eq!(directory_identity(&fixture.target), replacement);
    assert_ne!(original, replacement);
    assert!(fs::read_dir(&detached).unwrap().next().is_none());
    assert!(fs::read_dir(&fixture.target).unwrap().next().is_none());
    assert_eq!(fs::read(fixture.parent.join("sentinel")).unwrap(), b"parent");
}

#[test]
fn present_admitted_target_rejects_removal_without_recreating_its_name() {
    let fixture = ExternalFixture::new();
    fs::create_dir(&fixture.target).unwrap();
    fs::set_permissions(&fixture.target, Permissions::from_mode(0o700)).unwrap();
    fs::write(fixture.parent.join("sentinel"), b"parent").unwrap();
    let original = directory_identity(&fixture.target);
    let admission = ExternalMaterializationAdmission::admit(&fixture.client.installation, &fixture.target).unwrap();
    let detached = fixture.target.with_extension("removed");
    fs::rename(&fixture.target, &detached).unwrap();

    let result = RetainedExternalMaterializationTarget::prepare_from(&fixture.client.installation, &admission);

    assert!(matches!(result, Err(Error::InitialMaterializationTargetChanged { .. })));
    assert!(!fixture.target.exists());
    assert_eq!(directory_identity(&detached), original);
    assert!(fs::read_dir(&detached).unwrap().next().is_none());
    assert_eq!(fs::read(fixture.parent.join("sentinel")).unwrap(), b"parent");
}

#[test]
fn absent_target_collision_is_never_adopted_or_removed() {
    let fixture = ExternalFixture::new();
    let target = fixture.target.clone();
    let hook_target = target.clone();
    arm_before_absent_target_creation(move || {
        fs::create_dir(&hook_target).unwrap();
        fs::set_permissions(&hook_target, Permissions::from_mode(0o700)).unwrap();
        fs::write(hook_target.join("occupant"), b"collision evidence").unwrap();
    });

    let result = blit_root(&fixture.client.installation, &empty_tree(), &target);

    assert!(matches!(result, Err(Error::InitialMaterializationTargetChanged { .. })));
    assert_eq!(fs::read(target.join("occupant")).unwrap(), b"collision evidence");
}

#[test]
fn target_substitution_before_fill_preserves_both_inodes_without_writing_either() {
    let fixture = ExternalFixture::new();
    fs::create_dir(&fixture.target).unwrap();
    fs::set_permissions(&fixture.target, Permissions::from_mode(0o700)).unwrap();
    let retained = directory_identity(&fixture.target);
    let detached = fixture.target.with_extension("retained");
    let target = fixture.target.clone();
    let hook_target = target.clone();
    let hook_detached = detached.clone();
    arm_before_external_fill(move || {
        fs::rename(&hook_target, &hook_detached).unwrap();
        fs::create_dir(&hook_target).unwrap();
        fs::set_permissions(&hook_target, Permissions::from_mode(0o700)).unwrap();
        fs::write(hook_target.join("occupant"), b"replacement").unwrap();
    });

    let result = blit_root(&fixture.client.installation, &directory_tree(), &target);

    assert!(matches!(result, Err(Error::InitialMaterializationTargetChanged { .. })));
    assert_eq!(directory_identity(&detached), retained);
    assert!(fs::read_dir(&detached).unwrap().next().is_none());
    assert_eq!(fs::read(target.join("occupant")).unwrap(), b"replacement");
}

#[test]
fn final_name_substitution_never_turns_a_filled_retained_root_into_success() {
    let fixture = ExternalFixture::new();
    fs::create_dir(&fixture.target).unwrap();
    fs::set_permissions(&fixture.target, Permissions::from_mode(0o700)).unwrap();
    let retained = directory_identity(&fixture.target);
    let detached = fixture.target.with_extension("filled");
    let target = fixture.target.clone();
    let hook_target = target.clone();
    let hook_detached = detached.clone();
    arm_before_external_final_proof(move || {
        fs::rename(&hook_target, &hook_detached).unwrap();
        fs::create_dir(&hook_target).unwrap();
        fs::set_permissions(&hook_target, Permissions::from_mode(0o700)).unwrap();
        fs::write(hook_target.join("occupant"), b"late replacement").unwrap();
    });

    let result = blit_root(&fixture.client.installation, &directory_tree(), &target);

    assert!(matches!(result, Err(Error::InitialMaterializationTargetChanged { .. })));
    assert_eq!(directory_identity(&detached), retained);
    assert!(detached.join("usr/share/external-proof").is_dir());
    assert_eq!(fs::read(target.join("occupant")).unwrap(), b"late replacement");
}

#[test]
fn symlink_and_nonempty_targets_are_left_untouched() {
    let fixture = ExternalFixture::new();
    let sentinel = fixture.parent.join("sentinel");
    fs::create_dir(&sentinel).unwrap();
    fs::write(sentinel.join("proof"), b"sentinel").unwrap();
    symlink(&sentinel, &fixture.target).unwrap();

    assert!(blit_root(&fixture.client.installation, &empty_tree(), &fixture.target).is_err());
    assert!(fs::symlink_metadata(&fixture.target).unwrap().file_type().is_symlink());
    assert_eq!(fs::read(sentinel.join("proof")).unwrap(), b"sentinel");

    fs::remove_file(&fixture.target).unwrap();
    fs::create_dir(&fixture.target).unwrap();
    fs::set_permissions(&fixture.target, Permissions::from_mode(0o700)).unwrap();
    fs::write(fixture.target.join("proof"), b"nonempty").unwrap();
    let before = directory_identity(&fixture.target);

    assert!(blit_root(&fixture.client.installation, &empty_tree(), &fixture.target).is_err());
    assert_eq!(directory_identity(&fixture.target), before);
    assert_eq!(fs::read(fixture.target.join("proof")).unwrap(), b"nonempty");
}

#[test]
fn world_writable_direct_parent_is_rejected_without_creating_or_removing_a_target() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    fs::create_dir(&installation_root).unwrap();
    let client = stateful_test_client(&installation_root);
    let parent = temporary.path().join("shared");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, Permissions::from_mode(0o777)).unwrap();
    fs::write(parent.join("sentinel"), b"retain").unwrap();
    let target = parent.join("root");

    let result = blit_root(&client.installation, &empty_tree(), &target);

    assert!(matches!(result, Err(Error::UnsafeInitialMaterializationParent { .. })));
    assert!(!target.exists());
    assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"retain");
}

struct ExternalFixture {
    _temporary: tempfile::TempDir,
    client: Client,
    parent: PathBuf,
    target: PathBuf,
}

impl ExternalFixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        fs::create_dir(&installation_root).unwrap();
        let client = stateful_test_client(&installation_root);
        let parent = temporary.path().join("external");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, Permissions::from_mode(0o700)).unwrap();
        let target = parent.join("root");
        Self {
            _temporary: temporary,
            client,
            parent,
            target,
        }
    }
}

fn empty_tree() -> vfs::Tree<PendingFile> {
    vfs(Vec::new()).unwrap()
}

fn directory_tree() -> vfs::Tree<PendingFile> {
    vfs(vec![(
        package::Id::from("external-proof"),
        StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFDIR | 0o755,
            tag: 0,
            file: StonePayloadLayoutFile::Directory("share/external-proof".into()),
        },
    )])
    .unwrap()
}

fn directory_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}
