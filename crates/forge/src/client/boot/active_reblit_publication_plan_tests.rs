use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::{
    client::{
        active_reblit_boot_topology_intent::BoundActiveReblitBootPartitionSelector,
        active_reblit_mounted_boot_topology::{
            ActiveReblitMountedBootTopology, ActiveReblitMountedBootTopologyObservation,
            MountedBootDestinationIdentity, MountedBootTargetObservation, ObservationPhase,
        },
    },
    linux_fs::{
        mountinfo_boot_policy::validated_boot_mount_policy_fixture, sysfs_block::parse_sysfs_partition_identity,
    },
};

use super::*;

const TEST_ESP_PARTUUID: &str = "11111111-2222-3333-4444-555555555555";
const TEST_XBOOTLDR_PARTUUID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const TEST_ALTERNATE_ESP_PARTUUID: &str = "99999999-8888-7777-6666-555555555555";
const TEST_ALTERNATE_XBOOTLDR_PARTUUID: &str = "bbbbbbbb-cccc-dddd-eeee-ffffffffffff";

#[allow(clippy::too_many_arguments)]
fn topology_target(
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
    let identity = parse_sysfs_partition_identity(dev.as_bytes(), partition.as_bytes(), uevent.as_bytes())
        .expect("synthetic publication topology must parse");
    let raw_device = u64::try_from(nix::libc::makedev(major, minor)).expect("Linux dev_t must fit u64");
    MountedBootTargetObservation::new(
        BoundActiveReblitBootPartitionSelector {
            partuuid,
            mount_point_hint: selector,
        },
        MountedBootDestinationIdentity::from_stat_device_and_inode(raw_device, inode),
        mount_id,
        validated_boot_mount_policy_fixture(),
        identity.device(),
        identity.partition_number(),
        identity.partition_uuid(),
        identity.disk_sequence(),
    )
}

fn alias_topology() -> ActiveReblitMountedBootTopology {
    ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Terminal,
        ActiveReblitMountedBootTopologyObservation::BootAliasesEsp {
            esp: topology_target(TEST_ESP_PARTUUID, "/synthetic/esp", 8, 1, 101, 11, 1),
        },
    )
    .expect("synthetic alias topology must validate")
}

fn distinct_topology() -> ActiveReblitMountedBootTopology {
    ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Terminal,
        ActiveReblitMountedBootTopologyObservation::DistinctXbootldr {
            esp: topology_target(TEST_ESP_PARTUUID, "/synthetic/esp", 8, 1, 101, 11, 1),
            xbootldr: topology_target(TEST_XBOOTLDR_PARTUUID, "/synthetic/boot", 8, 2, 202, 12, 2),
            same_revalidated_block_parent_snapshot: true,
        },
    )
    .expect("synthetic distinct topology must validate")
}

fn alternate_distinct_topology() -> ActiveReblitMountedBootTopology {
    ActiveReblitMountedBootTopology::from_observation(
        ObservationPhase::Terminal,
        ActiveReblitMountedBootTopologyObservation::DistinctXbootldr {
            esp: topology_target(TEST_ALTERNATE_ESP_PARTUUID, "/synthetic/other-esp", 9, 1, 303, 21, 1),
            xbootldr: topology_target(
                TEST_ALTERNATE_XBOOTLDR_PARTUUID,
                "/synthetic/other-boot",
                9,
                2,
                404,
                22,
                2,
            ),
            same_revalidated_block_parent_snapshot: true,
        },
    )
    .expect("synthetic alternate distinct topology must validate")
}

fn payload(
    path: impl Into<PathBuf>,
    binding_index: u16,
    digest: u128,
    length: u64,
) -> ActiveReblitBootPublicationRequest {
    ActiveReblitBootPublicationRequest::sealed_payload(path.into(), binding_index, digest, length)
}

fn entry(path: impl Into<PathBuf>, bytes: &[u8]) -> ActiveReblitBootPublicationRequest {
    ActiveReblitBootPublicationRequest::generated_entry(path.into(), bytes.into())
}

fn loader_control(bytes: &[u8]) -> ActiveReblitBootPublicationRequest {
    ActiveReblitBootPublicationRequest::generated_loader_control(bytes.into())
}

fn fallback_bootloader(binding_index: u16, digest: u128, length: u64) -> ActiveReblitBootPublicationRequest {
    ActiveReblitBootPublicationRequest::sealed_fallback_bootloader(binding_index, digest, length)
}

fn systemd_bootloader(binding_index: u16, digest: u128, length: u64) -> ActiveReblitBootPublicationRequest {
    ActiveReblitBootPublicationRequest::sealed_systemd_bootloader(binding_index, digest, length)
}

fn sealed_source(binding_index: u16, digest: u128, length: u64) -> ActiveReblitBootPublicationRequestSource {
    ActiveReblitBootPublicationRequestSource::SealedSnapshot {
        binding_index,
        digest,
        length,
    }
}

fn generated_source(bytes: &[u8]) -> ActiveReblitBootPublicationRequestSource {
    ActiveReblitBootPublicationRequestSource::Generated { bytes: bytes.into() }
}

fn prepare_alias(
    requests: impl IntoIterator<Item = ActiveReblitBootPublicationRequest>,
) -> Result<PreparedActiveReblitBootPublicationPlan, ActiveReblitBootPublicationPlanError> {
    let topology = alias_topology();
    PreparedActiveReblitBootPublicationPlan::prepare_until(
        requests,
        topology.bound(),
        Instant::now() + Duration::from_secs(5),
    )
}

fn prepare_alias_until(
    requests: impl IntoIterator<Item = ActiveReblitBootPublicationRequest>,
    deadline: Instant,
) -> Result<PreparedActiveReblitBootPublicationPlan, ActiveReblitBootPublicationPlanError> {
    let topology = alias_topology();
    PreparedActiveReblitBootPublicationPlan::prepare_until(requests, topology.bound(), deadline)
}

fn prepare_distinct(
    requests: impl IntoIterator<Item = ActiveReblitBootPublicationRequest>,
) -> Result<PreparedActiveReblitBootPublicationPlan, ActiveReblitBootPublicationPlanError> {
    let topology = distinct_topology();
    PreparedActiveReblitBootPublicationPlan::prepare_until(
        requests,
        topology.bound(),
        Instant::now() + Duration::from_secs(5),
    )
}

fn prepare_with_policy(
    requests: impl IntoIterator<Item = ActiveReblitBootPublicationRequest>,
    policy: PublicationPlanPolicy,
) -> Result<PreparedActiveReblitBootPublicationPlan, ActiveReblitBootPublicationPlanError> {
    prepare_publication_plan_until(
        requests,
        policy,
        ActiveReblitBootDestinationCollisionDomains::boot_aliases_esp(),
        Instant::now() + Duration::from_secs(5),
    )
}

include!("active_reblit_publication_plan_tests/roles_and_collisions.rs");
include!("active_reblit_publication_plan_tests/path_policy.rs");
include!("active_reblit_publication_plan_tests/bounds_and_deadlines.rs");
