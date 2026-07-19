use crate::{
    client::{
        active_reblit_boot_topology_intent::BoundActiveReblitBootPartitionSelector,
        active_reblit_mounted_boot_topology::{
            ActiveReblitMountedBootTopology, ActiveReblitMountedBootTopologyObservation,
            MountedBootDestinationIdentity, MountedBootTargetObservation, ObservationPhase,
        },
    },
    linux_fs::sysfs_block::parse_sysfs_partition_identity,
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
        "MAJOR={major}\nMINOR={minor}\nDEVTYPE=partition\nPARTN={partition_number}\nPARTUUID={partuuid}\nDISKSEQ=77\n"
    );
    let identity = parse_sysfs_partition_identity(dev.as_bytes(), partition.as_bytes(), uevent.as_bytes()).unwrap();
    let raw_device = u64::try_from(nix::libc::makedev(major, minor)).unwrap();
    MountedBootTargetObservation::new(
        BoundActiveReblitBootPartitionSelector {
            partuuid,
            mount_point_hint: selector,
        },
        MountedBootDestinationIdentity::from_stat_device_and_inode(raw_device, inode),
        mount_id,
        identity.device(),
        identity.partition_number(),
        identity.partition_uuid(),
        identity.disk_sequence(),
    )
}

pub(super) fn alias_topology() -> ActiveReblitMountedBootTopology {
    ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Terminal,
        ActiveReblitMountedBootTopologyObservation::BootAliasesEsp {
            esp: target(ESP, "/synthetic/esp", 8, 1, 101, 11, 1),
        },
    )
    .unwrap()
}

pub(super) fn distinct_topology() -> ActiveReblitMountedBootTopology {
    ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Terminal,
        ActiveReblitMountedBootTopologyObservation::DistinctXbootldr {
            esp: target(ESP, "/synthetic/esp", 8, 1, 101, 11, 1),
            xbootldr: target(XBOOTLDR, "/synthetic/boot", 8, 2, 202, 12, 2),
            same_revalidated_block_parent_snapshot: true,
        },
    )
    .unwrap()
}
