use std::{
    fs::{self, File},
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
        FixtureRetainedBootFilePublicationFault, PreparedMountNamespaceAnchor, PreparedTaskRootedAttachment,
        RetainedBootFilePublicationError, RetainedBootFilePublicationLimits, RetainedBootFilePublicationOutcome,
        RetainedBootFilePublicationRequest, arm_retained_boot_file_private_name_substitution,
        arm_retained_boot_file_publication_fault,
    },
};

const FULL_SEALS: i32 =
    nix::libc::F_SEAL_WRITE | nix::libc::F_SEAL_GROW | nix::libc::F_SEAL_SHRINK | nix::libc::F_SEAL_SEAL;
const LEAF: &str = "vmlinuz";

struct PublicationFixture {
    temporary: tempfile::TempDir,
    destination: PathBuf,
    anchor: PreparedMountNamespaceAnchor,
    attachment: PreparedTaskRootedAttachment,
}

impl PublicationFixture {
    fn new(prefix: &str) -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("forge manifest has one workspace parent")
            .join("target");
        fs::create_dir_all(&target).unwrap();
        let temporary = tempfile::Builder::new().prefix(prefix).tempdir_in(target).unwrap();
        let destination = temporary.path().join("destination");
        fs::create_dir(&destination).unwrap();
        let anchor = PreparedMountNamespaceAnchor::prepare().unwrap();
        let selector = destination.to_str().unwrap();
        let attachment = anchor
            .revalidate()
            .unwrap()
            .prepare_task_rooted_attachment(selector)
            .unwrap();
        Self {
            temporary,
            destination,
            anchor,
            attachment,
        }
    }

    fn publish_generated(
        &self,
        bytes: &[u8],
    ) -> Result<crate::linux_fs::mount_namespace::ValidatedRetainedBootFilePublication, RetainedBootFilePublicationError>
    {
        self.publish_generated_with_sha(bytes, Sha256::digest(bytes).into())
    }

    fn publish_generated_with_sha(
        &self,
        bytes: &[u8],
        sha256: [u8; 32],
    ) -> Result<crate::linux_fs::mount_namespace::ValidatedRetainedBootFilePublication, RetainedBootFilePublicationError>
    {
        let view = self.attachment.revalidate_against(&self.anchor).unwrap();
        view.publish_immutable_boot_file_until(
            request(bytes, sha256),
            RetainedBootNamespaceExpectedSource::generated(bytes),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
    }

    fn canonical(&self) -> PathBuf {
        self.destination.join(LEAF)
    }

    fn private_path(&self) -> PathBuf {
        let mut private = fs::read_dir(&self.destination)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".cast-payload-") && name.ends_with(".stage"))
            });
        let found = private.next().expect("fixture has one deterministic private residue");
        assert!(private.next().is_none(), "fixture has more than one private residue");
        found
    }
}

fn request(bytes: &[u8], sha256: [u8; 32]) -> RetainedBootFilePublicationRequest<'static> {
    RetainedBootFilePublicationRequest::new(LEAF, bytes.len() as u64, xxh3_128(bytes), sha256)
}

fn request_for_leaf<'leaf>(
    leaf: &'leaf str,
    bytes: &[u8],
    sha256: [u8; 32],
) -> RetainedBootFilePublicationRequest<'leaf> {
    RetainedBootFilePublicationRequest::new(leaf, bytes.len() as u64, xxh3_128(bytes), sha256)
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn sealed_memfd(bytes: &[u8]) -> File {
    // SAFETY: the static name remains live for the one memfd_create call.
    let raw = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_memfd_create,
            c"forge-boot-file-source".as_ptr(),
            nix::libc::MFD_CLOEXEC | nix::libc::MFD_ALLOW_SEALING,
        )
    };
    assert!(raw >= 0, "memfd_create failed: {}", std::io::Error::last_os_error());
    let raw = i32::try_from(raw).unwrap();
    // SAFETY: successful memfd_create returned this fresh owned descriptor.
    let owned = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut file = File::from(owned);
    file.write_all(bytes).unwrap();
    // SAFETY: fchmod consumes only the live descriptor and integer mode.
    assert_eq!(unsafe { nix::libc::fchmod(file.as_raw_fd(), 0o400) }, 0);
    // SAFETY: F_ADD_SEALS consumes only the live descriptor and fixed mask.
    assert_eq!(unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_ADD_SEALS, FULL_SEALS) }, 0);
    file
}

fn assert_nonexact_mode_0644_residue(
    fault: FixtureRetainedBootFilePublicationFault,
    bytes: &[u8],
    expected_residue: &[u8],
) {
    let fixture = PublicationFixture::new("forge-boot-leaf-nonexact-residue-");
    arm_retained_boot_file_publication_fault(fault);

    let first = fixture.publish_generated(bytes);

    assert!(matches!(first, Err(RetainedBootFilePublicationError::InjectedFault { .. })));
    assert!(!fixture.canonical().exists());
    let private = fixture.private_path();
    let opening = fs::metadata(&private).unwrap();
    assert_eq!(opening.permissions().mode() & 0o7777, 0o644);
    assert_eq!(opening.nlink(), 1);
    assert_eq!(fs::read(&private).unwrap(), expected_residue);

    let retry = fixture.publish_generated(bytes);

    assert!(retry.is_err(), "an unowned incomplete attempt must not be adopted or rewritten");
    assert!(!fixture.canonical().exists());
    let closing = fs::metadata(&private).unwrap();
    assert_eq!(closing.ino(), opening.ino());
    assert_eq!(closing.permissions().mode() & 0o7777, 0o644);
    assert_eq!(fs::read(&private).unwrap(), expected_residue);
}

#[test]
fn canonical_request_cannot_alias_private_stage_namespace_case_insensitively() {
    let bytes = b"reserved namespace payload\n";
    for (index, leaf) in [
        ".cast-payload-canonical-alias.stage",
        ".CaSt-PaYlOaD-cross-request-alias.STAGE",
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = PublicationFixture::new(&format!("forge-boot-leaf-reserved-{index}-"));
        let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
        let error = view
            .publish_immutable_boot_file_until(
                request_for_leaf(leaf, bytes, Sha256::digest(bytes).into()),
                RetainedBootNamespaceExpectedSource::generated(bytes),
                RetainedBootFilePublicationLimits::default(),
                deadline(),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            RetainedBootFilePublicationError::ReservedPrivatePublicationLeaf
        ));
        assert_eq!(fs::read_dir(&fixture.destination).unwrap().count(), 0);
    }
}

#[test]
fn stop_after_exclusive_creation_preserves_empty_mode_0644_residue_and_retry_refuses() {
    assert_nonexact_mode_0644_residue(
        FixtureRetainedBootFilePublicationFault::AfterExclusiveCreation,
        b"bytes never streamed after exclusive creation\n",
        b"",
    );
}

#[test]
fn stop_mid_multichunk_write_preserves_partial_mode_0644_residue_and_retry_refuses() {
    let bytes = (0..(3 * 4096 + 73))
        .map(|index| ((index * 37 + 11) % 251) as u8)
        .collect::<Vec<_>>();
    assert_nonexact_mode_0644_residue(
        FixtureRetainedBootFilePublicationFault::MidMultiChunkWrite,
        &bytes,
        &bytes[..4096],
    );
}

#[test]
fn stop_after_final_write_preserves_exact_mode_0644_residue_then_same_inode_resume() {
    let fixture = PublicationFixture::new("forge-boot-leaf-exact-residue-");
    let bytes = b"complete bytes already in the sole public mode\n";
    arm_retained_boot_file_publication_fault(
        FixtureRetainedBootFilePublicationFault::AfterFinalWriteBeforeSourceValidation,
    );

    let first = fixture.publish_generated(bytes);

    assert!(matches!(first, Err(RetainedBootFilePublicationError::InjectedFault { .. })));
    assert!(!fixture.canonical().exists());
    let private = fixture.private_path();
    let residue = fs::metadata(&private).unwrap();
    assert_eq!(residue.permissions().mode() & 0o7777, 0o644);
    assert_eq!(residue.nlink(), 1);
    assert_eq!(fs::read(&private).unwrap(), bytes);

    let resumed = fixture.publish_generated(bytes).unwrap();

    assert_eq!(resumed.outcome(), RetainedBootFilePublicationOutcome::Published);
    assert_eq!(resumed.file_inode(), residue.ino());
    assert!(!private.exists());
    assert_eq!(fs::read(fixture.canonical()).unwrap(), bytes);
    assert_eq!(fs::metadata(fixture.canonical()).unwrap().permissions().mode() & 0o7777, 0o644);
}

#[test]
fn generated_source_publishes_once_and_exact_destination_is_idempotent() {
    let fixture = PublicationFixture::new("forge-boot-leaf-generated-");
    let bytes = b"generated immutable kernel payload\n";

    let published = fixture.publish_generated(bytes).unwrap();
    assert_eq!(published.outcome(), RetainedBootFilePublicationOutcome::Published);
    assert_eq!(published.length(), bytes.len() as u64);
    assert_eq!(published.xxh3(), xxh3_128(bytes));
    let expected_sha256: [u8; 32] = Sha256::digest(bytes).into();
    assert_eq!(published.sha256(), expected_sha256);
    assert_eq!(fs::read(fixture.canonical()).unwrap(), bytes);
    assert_eq!(fs::metadata(fixture.canonical()).unwrap().permissions().mode() & 0o7777, 0o644);
    let inode = published.file_inode();

    let idempotent = fixture.publish_generated(bytes).unwrap();
    assert_eq!(idempotent.outcome(), RetainedBootFilePublicationOutcome::AlreadyExact);
    assert_eq!(idempotent.file_inode(), inode);
    assert_eq!(idempotent.destination_device(), published.destination_device());
    assert_eq!(idempotent.destination_inode(), published.destination_inode());
    assert_eq!(idempotent.destination_mount_id(), published.destination_mount_id());
    assert_eq!(idempotent.file_device(), published.file_device());
    assert!(fixture.temporary.path().is_dir());
}

#[test]
fn sealed_source_streams_multiple_chunks_without_exposing_its_descriptor() {
    let fixture = PublicationFixture::new("forge-boot-leaf-sealed-");
    let bytes = (0..(3 * 4096 + 73))
        .map(|index| ((index * 29 + 17) % 251) as u8)
        .collect::<Vec<_>>();
    let source = sealed_memfd(&bytes);
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();

    let published = view
        .publish_immutable_boot_file_until(
            request(&bytes, Sha256::digest(&bytes).into()),
            RetainedBootNamespaceExpectedSource::sealed_descriptor(source.as_fd()),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();

    assert_eq!(published.outcome(), RetainedBootFilePublicationOutcome::Published);
    assert_eq!(fs::read(fixture.canonical()).unwrap(), bytes);
}

#[test]
fn different_canonical_destination_is_preserved_and_refused() {
    let fixture = PublicationFixture::new("forge-boot-leaf-different-");
    fs::write(fixture.canonical(), b"foreign canonical bytes").unwrap();
    let before = fs::read(fixture.canonical()).unwrap();

    let error = fixture.publish_generated(b"expected bytes").unwrap_err();

    assert!(matches!(error, RetainedBootFilePublicationError::DifferentCanonicalDestination));
    assert_eq!(fs::read(fixture.canonical()).unwrap(), before);
    assert_eq!(fs::read_dir(&fixture.destination).unwrap().count(), 1);
}

#[test]
fn exact_private_residue_is_resumed_without_replacement_or_deletion() {
    let fixture = PublicationFixture::new("forge-boot-leaf-resume-");
    let bytes = b"exact crash residue\n";
    arm_retained_boot_file_publication_fault(FixtureRetainedBootFilePublicationFault::BeforePrivateSync);
    assert!(matches!(
        fixture.publish_generated(bytes),
        Err(RetainedBootFilePublicationError::InjectedFault { .. })
    ));
    let private = fixture.private_path();
    let private_inode = fs::metadata(&private).unwrap().ino();
    assert!(!fixture.canonical().exists());

    let resumed = fixture.publish_generated(bytes).unwrap();

    assert_eq!(resumed.outcome(), RetainedBootFilePublicationOutcome::Published);
    assert_eq!(resumed.file_inode(), private_inode);
    assert!(!private.exists());
    assert_eq!(fs::read(fixture.canonical()).unwrap(), bytes);
}

#[test]
fn different_and_foreign_private_residue_are_preserved_and_refused() {
    let fixture = PublicationFixture::new("forge-boot-leaf-private-foreign-");
    let bytes = b"expected private bytes\n";
    arm_retained_boot_file_publication_fault(FixtureRetainedBootFilePublicationFault::BeforePrivateSync);
    assert!(fixture.publish_generated(bytes).is_err());
    let private = fixture.private_path();
    fs::write(&private, b"different private bytes").unwrap();
    let different = fixture.publish_generated(bytes).unwrap_err();
    assert!(matches!(different, RetainedBootFilePublicationError::DifferentPrivateResidue));
    assert_eq!(fs::read(&private).unwrap(), b"different private bytes");
    assert!(!fixture.canonical().exists());

    fs::remove_file(&private).unwrap();
    fs::create_dir(&private).unwrap();
    assert!(fixture.publish_generated(bytes).is_err());
    assert!(private.is_dir());
    assert!(!fixture.canonical().exists());
}

#[test]
fn exact_bytes_with_wrong_effective_mode_are_not_adopted() {
    let fixture = PublicationFixture::new("forge-boot-leaf-mode-");
    let bytes = b"mode-bound payload\n";
    fs::write(fixture.canonical(), bytes).unwrap();
    fs::set_permissions(fixture.canonical(), fs::Permissions::from_mode(0o600)).unwrap();

    let error = fixture.publish_generated(bytes).unwrap_err();

    assert!(matches!(
        error,
        RetainedBootFilePublicationError::ContentIdentityMismatch {
            field: "destination metadata or effective mode"
        }
    ));
    assert_eq!(fs::metadata(fixture.canonical()).unwrap().permissions().mode() & 0o7777, 0o600);
}

#[test]
fn source_sha256_mismatch_fails_before_private_publication() {
    let fixture = PublicationFixture::new("forge-boot-leaf-sha-");
    let bytes = b"sha-bound source\n";

    let error = fixture.publish_generated_with_sha(bytes, [0x5a; 32]).unwrap_err();

    assert!(matches!(
        error,
        RetainedBootFilePublicationError::ContentIdentityMismatch { field: "source SHA-256" }
    ));
    assert!(!fixture.canonical().exists());
    assert!(fixture.private_path().is_file());
}

#[test]
fn error_reported_after_single_move_is_reconciled_as_published() {
    let fixture = PublicationFixture::new("forge-boot-leaf-rename-reconcile-");
    let bytes = b"rename reconciliation payload\n";
    arm_retained_boot_file_publication_fault(
        FixtureRetainedBootFilePublicationFault::RenameReportsErrorAfterApplied,
    );

    let published = fixture.publish_generated(bytes).unwrap();

    assert_eq!(published.outcome(), RetainedBootFilePublicationOutcome::Published);
    assert_eq!(fs::read(fixture.canonical()).unwrap(), bytes);
    assert_eq!(fs::read_dir(&fixture.destination).unwrap().count(), 1);
}

#[test]
fn durability_suffix_failures_leave_an_exact_idempotent_canonical_leaf() {
    let bytes = b"durability suffix payload\n";
    for fault in [
        FixtureRetainedBootFilePublicationFault::BeforeCanonicalSync,
        FixtureRetainedBootFilePublicationFault::BeforeParentSync,
        FixtureRetainedBootFilePublicationFault::BeforeFilesystemSync,
    ] {
        let fixture = PublicationFixture::new("forge-boot-leaf-durability-");
        arm_retained_boot_file_publication_fault(fault);
        assert!(matches!(
            fixture.publish_generated(bytes),
            Err(RetainedBootFilePublicationError::InjectedFault { .. })
        ));
        assert_eq!(fs::read(fixture.canonical()).unwrap(), bytes);

        let completed = fixture.publish_generated(bytes).unwrap();
        assert_eq!(completed.outcome(), RetainedBootFilePublicationOutcome::AlreadyExact);
        assert_eq!(fs::read_dir(&fixture.destination).unwrap().count(), 1);
    }
}

#[test]
fn retained_attachment_replacement_fails_before_mutating_either_directory() {
    let fixture = PublicationFixture::new("forge-boot-leaf-attachment-");
    let view = fixture.attachment.revalidate_against(&fixture.anchor).unwrap();
    let displaced = fixture.temporary.path().join("displaced-destination");
    fs::rename(&fixture.destination, &displaced).unwrap();
    fs::create_dir(&fixture.destination).unwrap();
    let bytes = b"must not publish after attachment replacement\n";

    let error = view
        .publish_immutable_boot_file_until(
            request(bytes, Sha256::digest(bytes).into()),
            RetainedBootNamespaceExpectedSource::generated(bytes),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap_err();

    assert!(matches!(error, RetainedBootFilePublicationError::Attachment { .. }));
    assert_eq!(fs::read_dir(&displaced).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&fixture.destination).unwrap().count(), 0);
}

#[test]
fn same_credential_private_name_substitution_fails_without_validated_evidence() {
    let fixture = PublicationFixture::new("forge-boot-leaf-private-substitution-");
    let bytes = b"authenticated private inode\n";
    let foreign = b"foreign same-credential replacement\n".to_vec();
    let destination = fixture.destination.clone();
    let displaced = fixture.destination.join("displaced-authenticated-private");
    let callback_displaced = displaced.clone();
    let callback_foreign = foreign.clone();
    arm_retained_boot_file_private_name_substitution(move || {
        let private = fs::read_dir(&destination)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".cast-payload-") && name.ends_with(".stage"))
            })
            .expect("authenticated private stage must exist at the substitution boundary");
        fs::rename(&private, &callback_displaced).unwrap();
        fs::write(&private, &callback_foreign).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o644)).unwrap();
    });

    let error = fixture.publish_generated(bytes).unwrap_err();

    assert!(matches!(error, RetainedBootFilePublicationError::RenameAmbiguous));
    assert_eq!(fs::read(&displaced).unwrap(), bytes);
    assert_eq!(fs::read(fixture.canonical()).unwrap(), foreign);
    assert_eq!(fs::read_dir(&fixture.destination).unwrap().count(), 2);
}
