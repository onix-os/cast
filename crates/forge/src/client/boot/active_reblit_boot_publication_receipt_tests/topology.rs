use super::*;
use crate::{
    client::{
        active_reblit_boot_topology_intent::BoundActiveReblitBootPartitionSelector,
        active_reblit_mounted_boot_topology::{
            ActiveReblitMountedBootTopology, ActiveReblitMountedBootTopologyObservation,
            MountedBootDestinationIdentity, MountedBootTargetObservation, ObservationPhase,
            validated_boot_filesystem_evidence_fixture,
        },
    },
    linux_fs::{
        mountinfo_boot_policy::validated_boot_mount_policy_fixture,
        sysfs_block::parse_sysfs_partition_identity,
    },
};

const ESP: &str = "11111111-2222-3333-4444-555555555555";
const XBOOTLDR: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

#[allow(clippy::too_many_arguments)]
fn target(
    partuuid: &'static str,
    selector: &'static str,
    major: u32,
    minor: u32,
    inode: u64,
    mount_id: u64,
    partition_number: u32,
) -> MountedBootTargetObservation<'static> {
    let dev = format!("{major}:{minor}\n");
    let partition = format!("{partition_number}\n");
    let uevent = format!(
        "MAJOR={major}\nMINOR={minor}\nDEVNAME=synthetic-diskp{partition_number}\nDEVTYPE=partition\nPARTN={partition_number}\nPARTUUID={partuuid}\nDISKSEQ=77\n"
    );
    let identity =
        parse_sysfs_partition_identity(dev.as_bytes(), partition.as_bytes(), uevent.as_bytes()).unwrap();
    let raw_device = u64::try_from(nix::libc::makedev(major, minor)).unwrap();
    MountedBootTargetObservation::new(
        BoundActiveReblitBootPartitionSelector {
            partuuid,
            mount_point_hint: selector,
        },
        MountedBootDestinationIdentity::from_stat_device_and_inode(raw_device, inode),
        validated_boot_filesystem_evidence_fixture(raw_device, inode),
        mount_id,
        validated_boot_mount_policy_fixture(),
        identity.device(),
        identity.partition_number(),
        identity.partition_uuid(),
        identity.disk_sequence(),
    )
}

#[test]
fn distinct_topology_maps_stable_partition_identity_and_historical_witnesses() {
    let topology = ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Terminal,
        ActiveReblitMountedBootTopologyObservation::DistinctXbootldr {
            esp: target(ESP, "/synthetic/esp", 8, 1, 101, 11, 1),
            xbootldr: target(XBOOTLDR, "/synthetic/boot", 8, 2, 202, 12, 2),
            same_revalidated_block_parent_snapshot: true,
        },
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut now = Instant::now;
    let destinations = map_destinations(
        ActiveReblitBootDestinationLayout::DistinctXbootldr,
        topology.bound(),
        deadline,
        &mut now,
    )
    .unwrap();

    let esp = destinations.esp();
    let xbootldr = destinations.xbootldr().unwrap();
    assert!(!destinations.aliases_esp());
    assert_eq!(esp.partuuid(), ESP);
    assert_eq!(esp.partition_number(), 1);
    assert_eq!(xbootldr.partuuid(), XBOOTLDR);
    assert_eq!(xbootldr.partition_number(), 2);
    assert_eq!(esp.historical_runtime_witness().destination_inode(), 101);
    assert_eq!(esp.historical_runtime_witness().mount_id(), 11);
    assert_eq!(esp.historical_runtime_witness().partition_device_major(), 8);
    assert_eq!(esp.historical_runtime_witness().partition_device_minor(), 1);
    assert_eq!(esp.historical_runtime_witness().disk_sequence(), Some(77));
    assert_eq!(xbootldr.historical_runtime_witness().destination_inode(), 202);
    assert_eq!(xbootldr.historical_runtime_witness().mount_id(), 12);
}

#[test]
fn topology_shape_must_match_the_desired_destination_layout() {
    let topology = ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Terminal,
        ActiveReblitMountedBootTopologyObservation::BootAliasesEsp {
            esp: target(ESP, "/synthetic/esp", 8, 1, 101, 11, 1),
        },
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut now = Instant::now;
    assert!(matches!(
        map_destinations(
            ActiveReblitBootDestinationLayout::DistinctXbootldr,
            topology.bound(),
            deadline,
            &mut now,
        ),
        Err(ActiveReblitBootPublicationReceiptError::TopologyLayoutMismatch)
    ));
}
