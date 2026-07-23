use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use sha2::{Digest as _, Sha256};
use xxhash_rust::xxh3::xxh3_128;

use crate::linux_fs::mount_namespace::{
    FixtureRetainedBootLeafAssessmentHookGuard, PreparedMountNamespaceAnchor,
    PreparedTaskRootedAttachment, RetainedBootLeafAssessmentError,
    RetainedBootLeafAssessmentLimits, RetainedBootLeafAssessmentRequest,
    RetainedBootLeafAssessmentState, ValidatedRetainedBootLeafAssessment,
    arm_retained_boot_leaf_assessment_terminal_rebind_hook,
};

const LEAF: &str = "vmlinuz-test";
const EXPECTED: &[u8] = b"receipt-bound boot payload\n";

struct AssessmentFixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    anchor: PreparedMountNamespaceAnchor,
    attachment: PreparedTaskRootedAttachment,
}

impl AssessmentFixture {
    fn new(prefix: &str) -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("forge manifest has one workspace parent")
            .join("target");
        fs::create_dir_all(&target).unwrap();
        let temporary = tempfile::Builder::new().prefix(prefix).tempdir_in(target).unwrap();
        let root = temporary.path().join("boot-root");
        create_safe_directory(&root);
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

    fn assess(
        &self,
        parents: &[&str],
        request: RetainedBootLeafAssessmentRequest<'_>,
    ) -> Result<ValidatedRetainedBootLeafAssessment, RetainedBootLeafAssessmentError> {
        self.assess_with(parents, request, RetainedBootLeafAssessmentLimits::default(), deadline())
    }

    fn assess_with(
        &self,
        parents: &[&str],
        request: RetainedBootLeafAssessmentRequest<'_>,
        limits: RetainedBootLeafAssessmentLimits,
        deadline: Instant,
    ) -> Result<ValidatedRetainedBootLeafAssessment, RetainedBootLeafAssessmentError> {
        self.attachment
            .revalidate_against(&self.anchor)
            .unwrap()
            .assess_boot_leaf_below_parent_until(parents, request, limits, deadline)
    }

    fn parent(&self) -> PathBuf {
        self.root.join("EFI")
    }

    fn leaf(&self) -> PathBuf {
        self.parent().join(LEAF)
    }
}

fn request(bytes: &[u8]) -> RetainedBootLeafAssessmentRequest<'static> {
    RetainedBootLeafAssessmentRequest::new(
        LEAF,
        bytes.len() as u64,
        xxh3_128(bytes),
        Sha256::digest(bytes).into(),
    )
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn create_safe_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    set_mode(path, 0o755);
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn write_leaf(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    set_mode(path, 0o644);
}

#[test]
fn missing_parent_is_absent_without_creating_or_synchronizing_it() {
    let fixture = AssessmentFixture::new("forge-boot-leaf-missing-parent-");
    create_safe_directory(&fixture.parent());
    let missing = fixture.parent().join("Linux");

    let evidence = fixture.assess(&["EFI", "Linux"], request(EXPECTED)).unwrap();

    assert_eq!(evidence.state(), RetainedBootLeafAssessmentState::Absent);
    assert_eq!(evidence.parent_components().collect::<Vec<_>>(), ["EFI", "Linux"]);
    assert_eq!(evidence.canonical_leaf(), LEAF);
    assert_eq!(evidence.expected_length(), EXPECTED.len() as u64);
    assert_eq!(evidence.expected_xxh3(), xxh3_128(EXPECTED));
    assert_eq!(evidence.expected_sha256(), <[u8; 32]>::from(Sha256::digest(EXPECTED)));
    assert_eq!(evidence.retained_parent_device(), None);
    assert_eq!(evidence.exact_file_inode(), None);
    assert!(!missing.exists());
    assert_eq!(fs::read_dir(&fixture.parent()).unwrap().count(), 0);
}

#[test]
fn missing_leaf_is_absent_but_binds_the_existing_retained_parent() {
    let fixture = AssessmentFixture::new("forge-boot-leaf-absent-");
    create_safe_directory(&fixture.parent());
    let root = fs::metadata(&fixture.root).unwrap();
    let parent = fs::metadata(fixture.parent()).unwrap();

    let evidence = fixture.assess(&["EFI"], request(EXPECTED)).unwrap();

    assert_eq!(evidence.state(), RetainedBootLeafAssessmentState::Absent);
    assert_eq!(evidence.assessment_root_device(), root.dev());
    assert_eq!(evidence.assessment_root_inode(), root.ino());
    assert_eq!(evidence.retained_parent_device(), Some(parent.dev()));
    assert_eq!(evidence.retained_parent_inode(), Some(parent.ino()));
    assert_eq!(evidence.retained_parent_mount_id(), Some(evidence.assessment_root_mount_id()));
    assert_eq!(evidence.exact_file_device(), None);
}

#[test]
fn exact_regular_mode_0644_single_link_binds_both_hashes_and_inode() {
    let fixture = AssessmentFixture::new("forge-boot-leaf-exact-");
    create_safe_directory(&fixture.parent());
    write_leaf(&fixture.leaf(), EXPECTED);
    let file = fs::metadata(fixture.leaf()).unwrap();

    let evidence = fixture.assess(&["EFI"], request(EXPECTED)).unwrap();

    assert_eq!(evidence.state(), RetainedBootLeafAssessmentState::Exact);
    assert_eq!(evidence.exact_file_device(), Some(file.dev()));
    assert_eq!(evidence.exact_file_inode(), Some(file.ino()));
    assert_eq!(evidence.exact_file_mount_id(), evidence.retained_parent_mount_id());
    assert_eq!(fs::read(fixture.leaf()).unwrap(), EXPECTED);
}

#[test]
fn stable_regular_content_mode_and_length_mismatches_are_different() {
    let fixture = AssessmentFixture::new("forge-boot-leaf-different-");
    create_safe_directory(&fixture.parent());

    let mut different_content = EXPECTED.to_vec();
    different_content[0] ^= 0x20;
    write_leaf(&fixture.leaf(), &different_content);
    let content = fixture.assess(&["EFI"], request(EXPECTED)).unwrap();
    assert_eq!(content.state(), RetainedBootLeafAssessmentState::Different);
    assert_eq!(content.exact_file_inode(), None);

    write_leaf(&fixture.leaf(), EXPECTED);
    set_mode(&fixture.leaf(), 0o000);
    let mode = fixture.assess(&["EFI"], request(EXPECTED)).unwrap();
    assert_eq!(mode.state(), RetainedBootLeafAssessmentState::Different);

    set_mode(&fixture.leaf(), 0o644);
    write_leaf(&fixture.leaf(), b"short");
    let length = fixture.assess(&["EFI"], request(EXPECTED)).unwrap();
    assert_eq!(length.state(), RetainedBootLeafAssessmentState::Different);
}

#[test]
fn symlink_nonregular_and_hardlinked_leaves_fail_closed() {
    let fixture = AssessmentFixture::new("forge-boot-leaf-types-");
    create_safe_directory(&fixture.parent());
    let target = fixture.parent().join("target");
    write_leaf(&target, EXPECTED);
    symlink("target", fixture.leaf()).unwrap();
    assert!(matches!(
        fixture.assess(&["EFI"], request(EXPECTED)),
        Err(RetainedBootLeafAssessmentError::UnsafeLeafType)
    ));

    fs::remove_file(fixture.leaf()).unwrap();
    create_safe_directory(&fixture.leaf());
    assert!(matches!(
        fixture.assess(&["EFI"], request(EXPECTED)),
        Err(RetainedBootLeafAssessmentError::UnsafeLeafType)
    ));

    fs::remove_dir(fixture.leaf()).unwrap();
    fs::hard_link(&target, fixture.leaf()).unwrap();
    assert!(matches!(
        fixture.assess(&["EFI"], request(EXPECTED)),
        Err(RetainedBootLeafAssessmentError::UnsafeLinkCount { found: 2 })
    ));
}

#[test]
fn parent_symlink_non_directory_and_unsafe_policy_fail_closed() {
    let symlink_fixture = AssessmentFixture::new("forge-boot-leaf-parent-symlink-");
    create_safe_directory(&symlink_fixture.root.join("real"));
    symlink("real", symlink_fixture.parent()).unwrap();
    assert!(matches!(
        symlink_fixture.assess(&["EFI"], request(EXPECTED)),
        Err(RetainedBootLeafAssessmentError::UnsafeParentType { index: 0 })
    ));

    let file_fixture = AssessmentFixture::new("forge-boot-leaf-parent-file-");
    fs::write(file_fixture.parent(), b"not a directory").unwrap();
    assert!(matches!(
        file_fixture.assess(&["EFI"], request(EXPECTED)),
        Err(RetainedBootLeafAssessmentError::UnsafeParentType { index: 0 })
    ));

    let policy_fixture = AssessmentFixture::new("forge-boot-leaf-parent-policy-");
    create_safe_directory(&policy_fixture.parent());
    set_mode(&policy_fixture.parent(), 0o777);
    assert!(matches!(
        policy_fixture.assess(&["EFI"], request(EXPECTED)),
        Err(RetainedBootLeafAssessmentError::UnsafeParentPolicy { index: 0 })
    ));
}

#[test]
fn leaf_and_parent_substitution_windows_are_rejected() {
    let leaf_fixture = AssessmentFixture::new("forge-boot-leaf-race-leaf-");
    create_safe_directory(&leaf_fixture.parent());
    write_leaf(&leaf_fixture.leaf(), EXPECTED);
    let leaf = leaf_fixture.leaf();
    let displaced = leaf_fixture.parent().join("displaced");
    let guard: FixtureRetainedBootLeafAssessmentHookGuard =
        arm_retained_boot_leaf_assessment_terminal_rebind_hook(move || {
            fs::rename(&leaf, &displaced).unwrap();
            write_leaf(&leaf, EXPECTED);
        });
    let error = leaf_fixture.assess(&["EFI"], request(EXPECTED)).unwrap_err();
    drop(guard);
    assert!(
        matches!(error, RetainedBootLeafAssessmentError::LeafIdentityChanged { .. }),
        "unexpected leaf-substitution race error: {error:?}"
    );

    let parent_fixture = AssessmentFixture::new("forge-boot-leaf-race-parent-");
    create_safe_directory(&parent_fixture.parent());
    write_leaf(&parent_fixture.leaf(), EXPECTED);
    let parent = parent_fixture.parent();
    let moved = parent_fixture.root.join("moved");
    let guard: FixtureRetainedBootLeafAssessmentHookGuard =
        arm_retained_boot_leaf_assessment_terminal_rebind_hook(move || {
            fs::rename(&parent, &moved).unwrap();
        });
    let error = parent_fixture.assess(&["EFI"], request(EXPECTED)).unwrap_err();
    drop(guard);
    assert!(matches!(error, RetainedBootLeafAssessmentError::ParentRevalidation { .. }));
}

#[test]
fn missing_parent_appearance_request_bounds_and_deadline_fail_closed() {
    let fixture = AssessmentFixture::new("forge-boot-leaf-race-missing-");
    let parent = fixture.parent();
    let guard: FixtureRetainedBootLeafAssessmentHookGuard =
        arm_retained_boot_leaf_assessment_terminal_rebind_hook(move || create_safe_directory(&parent));
    let error = fixture.assess(&["EFI"], request(EXPECTED)).unwrap_err();
    drop(guard);
    assert!(
        matches!(
            error,
            RetainedBootLeafAssessmentError::LeafIdentityChanged { .. }
                | RetainedBootLeafAssessmentError::AttachmentIdentityChanged { .. }
        ),
        "unexpected missing-parent race error: {error:?}"
    );

    assert!(matches!(
        fixture.assess(&[], request(EXPECTED)),
        Err(RetainedBootLeafAssessmentError::EmptyParentComponents)
    ));
    assert!(matches!(
        fixture.assess(&["../EFI"], request(EXPECTED)),
        Err(RetainedBootLeafAssessmentError::InvalidParentComponent { index: 0 })
    ));
    let too_many = ["p"; 16];
    assert!(matches!(
        fixture.assess(&too_many, request(EXPECTED)),
        Err(RetainedBootLeafAssessmentError::ParentComponentLimit { limit: 15, actual: 16 })
    ));
    let short_limits = RetainedBootLeafAssessmentLimits {
        max_read_bytes: (EXPECTED.len() - 1) as u64,
        max_read_calls: 8,
    };
    assert!(matches!(
        fixture.assess_with(&["EFI"], request(EXPECTED), short_limits, deadline()),
        Err(RetainedBootLeafAssessmentError::LengthLimitExceeded { .. })
    ));
    let call_limits = RetainedBootLeafAssessmentLimits {
        max_read_bytes: EXPECTED.len() as u64,
        max_read_calls: 1,
    };
    assert!(matches!(
        fixture.assess_with(&["EFI"], request(EXPECTED), call_limits, deadline()),
        Err(RetainedBootLeafAssessmentError::ReadCallLimitExceeded { required: 2, limit: 1 })
    ));
    assert!(matches!(
        fixture.assess_with(
            &["EFI"],
            request(EXPECTED),
            RetainedBootLeafAssessmentLimits::default(),
            Instant::now() - Duration::from_millis(1),
        ),
        Err(RetainedBootLeafAssessmentError::DeadlineExceeded { .. })
    ));
}
