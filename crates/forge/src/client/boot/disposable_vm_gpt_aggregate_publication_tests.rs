use std::{
    env, fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};

use super::{Client, Scope};
use crate::{
    Installation, db, repository,
    boot_publication::{
        BootPublicationOutputProvenanceClaim, BootPublicationReceiptFingerprint,
        BootPublicationSha256,
    },
    client::{
        active_reblit_bls_renderer::RenderedActiveReblitBlsRequests,
        active_reblit_boot_inputs::PreparedActiveReblitStoneBootInputs,
        active_reblit_boot_publication_receipt::BorrowedActiveReblitBootPublicationProvenanceClaim,
        active_reblit_boot_render_inputs::PreparedActiveReblitBootRenderInputs,
        active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
        active_reblit_mounted_boot_topology::{
            BoundActiveReblitMountedBootTarget, BoundActiveReblitMountedBootTopology,
            PreparedActiveReblitMountedBootTopology,
        },
        active_reblit_publication_plan::{
            ACTIVE_REBLIT_BOOT_OUTPUT_MODE, ActiveReblitBootDestinationLayout,
            ActiveReblitBootDestinationRoot, ActiveReblitBootPublicationPhase,
            ActiveReblitBootPublicationRole,
        },
    },
    db::state::BootPublicationReceiptStageOutcome,
    linux_fs::mount_namespace::{
        RetainedBootFilePublicationOutcome, ValidatedRetainedBootFilePublication,
    },
    state::{self, TransitionId},
    transition_journal::{
        BootId, CodecError, MountNamespaceIdentity, Operation, Phase, Previous,
        PreviousOrigin, QuarantineName, RuntimeEpoch, RuntimeTreeIdentity,
        TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
        TreeToken,
    },
};

#[path = "active_reblit_boot_render_inputs_tests/support.rs"]
#[allow(dead_code)]
mod render_support;

const TEST_NAME: &str = "client::disposable_vm_gpt_aggregate_publication_tests::disposable_vm_receipt_bound_aggregate_publication";
const CONFIRMATION: &str = "disposable-vm-gpt-receipt-bound-aggregate-only";
const CAMPAIGN_PROFILE: &str = "gpt-receipt-bound-aggregate-v1";
const RUNTIME_ROOT: &str = "/run/cast-vm-boot-storage";
const MOUNT_ROOT: &str = "/run/cast-vm-boot-storage/mount";
const CONSUMED_MARKER: &str = "/run/cast-vm-boot-storage/authorization-v1.consumed";
const TOPOLOGY_SOURCE: &str = "etc/cast/boot-topology.glu";

#[derive(Clone, Debug)]
struct ExpectedTarget {
    path: PathBuf,
    device: u64,
    mount_id: u64,
}

fn required(name: &str) -> String {
    let value = env::var(name).unwrap_or_else(|_| panic!("{TEST_NAME}: missing {name}"));
    assert!(!value.is_empty() && !value.contains(['\n', '\r']));
    value
}

fn read_line(path: &Path) -> String {
    let bytes = fs::read(path).unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    let value = text.strip_suffix('\n').unwrap_or(text);
    assert!(!value.is_empty() && !value.contains(['\n', '\r']));
    value.to_owned()
}

fn marker_value<'a>(lines: &'a [&str], key: &str) -> &'a str {
    let prefix = format!("{key}=");
    let mut values = lines.iter().filter_map(|line| line.strip_prefix(&prefix));
    let value = values.next().unwrap_or_else(|| panic!("marker omits {key}"));
    assert!(values.next().is_none(), "marker duplicates {key}");
    value
}

fn assert_lower_hex(value: &str, lengths: &[usize]) {
    assert!(lengths.contains(&value.len()));
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

fn assert_partuuid(value: &str) {
    assert_eq!(value.len(), 36);
    assert_eq!(&value[8..9], "-");
    assert_eq!(&value[13..14], "-");
    assert_eq!(&value[18..19], "-");
    assert_eq!(&value[23..24], "-");
    assert!(value.bytes().all(|byte| {
        byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || byte == b'-'
    }));
}

fn parse_devnum(value: &str) -> (u32, u32) {
    let (major, minor) = value.split_once(':').unwrap();
    assert!(!major.is_empty() && !minor.is_empty());
    (major.parse().unwrap(), minor.parse().unwrap())
}

fn assert_marker_bound_vm_identity() -> String {
    assert!(nix::unistd::geteuid().is_root());
    assert!(Path::new("/sys/firmware/efi").is_dir());

    let hostname = required("CAST_VM_BOOT_PUBLICATION_EXPECTED_HOSTNAME");
    let machine_id = required("CAST_VM_BOOT_PUBLICATION_EXPECTED_MACHINE_ID");
    let boot_id = required("CAST_VM_BOOT_PUBLICATION_EXPECTED_BOOT_ID");
    let virtualization = required("CAST_VM_BOOT_PUBLICATION_EXPECTED_VIRTUALIZATION");
    let ssh_sha256 = required("CAST_VM_BOOT_PUBLICATION_EXPECTED_SSH_SHA256");
    let commit = required("CAST_VM_BOOT_PUBLICATION_EXPECTED_COMMIT");
    let target_disk = required("CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_DISK");
    let target_stable_path = required("CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_STABLE_PATH");
    let target_diskseq = required("CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_DISKSEQ");
    let target_bytes = required("CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_BYTES");
    assert_lower_hex(&ssh_sha256, &[64]);
    assert_lower_hex(&commit, &[40, 64]);
    assert_eq!(read_line(Path::new("/proc/sys/kernel/hostname")), hostname);
    assert_eq!(read_line(Path::new("/etc/machine-id")), machine_id);
    assert_eq!(read_line(Path::new("/proc/sys/kernel/random/boot_id")), boot_id);
    let ssh_connection = required("SSH_CONNECTION");
    assert_eq!(hex::encode(Sha256::digest(ssh_connection.as_bytes())), ssh_sha256);

    assert_eq!(required("CAST_VM_BOOT_PUBLICATION_CONSUMED_MARKER"), CONSUMED_MARKER);
    let marker = Path::new(CONSUMED_MARKER);
    let metadata = fs::symlink_metadata(marker).unwrap();
    assert!(metadata.file_type().is_file());
    assert_eq!(metadata.uid(), 0);
    assert_eq!(metadata.gid(), 0);
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    assert!(metadata.len() > 0 && metadata.len() <= 4096);
    let bytes = fs::read(marker).unwrap();
    assert!(bytes.ends_with(b"\n") && !bytes.contains(&b'\r'));
    let text = std::str::from_utf8(&bytes).unwrap();
    let lines = text.lines().collect::<Vec<_>>();

    assert_eq!(marker_value(&lines, "protocol"), "3");
    assert_eq!(marker_value(&lines, "campaign_profile"), CAMPAIGN_PROFILE);
    for (key, expected) in [
        ("hostname", hostname.as_str()),
        ("machine_id", machine_id.as_str()),
        ("boot_id", boot_id.as_str()),
        ("virtualization", virtualization.as_str()),
        ("ssh_connection_sha256", ssh_sha256.as_str()),
        ("commit", commit.as_str()),
        ("target_disk", target_disk.as_str()),
        ("target_stable_path", target_stable_path.as_str()),
        ("target_diskseq", target_diskseq.as_str()),
        ("target_bytes", target_bytes.as_str()),
    ] {
        assert_eq!(marker_value(&lines, key), expected);
    }
    assert_eq!(marker_value(&lines, "remote_confirmation"), "disposable-vm-remote-only");
    assert_eq!(
        marker_value(&lines, "cooperative_root_confirmation"),
        "cooperative-guest-root-no-hotplug"
    );
    let challenge = marker_value(&lines, "challenge");
    assert_lower_hex(challenge, &[64]);
    challenge.to_owned()
}

fn assert_directory(path: &Path, mode: u32) -> PathBuf {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.file_type().is_dir());
    assert_eq!(metadata.uid(), 0);
    assert_eq!(metadata.gid(), 0);
    assert_eq!(metadata.permissions().mode() & 0o7777, mode);
    let canonical = fs::canonicalize(path).unwrap();
    assert_eq!(canonical, path);
    canonical
}

fn prepare_fixture(kind: &str, challenge: &str) -> render_support::RenderFixture {
    let expected_boot_id = required("CAST_VM_BOOT_PUBLICATION_EXPECTED_BOOT_ID");
    let build_root = PathBuf::from(required("CAST_VM_BOOT_PUBLICATION_BUILD_ROOT"));
    assert_eq!(
        build_root,
        Path::new("/var/tmp").join(format!(
            "cast-vm-boot-storage-{expected_boot_id}-{challenge}"
        ))
    );
    assert_directory(&build_root, 0o700);

    let fixture_parent = PathBuf::from(required("CAST_VM_GPT_AGGREGATE_FIXTURE_PARENT"));
    assert_eq!(
        fixture_parent,
        build_root.join("gpt-aggregate-fixtures").join(kind)
    );
    assert!(!fixture_parent.starts_with(MOUNT_ROOT));
    assert!(fixture_parent.starts_with(&build_root));
    assert_directory(&fixture_parent, 0o700);

    let source_installation = PathBuf::from(required("CAST_VM_GPT_TOPOLOGY_INSTALLATION"));
    assert_eq!(source_installation, build_root.join("topology-installation"));
    assert!(!source_installation.starts_with(MOUNT_ROOT));
    assert_directory(&source_installation, 0o755);
    let admitted_source = source_installation.join(TOPOLOGY_SOURCE);
    let source_metadata = fs::symlink_metadata(&admitted_source).unwrap();
    assert!(source_metadata.file_type().is_file());
    assert_eq!(source_metadata.uid(), 0);
    assert_eq!(source_metadata.gid(), 0);
    assert_eq!(source_metadata.permissions().mode() & 0o7777, 0o644);
    assert_eq!(source_metadata.nlink(), 1);
    assert!(source_metadata.len() > 0 && source_metadata.len() <= 64 * 1024);

    let fixture = render_support::RenderFixture::new_in(
        &fixture_parent,
        render_support::StateSpec::one_kernel("6.12"),
        Vec::new(),
    );
    assert_eq!(fixture.installation.root.parent(), Some(fixture_parent.as_path()));
    assert!(fixture.installation.root.starts_with(&fixture_parent));
    assert!(!fixture.installation.root.starts_with(MOUNT_ROOT));
    assert_eq!(
        fs::canonicalize(&fixture.installation.root).unwrap(),
        fixture.installation.root
    );
    let fixture_source = fixture.installation.root.join(TOPOLOGY_SOURCE);
    assert!(!fixture_source.exists());
    fs::copy(&admitted_source, &fixture_source).unwrap();
    fs::set_permissions(&fixture_source, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(fs::read(&fixture_source).unwrap(), fs::read(&admitted_source).unwrap());
    fixture
}

fn staging_client(fixture: &render_support::RenderFixture) -> Client {
    let repositories = repository::Manager::with_explicit(
        "disposable-vm-gpt-aggregate-publication",
        repository::Map::default(),
        fixture.installation.clone(),
    )
    .unwrap();
    Client {
        registry: super::build_repository_registry(&repositories),
        install_db: db::meta::Database::new(":memory:").unwrap(),
        state_db: fixture.state_db.clone(),
        layout_db: fixture.layout_db.clone(),
        config: None,
        repositories,
        scope: Scope::Stateful,
        installation: fixture.installation.clone(),
    }
}

fn preparing_record() -> TransitionRecord {
    TransitionRecord::preparing(
        TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        RuntimeEpoch {
            boot_id: BootId::parse("01234567-89ab-4cde-8f01-23456789abcd").unwrap(),
            mount_namespace: MountNamespaceIdentity {
                st_dev: 30,
                inode: 31,
            },
        },
        Operation::ActiveReblit,
        Some(42),
        TreeToken::parse("a".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
        RuntimeTreeIdentity {
            st_dev: 10,
            inode: 11,
            mount_id: 12,
        },
        Previous {
            id: Some(42),
            tree_token: TreeToken::parse("b".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
            usr_runtime_identity: RuntimeTreeIdentity {
                st_dev: 10,
                inode: 13,
                mount_id: 12,
            },
            origin: PreviousOrigin::ActiveReblitCorrupt,
        },
        true,
        true,
        QuarantineName::parse("failed-0123456789abcdef").unwrap(),
    )
    .unwrap()
}

fn exact_system_triggers_complete_journal(
    installation: &Installation,
) -> (
    TransitionJournalStore,
    TransitionRecord,
    TransitionJournalRecordBinding,
) {
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let journal = TransitionJournalStore::open_in_retained_cast(cast, &installation.root).unwrap();
    let mut predecessor = preparing_record();
    journal.create(&predecessor).unwrap();
    loop {
        match predecessor.forward_successor(None) {
            Ok(successor) => {
                journal.advance(&predecessor, &successor).unwrap();
                predecessor = successor;
            }
            Err(CodecError::ExplicitBootSyncStartedSuccessorRequired) => break,
            Err(error) => panic!("construct exact pre-boot record: {error}"),
        }
    }
    assert_eq!(predecessor.operation, Operation::ActiveReblit);
    assert_eq!(predecessor.phase, Phase::SystemTriggersComplete);
    let binding = journal.record_binding(cast, &predecessor).unwrap();
    (journal, predecessor, binding)
}

fn claim_bindings<'inventory>(
    inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
    phase: &str,
) -> Vec<BorrowedActiveReblitBootPublicationProvenanceClaim<'inventory>> {
    let claim = match phase {
        "publish" => BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        "revalidate" => BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption,
        _ => unreachable!(),
    };
    inventory
        .outputs()
        .iter()
        .map(|output| {
            BorrowedActiveReblitBootPublicationProvenanceClaim::new(
                output.root(),
                output.relative_path(),
                BootPublicationSha256::from_bytes(*output.content_identity().as_bytes()),
                claim,
            )
        })
        .collect()
}

fn expected_target(
    target: BoundActiveReblitMountedBootTarget<'_>,
    path: PathBuf,
    devnum: (u32, u32),
    partuuid: &str,
) -> ExpectedTarget {
    assert_eq!((target.device_major(), target.device_minor()), devnum);
    assert_eq!(target.partuuid, partuuid);
    assert_eq!(target.selector, path.to_str().unwrap());
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(metadata.dev(), target.destination.raw_device());
    assert_eq!(metadata.ino(), target.destination.inode());
    ExpectedTarget {
        path,
        device: target.destination.raw_device(),
        mount_id: target.mount_id,
    }
}

fn topology_targets(
    kind: &str,
    topology: BoundActiveReblitMountedBootTopology<'_>,
) -> (ExpectedTarget, ExpectedTarget) {
    let esp_path = PathBuf::from(required("CAST_VM_GPT_TOPOLOGY_ESP_MOUNT"));
    assert_eq!(esp_path, Path::new(MOUNT_ROOT).join("esp"));
    let esp_devnum = parse_devnum(&required("CAST_VM_GPT_TOPOLOGY_ESP_DEVNUM"));
    let esp_partuuid = required("CAST_VM_GPT_TOPOLOGY_ESP_PARTUUID");
    assert_partuuid(&esp_partuuid);
    match (kind, topology) {
        ("alias", BoundActiveReblitMountedBootTopology::BootAliasesEsp { esp }) => {
            let esp = expected_target(esp, esp_path, esp_devnum, &esp_partuuid);
            (esp.clone(), esp)
        }
        (
            "distinct",
            BoundActiveReblitMountedBootTopology::DistinctXbootldr { esp, xbootldr },
        ) => {
            let esp = expected_target(esp, esp_path, esp_devnum, &esp_partuuid);
            let xbootldr_path = PathBuf::from(required("CAST_VM_GPT_TOPOLOGY_XBOOTLDR_MOUNT"));
            assert_eq!(xbootldr_path, Path::new(MOUNT_ROOT).join("xbootldr"));
            let xbootldr_devnum = parse_devnum(&required("CAST_VM_GPT_TOPOLOGY_XBOOTLDR_DEVNUM"));
            let xbootldr_partuuid = required("CAST_VM_GPT_TOPOLOGY_XBOOTLDR_PARTUUID");
            assert_partuuid(&xbootldr_partuuid);
            let xbootldr = expected_target(
                xbootldr,
                xbootldr_path,
                xbootldr_devnum,
                &xbootldr_partuuid,
            );
            assert_ne!(esp.device, xbootldr.device);
            (esp, xbootldr)
        }
        _ => panic!("production topology shape differs from the exact campaign kind"),
    }
}

fn expected_paths(state: state::Id) -> Vec<PathBuf> {
    let kernel = b"render kernel 6.12";
    let payload = format!(
        "EFI/head/xxh3-{:032x}-l{:016x}/vmlinuz",
        xxhash_rust::xxh3::xxh3_128(kernel),
        kernel.len()
    );
    vec![
        PathBuf::from(payload),
        PathBuf::from(format!("loader/entries/head-6.12-{}.conf", i32::from(state))),
        PathBuf::from("loader/loader.conf"),
        PathBuf::from("EFI/Boot/BOOTX64.EFI"),
        PathBuf::from("EFI/systemd/systemd-bootx64.efi"),
    ]
}

fn assert_output(
    evidence: &ValidatedRetainedBootFilePublication,
    output: &super::active_reblit_bls_renderer::BoundActiveReblitBlsPublication<'_, '_>,
    target: &ExpectedTarget,
    expected_outcome: RetainedBootFilePublicationOutcome,
) {
    assert_eq!(evidence.outcome(), expected_outcome);
    assert_eq!(evidence.destination_device(), target.device);
    assert_eq!(evidence.destination_mount_id(), target.mount_id);
    assert_eq!(evidence.file_device(), target.device);
    assert_eq!(evidence.length(), output.expected_length());
    assert_eq!(evidence.xxh3(), output.expected_digest());
    assert_eq!(evidence.sha256(), *output.expected_content_identity().as_bytes());

    let path = target.path.join(output.relative_path());
    let parent_metadata = fs::metadata(path.parent().unwrap()).unwrap();
    assert!(parent_metadata.file_type().is_dir());
    assert_eq!(parent_metadata.dev(), target.device);
    assert_eq!(parent_metadata.ino(), evidence.destination_inode());
    let bytes = fs::read(&path).unwrap();
    let metadata = fs::metadata(&path).unwrap();
    assert!(metadata.file_type().is_file());
    assert_eq!(metadata.permissions().mode() & 0o7777, ACTIVE_REBLIT_BOOT_OUTPUT_MODE);
    assert_eq!(metadata.nlink(), 1);
    assert_eq!(metadata.dev(), target.device);
    assert_eq!(metadata.ino(), evidence.file_inode());
    assert_eq!(u64::try_from(bytes.len()).unwrap(), evidence.length());
    assert_eq!(xxhash_rust::xxh3::xxh3_128(&bytes), evidence.xxh3());
    assert_eq!(<[u8; 32]>::from(Sha256::digest(&bytes)), evidence.sha256());
    assert!(fs::read_dir(path.parent().unwrap()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with(".cast-payload-")
    }));
}

fn assert_pending_boot_sync_started(
    database: &db::state::Database,
    installation: &Installation,
    expected_record: &TransitionRecord,
    fingerprint: BootPublicationReceiptFingerprint,
) {
    let state = database.boot_publication_receipt_state().unwrap();
    assert_eq!(state.pending().unwrap().fingerprint(), fingerprint);
    assert!(state.head().committed().is_none());
    assert_eq!(expected_record.operation, Operation::ActiveReblit);
    assert_eq!(expected_record.phase, Phase::BootSyncStarted);
    let pair = expected_record
        .boot_publication_receipt_correlation()
        .unwrap()
        .unwrap();
    assert_eq!(pair.committed, None);
    assert_eq!(pair.pending, fingerprint);
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let journal = TransitionJournalStore::open_in_retained_cast(cast, &installation.root).unwrap();
    assert_eq!(
        journal.load_revalidated_retained_cast(cast).unwrap(),
        Some(expected_record.clone()),
    );
}

#[test]
#[ignore = "requires the guarded disposable-VM receipt-bound GPT aggregate campaign"]
fn disposable_vm_receipt_bound_aggregate_publication() {
    assert_eq!(required("CAST_VM_GPT_TOPOLOGY_CONFIRMATION"), CONFIRMATION);
    let challenge = assert_marker_bound_vm_identity();
    let kind = required("CAST_VM_GPT_TOPOLOGY_KIND");
    assert!(matches!(kind.as_str(), "alias" | "distinct"));
    let phase = required("CAST_VM_GPT_TOPOLOGY_PHASE");
    assert!(matches!(phase.as_str(), "publish" | "revalidate"));
    let expected_outcome = if phase == "publish" {
        RetainedBootFilePublicationOutcome::Published
    } else {
        RetainedBootFilePublicationOutcome::AlreadyExact
    };

    let fixture = prepare_fixture(&kind, &challenge);
    let client = staging_client(&fixture);
    let deadline = render_support::future_deadline();
    let stone = fixture.stone();
    let roots = fixture.roots(&stone);
    let prepared = render_support::prepare_static(&fixture, &stone, &roots);
    let local_policy = fixture.local_policy();
    let root_intent = fixture.root_intent();
    let inputs = prepared
        .revalidate_until(
            &fixture.state_db,
            &fixture.layout_db,
            &client.installation,
            &local_policy,
            &root_intent,
            deadline,
        )
        .unwrap();
    let topology_prepared =
        PreparedActiveReblitMountedBootTopology::prepare_until(&client.installation, deadline).unwrap();
    let topology = topology_prepared
        .revalidate_until(&client.installation, deadline)
        .unwrap();
    let (esp_target, boot_target) = topology_targets(&kind, topology.topology());
    let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
    let plan = rendered.into_publication_plan(&topology).unwrap();
    assert_eq!(plan.publication_count(), 5);
    assert_eq!(
        plan.destination_layout(),
        if kind == "alias" {
            ActiveReblitBootDestinationLayout::BootAliasesEsp
        } else {
            ActiveReblitBootDestinationLayout::DistinctXbootldr
        }
    );

    let paths = expected_paths(fixture.head.id);
    let roles = [
        ActiveReblitBootPublicationRole::Payload,
        ActiveReblitBootPublicationRole::Entry,
        ActiveReblitBootPublicationRole::LoaderControl,
        ActiveReblitBootPublicationRole::FallbackBootloader,
        ActiveReblitBootPublicationRole::SystemdBootloader,
    ];
    let phases = [
        ActiveReblitBootPublicationPhase::Payload,
        ActiveReblitBootPublicationPhase::Entry,
        ActiveReblitBootPublicationPhase::LoaderControl,
        ActiveReblitBootPublicationPhase::Bootloader,
        ActiveReblitBootPublicationPhase::Bootloader,
    ];
    let roots = [
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootDestinationRoot::Esp,
        ActiveReblitBootDestinationRoot::Esp,
    ];
    for (index, output) in plan.outputs().enumerate() {
        assert_eq!(output.relative_path(), paths[index]);
        assert_eq!(output.role(), roles[index]);
        assert_eq!(output.phase(), phases[index]);
        assert_eq!(output.root(), roots[index]);
        assert_eq!(output.mode(), ACTIVE_REBLIT_BOOT_OUTPUT_MODE);
    }

    let inventory = plan.prepare_desired_publication_inventory().unwrap();
    assert_eq!(inventory.outputs().len(), 5);
    let claims = claim_bindings(&inventory, &phase);
    let (journal, predecessor, binding) =
        exact_system_triggers_complete_journal(&client.installation);
    let staged = client
        .stage_active_reblit_boot_sync(
            &plan,
            &inventory,
            &claims,
            journal,
            predecessor,
            binding,
        )
        .unwrap();
    assert_eq!(staged.database_outcome(), BootPublicationReceiptStageOutcome::Staged);
    assert_eq!(staged.record().phase, Phase::BootSyncStarted);
    let expected_record = staged.record().clone();
    let fingerprint = staged.receipt_fingerprint();

    let result = staged.attempt_immutable_boot_publication(&client).unwrap();
    assert_eq!(result.receipt_fingerprint(), fingerprint);
    assert_eq!(result.publication_count(), 5);
    assert_eq!(result.evidence().len(), 5);
    if phase == "publish" {
        assert_eq!(result.published_count(), 5);
        assert_eq!(result.already_exact_count(), 0);
    } else {
        assert_eq!(result.published_count(), 0);
        assert_eq!(result.already_exact_count(), 5);
    }
    for (index, (evidence, output)) in result.evidence().iter().zip(plan.outputs()).enumerate() {
        let target = match (kind.as_str(), output.root()) {
            ("alias", _) => &esp_target,
            ("distinct", ActiveReblitBootDestinationRoot::Boot) => &boot_target,
            ("distinct", ActiveReblitBootDestinationRoot::Esp) => &esp_target,
            _ => unreachable!(),
        };
        assert_eq!(output.relative_path(), paths[index]);
        assert_output(evidence, &output, target, expected_outcome);
        if kind == "distinct" {
            let other = if output.root() == ActiveReblitBootDestinationRoot::Boot {
                &esp_target
            } else {
                &boot_target
            };
            assert!(!other.path.join(output.relative_path()).exists());
        }
    }
    drop(result);

    // Each phase intentionally builds a fresh deterministic journal and receipt.
    // `revalidate` proves five preexisting exact outputs, not cross-process
    // continuity of the receipt created by the preceding `publish` invocation.
    assert_pending_boot_sync_started(
        &fixture.state_db,
        &client.installation,
        &expected_record,
        fingerprint,
    );
    assert!(Path::new(RUNTIME_ROOT).is_dir());
}
