use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use sha2::{Digest as _, Sha256};
use xxhash_rust::xxh3::xxh3_128;

use super::*;
use crate::{
    boot_publication::{
        BootPublicationDestination, BootPublicationDestinations,
        BootPublicationHistoricalRuntimeWitness,
        BootPublicationOutputProvenanceClaim, BootPublicationOutputRole,
        BootPublicationPublicationPhase, BootPublicationSha256,
        BootPublicationReceiptBody, BootPublicationXxh3,
        CanonicalBootPublicationReceipt, prepare_boot_publication_receipt,
    },
    client::{
        active_reblit_mounted_boot_topology::{
            AliasFixture, arm_fixture_owned_cleanup_targets,
            fixture_owned_cleanup_targets_remaining,
        },
        startup_gate::ActiveReblitBootSyncStartedCleanupSeal,
    },
    db::state::{
        BootPublicationReceiptPromotionOutcome,
        BootPublicationReceiptStageOutcome,
        CurrentExactPromotedBootPublicationReceiptChain, Database,
    },
    linux_fs::{
        descriptor_boot_namespace::RetainedBootNamespaceExpectedSource,
        mount_namespace::{
            PreparedMountNamespaceAnchor, PreparedTaskRootedAttachment,
            RetainedBootFileStaleCleanupRequest,
            arm_stale_boot_file_stop_after_detach,
        },
    },
    state::TransitionId,
};

const ESP_PARTUUID: &str = "5e85a94f-b115-41c5-9d72-9d23958b5edc";
const REPLACEMENT_PATH: &str = "EFI/Linux/restart-replacement.efi";
const STALE_PATH: &str = "EFI/Linux/restart-stale.efi";
const PREDECESSOR_BYTES: &[u8] = b"restart predecessor payload\n";
const INSTALLED_BYTES: &[u8] = b"restart installed payload\n";
const STALE_BYTES: &[u8] = b"restart stale payload\n";

struct CleanupFixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    anchor: PreparedMountNamespaceAnchor,
    attachment: PreparedTaskRootedAttachment,
}

impl CleanupFixture {
    fn new(prefix: &str) -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join("target");
        let mut target_builder = fs::DirBuilder::new();
        target_builder.recursive(true).create(&target).unwrap();
        let temporary = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(target)
            .unwrap();
        let root = temporary.path().join("boot-root");
        fs::DirBuilder::new().create(&root).unwrap();
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

fn fingerprint(byte: u8) -> BootPublicationReceiptFingerprint {
    BootPublicationReceiptFingerprint::from_bytes([byte; 32])
}

fn output(path: &str, bytes: &[u8]) -> BootPublicationOutput {
    output_with_claim(
        path,
        bytes,
        BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
    )
}

fn output_with_claim(
    path: &str,
    bytes: &[u8],
    claim: BootPublicationOutputProvenanceClaim,
) -> BootPublicationOutput {
    BootPublicationOutput::new(
        BootPublicationRoot::Boot,
        BootPublicationPublicationPhase::Payload,
        BootPublicationOutputRole::Payload,
        path,
        0o644,
        BootPublicationXxh3::from_u128(xxh3_128(bytes)),
        u64::try_from(bytes.len()).unwrap(),
        BootPublicationSha256::from_bytes(Sha256::digest(bytes).into()),
        claim,
    )
}

fn transition(digit: char) -> TransitionId {
    TransitionId::parse(digit.to_string().repeat(TransitionId::TEXT_LENGTH))
        .unwrap()
}

fn alias_destinations(runtime_seed: u64) -> BootPublicationDestinations {
    BootPublicationDestinations::boot_aliases_esp(
        BootPublicationDestination::new(
            ESP_PARTUUID,
            1,
            BootPublicationHistoricalRuntimeWitness::new(
                nix::libc::makedev(8, 1),
                100 + runtime_seed,
                10 + runtime_seed,
                8,
                1,
                Some(1_000 + runtime_seed),
            ),
        ),
    )
}

fn receipt(
    digit: char,
    predecessor: Option<BootPublicationReceiptFingerprint>,
    salt: u8,
    runtime_seed: u64,
    outputs: Vec<BootPublicationOutput>,
) -> CanonicalBootPublicationReceipt {
    prepare_boot_publication_receipt(
        BootPublicationReceiptBody::new(
            transition(digit),
            predecessor,
            BootPublicationSha256::from_bytes([salt; 32]),
            BootPublicationSha256::from_bytes([salt.wrapping_add(1); 32]),
            alias_destinations(runtime_seed),
            outputs,
        )
        .unwrap(),
    )
    .unwrap()
}

fn promote_receipt(
    database: &Database,
    receipt: &CanonicalBootPublicationReceipt,
) {
    assert_eq!(
        database.stage_boot_publication_receipt(receipt).unwrap(),
        BootPublicationReceiptStageOutcome::Staged,
    );
    assert_eq!(
        database
            .promote_boot_publication_receipt(receipt, deadline())
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
}

fn retain_parent<'view, 'prepared>(
    view: &'view crate::linux_fs::mount_namespace::RevalidatedTaskRootedAttachment<'prepared>,
) -> crate::linux_fs::mount_namespace::RetainedBootPublicationParent<'view, 'prepared> {
    view.retain_boot_publication_parent_until(&["EFI", "Linux"], deadline())
        .unwrap()
}

#[test]
fn receipt_replacement_reconstructs_fresh_authority_then_is_already_clean() {
    let fixture = CleanupFixture::new("forge-restart-receipt-replacement-");
    let view = fixture
        .attachment
        .revalidate_against(&fixture.anchor)
        .unwrap();
    let identity = OwnedCleanupTargetIdentity::from_attachment(&view);
    let predecessor = output(REPLACEMENT_PATH, PREDECESSOR_BYTES);
    let installed = output(REPLACEMENT_PATH, INSTALLED_BYTES);
    let owner = receipt_owner(fingerprint(0x41));
    let parent = retain_parent(&view);
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
                owner,
            ),
            &RetainedBootNamespaceExpectedSource::generated(INSTALLED_BYTES),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let sidecar = fixture
        .root
        .join("EFI/Linux")
        .join(applied.sidecar_leaf());
    assert_eq!(fs::read(&sidecar).unwrap(), PREDECESSOR_BYTES);
    drop(applied);
    drop(parent);

    let path = split_cleanup_path(REPLACEMENT_PATH, "restart replacement", 0)
        .unwrap();
    assert_eq!(
        reconcile_restart_replacement_with_attachment(
            &view,
            identity,
            deadline(),
            0,
            &path,
            &predecessor,
            &installed,
            owner,
        )
        .unwrap(),
        ActiveReblitBootOwnedCleanupOutcome::RemovedReplacementRollback,
    );
    assert!(!sidecar.exists());
    assert_eq!(
        fs::read(fixture.root.join(REPLACEMENT_PATH)).unwrap(),
        INSTALLED_BYTES,
    );
    assert_eq!(
        reconcile_restart_replacement_with_attachment(
            &view,
            identity,
            deadline(),
            0,
            &path,
            &predecessor,
            &installed,
            owner,
        )
        .unwrap(),
        ActiveReblitBootOwnedCleanupOutcome::AlreadyClean,
    );
}

#[test]
fn receipt_stale_cleanup_reconciles_canonical_detached_and_already_clean() {
    let fixture = CleanupFixture::new("forge-restart-receipt-stale-");
    let view = fixture
        .attachment
        .revalidate_against(&fixture.anchor)
        .unwrap();
    let identity = OwnedCleanupTargetIdentity::from_attachment(&view);
    let stale = output(STALE_PATH, STALE_BYTES);
    let owner = receipt_owner(fingerprint(0x51));
    let path = split_cleanup_path(STALE_PATH, "restart stale", 0).unwrap();

    let parent = retain_parent(&view);
    parent
        .publish_immutable_boot_file_until(
            output_request("restart-stale.efi", &stale),
            &RetainedBootNamespaceExpectedSource::generated(STALE_BYTES),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    drop(parent);
    assert_eq!(
        reconcile_restart_stale_with_attachment(
            &view,
            identity,
            deadline(),
            0,
            &path,
            &stale,
            owner,
        )
        .unwrap(),
        ActiveReblitBootOwnedCleanupOutcome::RemovedOwnedStale,
    );
    assert_eq!(
        reconcile_restart_stale_with_attachment(
            &view,
            identity,
            deadline(),
            0,
            &path,
            &stale,
            owner,
        )
        .unwrap(),
        ActiveReblitBootOwnedCleanupOutcome::AlreadyClean,
    );

    let parent = retain_parent(&view);
    parent
        .publish_immutable_boot_file_until(
            output_request("restart-stale.efi", &stale),
            &RetainedBootNamespaceExpectedSource::generated(STALE_BYTES),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let request = RetainedBootFileStaleCleanupRequest::new(
        output_request("restart-stale.efi", &stale),
        owner,
    );
    let authority = parent
        .authenticate_stale_boot_file_cleanup_until(
            request,
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let detached = fixture
        .root
        .join("EFI/Linux")
        .join(authority.private_leaf());
    arm_stale_boot_file_stop_after_detach();
    assert!(
        parent
            .cleanup_authenticated_stale_boot_file_until(authority, deadline())
            .is_err()
    );
    drop(parent);
    assert!(!fixture.root.join(STALE_PATH).exists());
    assert!(detached.exists());

    assert_eq!(
        reconcile_restart_stale_with_attachment(
            &view,
            identity,
            deadline(),
            0,
            &path,
            &stale,
            owner,
        )
        .unwrap(),
        ActiveReblitBootOwnedCleanupOutcome::RemovedOwnedStale,
    );
    assert!(!detached.exists());
    assert_eq!(
        reconcile_restart_stale_with_attachment(
            &view,
            identity,
            deadline(),
            0,
            &path,
            &stale,
            owner,
        )
        .unwrap(),
        ActiveReblitBootOwnedCleanupOutcome::AlreadyClean,
    );
}

#[test]
fn exact_receipt_plan_live_alias_targets_and_startup_seal_drive_restart_cleanup() {
    let predecessor = receipt(
        '1',
        None,
        0x71,
        71,
        vec![
            output_with_claim(
                REPLACEMENT_PATH,
                PREDECESSOR_BYTES,
                BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
            ),
            output_with_claim(
                STALE_PATH,
                STALE_BYTES,
                BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
            ),
        ],
    );
    let installed = receipt(
        '2',
        Some(predecessor.fingerprint()),
        0x72,
        172,
        vec![output_with_claim(
            REPLACEMENT_PATH,
            INSTALLED_BYTES,
            BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
        )],
    );
    let database = Database::new(":memory:").unwrap();
    promote_receipt(&database, &predecessor);
    promote_receipt(&database, &installed);
    let CurrentExactPromotedBootPublicationReceiptChain::Installed(chain) =
        database
            .load_current_exact_promoted_boot_publication_receipt_chain()
            .unwrap()
    else {
        panic!("the promoted receipt fixture must load an installed chain")
    };
    let plan = chain
        .prepare_active_reblit_promoted_boot_cleanup_plan()
        .unwrap();
    let replacement_index = plan
        .entries()
        .iter()
        .position(|entry| {
            entry.predecessor_output().relative_path() == REPLACEMENT_PATH
        })
        .unwrap();
    let stale_index = plan
        .entries()
        .iter()
        .position(|entry| {
            entry.predecessor_output().relative_path() == STALE_PATH
        })
        .unwrap();
    assert!(matches!(
        plan.entries()[replacement_index].disposition(),
        ActiveReblitPromotedBootCleanupDisposition::ReplaceOwned,
    ));
    assert!(matches!(
        plan.entries()[stale_index].disposition(),
        ActiveReblitPromotedBootCleanupDisposition::DeleteOwnedStale,
    ));

    let fixture = AliasFixture::stable().unwrap();
    fs::set_permissions(
        fixture.publication_root(),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let operation_deadline = deadline();
    let prepared = fixture.prepare_until(operation_deadline).unwrap();
    let topology = prepared
        .revalidate_until(fixture.installation(), operation_deadline)
        .unwrap();
    let validated = topology
        .revalidate_promoted_receipt_targets(&chain)
        .unwrap();

    let replacement_entry = &plan.entries()[replacement_index];
    let stale_entry = &plan.entries()[stale_index];
    let replacement_installed = replacement_entry.installed_output().unwrap();
    let owner = receipt_owner(plan.promoted_receipt());
    let real_anchor = PreparedMountNamespaceAnchor::prepare().unwrap();
    let real_attachment = real_anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment(
            fixture.publication_root().to_str().unwrap(),
        )
        .unwrap();
    let real_view = real_attachment
        .revalidate_against(&real_anchor)
        .unwrap();
    let parent = retain_parent(&real_view);
    parent
        .publish_immutable_boot_file_until(
            output_request(
                "restart-replacement.efi",
                replacement_entry.predecessor_output(),
            ),
            &RetainedBootNamespaceExpectedSource::generated(
                PREDECESSOR_BYTES,
            ),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let applied = parent
        .replace_exact_boot_file_until(
            replacement_request(
                "restart-replacement.efi",
                replacement_entry.predecessor_output(),
                replacement_installed,
                owner,
            ),
            &RetainedBootNamespaceExpectedSource::generated(INSTALLED_BYTES),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    let sidecar = fixture
        .publication_root()
        .join("EFI/Linux")
        .join(applied.sidecar_leaf());
    parent
        .publish_immutable_boot_file_until(
            output_request(
                "restart-stale.efi",
                stale_entry.predecessor_output(),
            ),
            &RetainedBootNamespaceExpectedSource::generated(STALE_BYTES),
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    drop(applied);
    drop(parent);
    assert!(sidecar.exists());
    assert!(fixture.publication_root().join(STALE_PATH).exists());

    let _cleanup_targets = arm_fixture_owned_cleanup_targets(
        fixture.publication_root().to_path_buf(),
        4,
    );
    let seal = ActiveReblitBootSyncStartedCleanupSeal::new_for_test(
        plan.promoted_receipt(),
    );
    assert_eq!(
        validated
            .reconcile_and_cleanup_restart_receipt_entry(
                &plan,
                replacement_index,
                &seal,
            )
            .unwrap(),
        ActiveReblitBootOwnedCleanupOutcome::RemovedReplacementRollback,
    );
    assert_eq!(
        validated
            .reconcile_and_cleanup_restart_receipt_entry(
                &plan,
                stale_index,
                &seal,
            )
            .unwrap(),
        ActiveReblitBootOwnedCleanupOutcome::RemovedOwnedStale,
    );
    for entry_index in [replacement_index, stale_index] {
        assert_eq!(
            validated
                .reconcile_and_cleanup_restart_receipt_entry(
                    &plan,
                    entry_index,
                    &seal,
                )
                .unwrap(),
            ActiveReblitBootOwnedCleanupOutcome::AlreadyClean,
        );
    }
    assert!(!sidecar.exists());
    assert_eq!(
        fs::read(fixture.publication_root().join(REPLACEMENT_PATH)).unwrap(),
        INSTALLED_BYTES,
    );
    assert!(!fixture.publication_root().join(STALE_PATH).exists());
    assert_eq!(fixture_owned_cleanup_targets_remaining(), 0);
}

#[test]
fn restart_admission_refuses_receipt_drift_noop_and_unowned_preserve() {
    let plan_receipt = fingerprint(0x61);
    require_restart_receipt_join(plan_receipt, plan_receipt, plan_receipt)
        .unwrap();
    assert!(matches!(
        require_restart_receipt_join(
            fingerprint(0x62),
            plan_receipt,
            plan_receipt,
        ),
        Err(ActiveReblitBootOwnedCleanupError::RestartTargetReceiptMismatch)
    ));
    assert!(matches!(
        require_restart_receipt_join(
            plan_receipt,
            fingerprint(0x63),
            plan_receipt,
        ),
        Err(ActiveReblitBootOwnedCleanupError::RestartSealReceiptMismatch)
    ));

    for disposition in [
        ActiveReblitPromotedBootCleanupDisposition::NoOp,
        ActiveReblitPromotedBootCleanupDisposition::PreserveUnownedStale,
    ] {
        assert!(matches!(
            require_mutating_disposition(&disposition, 7),
            Err(
                ActiveReblitBootOwnedCleanupError::RestartDispositionRefused {
                    entry_index: 7,
                }
            )
        ));
    }
    require_mutating_disposition(
        &ActiveReblitPromotedBootCleanupDisposition::ReplaceOwned,
        8,
    )
    .unwrap();
    require_mutating_disposition(
        &ActiveReblitPromotedBootCleanupDisposition::DeleteOwnedStale,
        9,
    )
    .unwrap();
}

#[path = "../../../../active_reblit_owned_cleanup_component_process_kill_tests.rs"]
mod component_process_kill;
