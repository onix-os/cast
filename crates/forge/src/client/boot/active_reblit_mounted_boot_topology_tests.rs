use crate::{
    client::{
        active_reblit_boot_topology_intent::BoundActiveReblitBootPartitionSelector,
        active_reblit_mounted_boot_topology::{
            ActiveReblitMountedBootTopology, ActiveReblitMountedBootTopologyError,
            ActiveReblitMountedBootTopologyObservation, BootTargetRole, BoundActiveReblitMountedBootTopology,
            MountedBootDestinationIdentity, MountedBootTargetObservation, ObservationPhase,
        },
    },
    linux_fs::{
        mountinfo_boot_policy::{BootFilesystemKind, validated_boot_mount_policy_fixture},
        sysfs_block::parse_sysfs_partition_identity,
    },
};

const ESP_PARTUUID: &str = "11111111-2222-3333-4444-555555555555";
const XBOOTLDR_PARTUUID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const ALTERNATE_PARTUUID: &str = "99999999-8888-7777-6666-555555555555";
const ESP_SELECTOR: &str = "/synthetic/esp-root";
const XBOOTLDR_SELECTOR: &str = "/synthetic/boot-root";
const ALTERNATE_SELECTOR: &str = "/synthetic/alternate-root";

fn selector(partuuid: &'static str, mount_point_hint: &'static str) -> BoundActiveReblitBootPartitionSelector<'static> {
    BoundActiveReblitBootPartitionSelector {
        partuuid,
        mount_point_hint,
    }
}

#[allow(clippy::too_many_arguments)]
fn target(
    partuuid: &'static str,
    mount_point_hint: &'static str,
    major: u32,
    minor: u32,
    inode: u64,
    mount_id: u64,
    partition_number: u32,
    disk_sequence: Option<u64>,
) -> MountedBootTargetObservation<'static> {
    let dev = format!("{major}:{minor}\n");
    let partition = format!("{partition_number}\n");
    let disk_sequence_field = disk_sequence
        .map(|sequence| format!("DISKSEQ={sequence}\n"))
        .unwrap_or_default();
    let uevent = format!(
        "MAJOR={major}\nMINOR={minor}\nDEVTYPE=partition\nPARTN={partition_number}\nPARTUUID={partuuid}\n{disk_sequence_field}"
    );
    let identity = parse_sysfs_partition_identity(dev.as_bytes(), partition.as_bytes(), uevent.as_bytes())
        .expect("synthetic scalar identity must parse");
    let raw_device = u64::try_from(nix::libc::makedev(major, minor)).expect("Linux dev_t must fit the retained scalar");
    MountedBootTargetObservation::new(
        selector(partuuid, mount_point_hint),
        MountedBootDestinationIdentity::from_stat_device_and_inode(raw_device, inode),
        mount_id,
        validated_boot_mount_policy_fixture(),
        identity.device(),
        identity.partition_number(),
        identity.partition_uuid(),
        identity.disk_sequence(),
    )
}

fn esp() -> MountedBootTargetObservation<'static> {
    target(ESP_PARTUUID, ESP_SELECTOR, 8, 1, 101, 11, 1, Some(77))
}

fn xbootldr() -> MountedBootTargetObservation<'static> {
    target(XBOOTLDR_PARTUUID, XBOOTLDR_SELECTOR, 8, 2, 202, 12, 2, Some(77))
}

fn alias(esp: MountedBootTargetObservation<'static>) -> ActiveReblitMountedBootTopologyObservation<'static> {
    ActiveReblitMountedBootTopologyObservation::BootAliasesEsp { esp }
}

fn distinct(
    esp: MountedBootTargetObservation<'static>,
    xbootldr: MountedBootTargetObservation<'static>,
    same_parent: bool,
) -> ActiveReblitMountedBootTopologyObservation<'static> {
    ActiveReblitMountedBootTopologyObservation::DistinctXbootldr {
        esp,
        xbootldr,
        same_revalidated_block_parent_snapshot: same_parent,
    }
}

#[test]
fn alias_is_structurally_one_target_and_exposes_only_closed_scalar_facts() {
    let facts = ActiveReblitMountedBootTopology::from_observation(ObservationPhase::Bootstrap, alias(esp())).unwrap();
    let BoundActiveReblitMountedBootTopology::BootAliasesEsp { esp: bound_esp } = facts.bound() else {
        panic!("alias observation must retain the one-target form");
    };
    assert_eq!(bound_esp.selector, ESP_SELECTOR);
    assert_eq!(bound_esp.partuuid, ESP_PARTUUID);
    assert_eq!(bound_esp.mount_id, 11);
    assert_eq!(bound_esp.mount_policy.filesystem(), BootFilesystemKind::Vfat);
    assert!(bound_esp.mount_policy.mount_read_write());
    assert!(bound_esp.mount_policy.superblock_read_write());
    assert!(bound_esp.mount_policy.nosuid());
    assert!(bound_esp.mount_policy.nodev());
    assert!(bound_esp.mount_policy.noexec());
    assert!(bound_esp.mount_policy.nosymfollow());
    assert_eq!(bound_esp.device_major(), 8);
    assert_eq!(bound_esp.device_minor(), 1);
    assert_eq!(bound_esp.partition_number.get(), 1);
    assert_eq!(bound_esp.partition_uuid.as_str(), ESP_PARTUUID);
    assert_eq!(bound_esp.disk_sequence.map(|sequence| sequence.get()), Some(77));
    assert_eq!(bound_esp.destination, esp().destination);
}

#[test]
fn distinct_targets_accept_every_required_inequality_and_same_parent_evidence() {
    let facts = ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Bootstrap,
        distinct(esp(), xbootldr(), true),
    )
    .unwrap();
    let BoundActiveReblitMountedBootTopology::DistinctXbootldr { esp, xbootldr } = facts.bound() else {
        panic!("distinct observation must retain two separate targets");
    };
    assert_eq!(esp.selector, ESP_SELECTOR);
    assert_eq!(xbootldr.selector, XBOOTLDR_SELECTOR);
    assert_ne!(esp.destination, xbootldr.destination);
    assert_ne!(esp.mount_id, xbootldr.mount_id);
    assert_ne!(esp.device, xbootldr.device);
    assert_ne!(esp.partuuid, xbootldr.partuuid);
}

#[test]
fn target_requires_nonzero_mount_id() {
    let error = ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Pass1,
        alias(target(ESP_PARTUUID, ESP_SELECTOR, 8, 1, 101, 0, 1, Some(77))),
    )
    .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitMountedBootTopologyError::InvalidMountId {
            phase: ObservationPhase::Pass1,
            role: BootTargetRole::Esp,
        }
    );
}

#[test]
fn target_requires_nonzero_destination_device_and_inode_identity() {
    let valid = esp();
    let invalid = [
        MountedBootDestinationIdentity::from_stat_device_and_inode(0, 101),
        MountedBootDestinationIdentity::from_stat_device_and_inode(valid.destination.raw_device, 0),
    ];
    for destination in invalid {
        let mut observation = valid;
        observation.destination = destination;
        assert_eq!(
            ActiveReblitMountedBootTopology::from_observation(ObservationPhase::Pass1, alias(observation),),
            Err(ActiveReblitMountedBootTopologyError::InvalidDestinationIdentity {
                phase: ObservationPhase::Pass1,
                role: BootTargetRole::Esp,
            })
        );
    }
}

#[test]
fn target_requires_destination_stat_device_to_match_typed_device() {
    let mut observation = esp();
    let wrong = u64::try_from(nix::libc::makedev(8, 9)).unwrap();
    observation.destination = MountedBootDestinationIdentity::from_stat_device_and_inode(wrong, 101);
    let error =
        ActiveReblitMountedBootTopology::from_observation(ObservationPhase::Pass2, alias(observation)).unwrap_err();
    assert_eq!(
        error,
        ActiveReblitMountedBootTopologyError::DestinationDeviceMismatch {
            phase: ObservationPhase::Pass2,
            role: BootTargetRole::Esp,
        }
    );
}

#[test]
fn target_requires_declarative_and_authenticated_partuuid_equality() {
    let mut observation = target(ALTERNATE_PARTUUID, ESP_SELECTOR, 8, 1, 101, 11, 1, Some(77));
    observation.intent = selector(ESP_PARTUUID, ESP_SELECTOR);
    let error =
        ActiveReblitMountedBootTopology::from_observation(ObservationPhase::Terminal, alias(observation)).unwrap_err();
    assert_eq!(
        error,
        ActiveReblitMountedBootTopologyError::PartitionUuidMismatch {
            phase: ObservationPhase::Terminal,
            role: BootTargetRole::Esp,
        }
    );
}

#[test]
fn distinct_rejects_equal_selectors() {
    let error = ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Pass1,
        distinct(
            esp(),
            target(XBOOTLDR_PARTUUID, ESP_SELECTOR, 8, 2, 202, 12, 2, Some(77)),
            true,
        ),
    )
    .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitMountedBootTopologyError::DistinctSelectorAlias {
            phase: ObservationPhase::Pass1,
        }
    );
}

#[test]
fn distinct_rejects_equal_destination_inode_identities() {
    let mut second = target(XBOOTLDR_PARTUUID, XBOOTLDR_SELECTOR, 8, 1, 101, 12, 2, Some(77));
    second.destination = esp().destination;
    let error =
        ActiveReblitMountedBootTopology::from_observation(ObservationPhase::Pass1, distinct(esp(), second, true))
            .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitMountedBootTopologyError::DistinctAttachmentAlias {
            phase: ObservationPhase::Pass1,
        }
    );
}

#[test]
fn distinct_rejects_equal_mount_ids() {
    let error = ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Pass1,
        distinct(
            esp(),
            target(XBOOTLDR_PARTUUID, XBOOTLDR_SELECTOR, 8, 2, 202, 11, 2, Some(77)),
            true,
        ),
    )
    .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitMountedBootTopologyError::DistinctMountIdAlias {
            phase: ObservationPhase::Pass1,
        }
    );
}

#[test]
fn distinct_rejects_equal_typed_devices_even_for_different_inodes() {
    let error = ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Pass1,
        distinct(
            esp(),
            target(XBOOTLDR_PARTUUID, XBOOTLDR_SELECTOR, 8, 1, 202, 12, 2, Some(77)),
            true,
        ),
    )
    .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitMountedBootTopologyError::DistinctDeviceAlias {
            phase: ObservationPhase::Pass1,
        }
    );
}

#[test]
fn distinct_rejects_equal_partuuids_independently_of_other_facts() {
    let error = ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Pass1,
        distinct(
            esp(),
            target(ESP_PARTUUID, XBOOTLDR_SELECTOR, 8, 2, 202, 12, 2, Some(77)),
            true,
        ),
    )
    .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitMountedBootTopologyError::DistinctPartuuidAlias {
            phase: ObservationPhase::Pass1,
        }
    );
}

#[test]
fn distinct_requires_explicit_same_revalidated_parent_evidence() {
    let error =
        ActiveReblitMountedBootTopology::from_observation(ObservationPhase::Pass2, distinct(esp(), xbootldr(), false))
            .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitMountedBootTopologyError::BlockParentMismatch {
            phase: ObservationPhase::Pass2,
        }
    );
}

#[test]
fn distinct_same_parent_evidence_rejects_inconsistent_disk_sequences() {
    let error = ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Pass2,
        distinct(
            esp(),
            target(XBOOTLDR_PARTUUID, XBOOTLDR_SELECTOR, 8, 2, 202, 12, 2, Some(78)),
            true,
        ),
    )
    .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitMountedBootTopologyError::BlockParentMismatch {
            phase: ObservationPhase::Pass2,
        }
    );
}

#[test]
fn every_later_phase_accepts_only_exact_alias_facts() {
    let facts = ActiveReblitMountedBootTopology::from_observation(ObservationPhase::Bootstrap, alias(esp())).unwrap();
    for phase in [
        ObservationPhase::Pass1,
        ObservationPhase::Pass2,
        ObservationPhase::Terminal,
    ] {
        facts
            .require_exact_observation(ObservationPhase::Bootstrap, phase, alias(esp()))
            .unwrap();
    }
}

#[test]
fn exact_pass_comparison_covers_every_retained_target_fact() {
    let facts = ActiveReblitMountedBootTopology::from_observation(ObservationPhase::Bootstrap, alias(esp())).unwrap();
    let changed = [
        target(ESP_PARTUUID, ALTERNATE_SELECTOR, 8, 1, 101, 11, 1, Some(77)),
        target(ALTERNATE_PARTUUID, ESP_SELECTOR, 8, 1, 101, 11, 1, Some(77)),
        target(ESP_PARTUUID, ESP_SELECTOR, 8, 1, 102, 11, 1, Some(77)),
        target(ESP_PARTUUID, ESP_SELECTOR, 8, 1, 101, 13, 1, Some(77)),
        target(ESP_PARTUUID, ESP_SELECTOR, 8, 3, 101, 11, 1, Some(77)),
        target(ESP_PARTUUID, ESP_SELECTOR, 8, 1, 101, 11, 3, Some(77)),
        target(ESP_PARTUUID, ESP_SELECTOR, 8, 1, 101, 11, 1, Some(78)),
        target(ESP_PARTUUID, ESP_SELECTOR, 8, 1, 101, 11, 1, None),
    ];
    for observation in changed {
        assert_eq!(
            facts.require_exact_observation(ObservationPhase::Bootstrap, ObservationPhase::Pass1, alias(observation),),
            Err(ActiveReblitMountedBootTopologyError::PassFactsChanged {
                expected_phase: ObservationPhase::Bootstrap,
                observed_phase: ObservationPhase::Pass1,
            })
        );
    }
}

#[test]
fn exact_pass_comparison_rejects_structural_alias_to_distinct_change() {
    let facts = ActiveReblitMountedBootTopology::from_observation(ObservationPhase::Bootstrap, alias(esp())).unwrap();
    assert_eq!(
        facts.require_exact_observation(
            ObservationPhase::Bootstrap,
            ObservationPhase::Terminal,
            distinct(esp(), xbootldr(), true),
        ),
        Err(ActiveReblitMountedBootTopologyError::PassFactsChanged {
            expected_phase: ObservationPhase::Bootstrap,
            observed_phase: ObservationPhase::Terminal,
        })
    );
}

#[test]
fn exact_pass_comparison_accepts_the_complete_distinct_fact_set() {
    let facts = ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Bootstrap,
        distinct(esp(), xbootldr(), true),
    )
    .unwrap();
    facts
        .require_exact_observation(
            ObservationPhase::Bootstrap,
            ObservationPhase::Terminal,
            distinct(esp(), xbootldr(), true),
        )
        .unwrap();
}
