use std::{
    env,
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use sha2::{Digest as _, Sha256};
use xxhash_rust::xxh3::xxh3_128;

use super::active_reblit_mounted_boot_topology::{
    BoundActiveReblitMountedBootTarget, BoundActiveReblitMountedBootTopology,
    PreparedActiveReblitMountedBootTopology,
};
use crate::{
    Installation,
    linux_fs::{
        descriptor_boot_namespace::RetainedBootNamespaceExpectedSource,
        mount_namespace::{
            PreparedMountNamespaceAnchor, RetainedBootFilePublicationLimits,
            RetainedBootFilePublicationOutcome, RetainedBootFilePublicationRequest,
        },
    },
};

const CONFIRMATION: &str = "disposable-vm-gpt-topology-only";
const RUNTIME_ROOT: &str = "/run/cast-vm-boot-storage";
const MOUNT_ROOT: &str = "/run/cast-vm-boot-storage/mount";
const CONSUMED_MARKER: &str = "/run/cast-vm-boot-storage/authorization-v1.consumed";
const TEST_NAME: &str = "client::disposable_vm_gpt_topology_tests::disposable_vm_authenticates_gpt_boot_topology_and_publishes_real_leaves";
const ALIAS_LEAF: &str = "cast-vm-gpt-alias.efi";
const DISTINCT_ESP_LEAF: &str = "cast-vm-gpt-distinct-esp.efi";
const DISTINCT_XBOOTLDR_LEAF: &str = "cast-vm-gpt-distinct-xbootldr.conf";
const ALIAS_PAYLOAD: &[u8] = b"cast disposable VM GPT ESP-as-BOOT publication\n";
const DISTINCT_ESP_PAYLOAD: &[u8] = b"cast disposable VM distinct ESP publication\n";
const DISTINCT_XBOOTLDR_PAYLOAD: &[u8] = b"cast disposable VM distinct XBOOTLDR publication\n";

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

fn parse_devnum(value: &str) -> (u32, u32) {
    let (major, minor) = value.split_once(':').unwrap();
    assert!(!major.is_empty() && !minor.is_empty());
    (major.parse().unwrap(), minor.parse().unwrap())
}

fn assert_partuuid(value: &str) {
    assert_eq!(value.len(), 36);
    assert_eq!(&value[8..9], "-");
    assert_eq!(&value[13..14], "-");
    assert_eq!(&value[18..19], "-");
    assert_eq!(&value[23..24], "-");
    assert!(value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || byte == b'-'));
}

fn marker_value<'a>(lines: &'a [&str], key: &str) -> &'a str {
    let prefix = format!("{key}=");
    let mut values = lines.iter().filter_map(|line| line.strip_prefix(&prefix));
    let value = values.next().unwrap_or_else(|| panic!("marker omits {key}"));
    assert!(values.next().is_none(), "marker duplicates {key}");
    value
}

fn assert_remote_identity_and_marker() {
    assert!(nix::unistd::geteuid().is_root());
    assert!(Path::new("/sys/firmware/efi").is_dir());
    assert_eq!(
        read_line(Path::new("/proc/sys/kernel/hostname")),
        required("CAST_VM_BOOT_PUBLICATION_EXPECTED_HOSTNAME")
    );
    assert_eq!(
        read_line(Path::new("/etc/machine-id")),
        required("CAST_VM_BOOT_PUBLICATION_EXPECTED_MACHINE_ID")
    );
    assert_eq!(
        read_line(Path::new("/proc/sys/kernel/random/boot_id")),
        required("CAST_VM_BOOT_PUBLICATION_EXPECTED_BOOT_ID")
    );
    let ssh_connection = required("SSH_CONNECTION");
    assert_eq!(
        hex::encode(Sha256::digest(ssh_connection.as_bytes())),
        required("CAST_VM_BOOT_PUBLICATION_EXPECTED_SSH_SHA256")
    );
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
    assert_eq!(marker_value(&lines, "protocol"), "2");
    assert_eq!(marker_value(&lines, "campaign_profile"), "gpt-boot-topologies");
    assert_eq!(marker_value(&lines, "remote_confirmation"), "disposable-vm-remote-only");
    assert_eq!(
        marker_value(&lines, "cooperative_root_confirmation"),
        "cooperative-guest-root-no-hotplug"
    );
}

fn assert_fixed_path(name: &str, expected: &str) -> PathBuf {
    let value = required(name);
    assert_eq!(value, expected);
    let path = PathBuf::from(value);
    assert!(path.is_absolute() && path.starts_with(RUNTIME_ROOT));
    assert_eq!(fs::canonicalize(&path).unwrap(), path);
    path
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn publish(
    installation: &Installation,
    topology: &PreparedActiveReblitMountedBootTopology,
    target: BoundActiveReblitMountedBootTarget<'_>,
    root: &Path,
    parent_components: &[&str],
    leaf: &str,
    payload: &'static [u8],
    expected: RetainedBootFilePublicationOutcome,
) -> u64 {
    topology.revalidate(installation).unwrap();
    let anchor = PreparedMountNamespaceAnchor::prepare().unwrap();
    let attachment = anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment(root.to_str().unwrap())
        .unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    assert_eq!(view.destination_device(), target.destination.raw_device());
    assert_eq!(view.destination_inode(), target.destination.inode());
    assert_eq!(view.destination_mount_id(), target.mount_id);
    view.authenticate_boot_filesystem_until(deadline()).unwrap();
    let parent = view
        .retain_boot_publication_parent_until(parent_components, deadline())
        .unwrap();
    assert_eq!(parent.root_device(), target.destination.raw_device());
    assert_eq!(parent.root_inode(), target.destination.inode());
    assert_eq!(parent.root_mount_id(), target.mount_id);
    let sha256: [u8; 32] = Sha256::digest(payload).into();
    let expected_source = RetainedBootNamespaceExpectedSource::generated(payload);
    let publication = parent
        .publish_immutable_boot_file_until(
            RetainedBootFilePublicationRequest::new(leaf, payload.len() as u64, xxh3_128(payload), sha256),
            &expected_source,
            RetainedBootFilePublicationLimits::default(),
            deadline(),
        )
        .unwrap();
    assert!(parent.matches_leaf_evidence(&publication));
    assert_eq!(publication.outcome(), expected);
    assert_eq!(publication.length(), payload.len() as u64);
    assert_eq!(publication.sha256(), sha256);
    let canonical = parent_components
        .iter()
        .fold(root.to_path_buf(), |path, component| path.join(component))
        .join(leaf);
    assert_eq!(fs::read(&canonical).unwrap(), payload);
    let metadata = fs::metadata(&canonical).unwrap();
    assert!(metadata.file_type().is_file());
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o644);
    assert_eq!(metadata.nlink(), 1);
    assert_eq!(metadata.ino(), publication.file_inode());
    assert!(fs::read_dir(canonical.parent().unwrap()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with(".cast-payload-")
    }));
    topology.revalidate(installation).unwrap();
    publication.file_inode()
}

#[test]
#[ignore = "requires the guarded disposable-VM GPT ESP/XBOOTLDR campaign"]
fn disposable_vm_authenticates_gpt_boot_topology_and_publishes_real_leaves() {
    assert_eq!(required("CAST_VM_GPT_TOPOLOGY_CONFIRMATION"), CONFIRMATION);
    assert_remote_identity_and_marker();
    let kind = required("CAST_VM_GPT_TOPOLOGY_KIND");
    assert!(matches!(kind.as_str(), "alias" | "distinct"));
    let phase = required("CAST_VM_GPT_TOPOLOGY_PHASE");
    assert!(matches!(phase.as_str(), "publish" | "revalidate"));
    let expected_outcome = if phase == "publish" {
        RetainedBootFilePublicationOutcome::Published
    } else {
        RetainedBootFilePublicationOutcome::AlreadyExact
    };
    let esp_mount = assert_fixed_path("CAST_VM_GPT_TOPOLOGY_ESP_MOUNT", &format!("{MOUNT_ROOT}/esp"));
    let esp_devnum = parse_devnum(&required("CAST_VM_GPT_TOPOLOGY_ESP_DEVNUM"));
    let esp_partuuid = required("CAST_VM_GPT_TOPOLOGY_ESP_PARTUUID");
    assert_partuuid(&esp_partuuid);
    let installation_root = PathBuf::from(required("CAST_VM_GPT_TOPOLOGY_INSTALLATION"));
    assert!(installation_root.starts_with("/var/tmp/cast-vm-boot-storage-"));
    assert!(!installation_root.starts_with(MOUNT_ROOT));
    assert_eq!(fs::canonicalize(&installation_root).unwrap(), installation_root);
    let installation = Installation::open(&installation_root, None).unwrap();
    let topology = PreparedActiveReblitMountedBootTopology::prepare(&installation).unwrap();
    let initial = topology.revalidate(&installation).unwrap();

    match (kind.as_str(), initial.topology()) {
        ("alias", BoundActiveReblitMountedBootTopology::BootAliasesEsp { esp }) => {
            assert_eq!((esp.device_major(), esp.device_minor()), esp_devnum);
            assert_eq!(esp.partuuid, esp_partuuid);
            assert_eq!(esp.selector, esp_mount.to_str().unwrap());
            let inode = publish(
                &installation,
                &topology,
                esp,
                &esp_mount,
                &["EFI", "Linux"],
                ALIAS_LEAF,
                ALIAS_PAYLOAD,
                expected_outcome,
            );
            if phase == "publish" {
                assert_eq!(
                    publish(
                        &installation,
                        &topology,
                        esp,
                        &esp_mount,
                        &["EFI", "Linux"],
                        ALIAS_LEAF,
                        ALIAS_PAYLOAD,
                        RetainedBootFilePublicationOutcome::AlreadyExact,
                    ),
                    inode
                );
            }
        }
        ("distinct", BoundActiveReblitMountedBootTopology::DistinctXbootldr { esp, xbootldr }) => {
            let xbootldr_mount = assert_fixed_path(
                "CAST_VM_GPT_TOPOLOGY_XBOOTLDR_MOUNT",
                &format!("{MOUNT_ROOT}/xbootldr"),
            );
            let xbootldr_devnum = parse_devnum(&required("CAST_VM_GPT_TOPOLOGY_XBOOTLDR_DEVNUM"));
            let xbootldr_partuuid = required("CAST_VM_GPT_TOPOLOGY_XBOOTLDR_PARTUUID");
            assert_partuuid(&xbootldr_partuuid);
            assert_eq!((esp.device_major(), esp.device_minor()), esp_devnum);
            assert_eq!((xbootldr.device_major(), xbootldr.device_minor()), xbootldr_devnum);
            assert_eq!(esp.partuuid, esp_partuuid);
            assert_eq!(xbootldr.partuuid, xbootldr_partuuid);
            assert_eq!(esp.selector, esp_mount.to_str().unwrap());
            assert_eq!(xbootldr.selector, xbootldr_mount.to_str().unwrap());
            assert_ne!(esp.device, xbootldr.device);
            assert_eq!(esp.disk_sequence, xbootldr.disk_sequence);
            let esp_inode = publish(
                &installation,
                &topology,
                esp,
                &esp_mount,
                &["EFI", "Linux"],
                DISTINCT_ESP_LEAF,
                DISTINCT_ESP_PAYLOAD,
                expected_outcome,
            );
            let xbootldr_inode = publish(
                &installation,
                &topology,
                xbootldr,
                &xbootldr_mount,
                &["loader", "entries"],
                DISTINCT_XBOOTLDR_LEAF,
                DISTINCT_XBOOTLDR_PAYLOAD,
                expected_outcome,
            );
            if phase == "publish" {
                assert_eq!(
                    publish(
                        &installation,
                        &topology,
                        esp,
                        &esp_mount,
                        &["EFI", "Linux"],
                        DISTINCT_ESP_LEAF,
                        DISTINCT_ESP_PAYLOAD,
                        RetainedBootFilePublicationOutcome::AlreadyExact,
                    ),
                    esp_inode
                );
                assert_eq!(
                    publish(
                        &installation,
                        &topology,
                        xbootldr,
                        &xbootldr_mount,
                        &["loader", "entries"],
                        DISTINCT_XBOOTLDR_LEAF,
                        DISTINCT_XBOOTLDR_PAYLOAD,
                        RetainedBootFilePublicationOutcome::AlreadyExact,
                    ),
                    xbootldr_inode
                );
            }
        }
        _ => panic!("production topology shape differs from the exact campaign kind"),
    }
}
