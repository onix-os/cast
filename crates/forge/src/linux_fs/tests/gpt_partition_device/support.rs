use std::{io, time::Instant};

use super::super::super::{
    gpt_partition_device::{
        BlockDeviceObservation, BlockDeviceObserver, FixtureGptPartitionDeviceLimits, ObservedDeviceAccess,
        ObservedNodeKind, ReconciledGptPartitionDeviceEvidence, reconcile_gpt_partition_device_fixture_until,
        reconcile_gpt_partition_device_fixture_with_clock_until,
    },
    gpt_partition_role::GptPartitionRole,
};

pub(super) const UUID: &str = "11111111-2222-3333-4444-555555555555";
pub(super) const TABLE_HASH: [u8; 32] = [0xa5; 32];
pub(super) const PRODUCTION_LIMITS: FixtureGptPartitionDeviceLimits = FixtureGptPartitionDeviceLimits {
    observation_calls: 2,
    work_units: 45,
};

#[derive(Clone, Copy)]
pub(super) struct ObservationFields {
    pub(super) node_kind: ObservedNodeKind,
    pub(super) access: ObservedDeviceAccess,
    pub(super) containing_device: u64,
    pub(super) inode: u64,
    pub(super) mount_id: u64,
    pub(super) block_major: u32,
    pub(super) block_minor: u32,
    pub(super) logical_block_size: u32,
    pub(super) byte_length: u64,
}

impl ObservationFields {
    pub(super) const fn standard() -> Self {
        Self {
            node_kind: ObservedNodeKind::BlockDevice,
            access: ObservedDeviceAccess::ReadOnly,
            containing_device: 41,
            inode: 52,
            mount_id: 63,
            block_major: 8,
            block_minor: 0,
            logical_block_size: 512,
            byte_length: 64 * 1024 * 1024,
        }
    }

    pub(super) const fn observation(self) -> BlockDeviceObservation {
        BlockDeviceObservation::new(
            self.node_kind,
            self.access,
            self.containing_device,
            self.inode,
            self.mount_id,
            self.block_major,
            self.block_minor,
            self.logical_block_size,
            self.byte_length,
        )
    }
}

pub(super) struct FixtureObserver {
    observations: [BlockDeviceObservation; 2],
    calls: usize,
    fail_on_call: Option<usize>,
}

impl FixtureObserver {
    pub(super) const fn stable(fields: ObservationFields) -> Self {
        let observation = fields.observation();
        Self {
            observations: [observation, observation],
            calls: 0,
            fail_on_call: None,
        }
    }

    pub(super) const fn changing(opening: ObservationFields, closing: ObservationFields) -> Self {
        Self {
            observations: [opening.observation(), closing.observation()],
            calls: 0,
            fail_on_call: None,
        }
    }

    pub(super) const fn failing(fields: ObservationFields, call: usize) -> Self {
        let observation = fields.observation();
        Self {
            observations: [observation, observation],
            calls: 0,
            fail_on_call: Some(call),
        }
    }

    pub(super) const fn calls(&self) -> usize {
        self.calls
    }
}

impl BlockDeviceObserver for FixtureObserver {
    fn observe_until(&mut self, _deadline: Instant) -> io::Result<BlockDeviceObservation> {
        let call = self.calls;
        self.calls += 1;
        if self.fail_on_call == Some(call) {
            return Err(io::Error::other("injected block-device observation failure"));
        }
        self.observations
            .get(call)
            .copied()
            .ok_or_else(|| io::Error::other("unexpected extra block-device observation"))
    }
}

#[derive(Clone, Copy)]
pub(super) struct FixtureInput {
    pub(super) parent_major: u32,
    pub(super) parent_minor: u32,
    pub(super) partition_number: u32,
    pub(super) partition_uuid: &'static str,
    pub(super) start_512_sectors: u64,
    pub(super) size_512_sectors: u64,
    pub(super) role: GptPartitionRole,
    pub(super) validated_partition_number: u32,
    pub(super) validated_partition_uuid: &'static str,
    pub(super) start_lba: u64,
    pub(super) size_lba: u64,
    pub(super) validated_logical_block_size: u32,
    pub(super) validated_image_bytes: u64,
    pub(super) table_sha256: [u8; 32],
}

impl FixtureInput {
    pub(super) const fn standard() -> Self {
        Self {
            parent_major: 8,
            parent_minor: 0,
            partition_number: 1,
            partition_uuid: UUID,
            start_512_sectors: 2_048,
            size_512_sectors: 4_096,
            role: GptPartitionRole::Esp,
            validated_partition_number: 1,
            validated_partition_uuid: UUID,
            start_lba: 2_048,
            size_lba: 4_096,
            validated_logical_block_size: 512,
            validated_image_bytes: 64 * 1024 * 1024,
            table_sha256: TABLE_HASH,
        }
    }
}

pub(super) fn authenticate(
    observer: &mut FixtureObserver,
    input: FixtureInput,
    limits: FixtureGptPartitionDeviceLimits,
    deadline: Instant,
) -> io::Result<ReconciledGptPartitionDeviceEvidence> {
    reconcile_gpt_partition_device_fixture_until(
        observer,
        input.parent_major,
        input.parent_minor,
        input.partition_number,
        input.partition_uuid,
        input.start_512_sectors,
        input.size_512_sectors,
        input.role,
        input.validated_partition_number,
        input.validated_partition_uuid,
        input.start_lba,
        input.size_lba,
        input.validated_logical_block_size,
        input.validated_image_bytes,
        input.table_sha256,
        limits,
        deadline,
    )
}

pub(super) fn authenticate_with_clock(
    observer: &mut FixtureObserver,
    input: FixtureInput,
    limits: FixtureGptPartitionDeviceLimits,
    deadline: Instant,
    clock: &mut dyn FnMut() -> Instant,
) -> io::Result<ReconciledGptPartitionDeviceEvidence> {
    reconcile_gpt_partition_device_fixture_with_clock_until(
        observer,
        input.parent_major,
        input.parent_minor,
        input.partition_number,
        input.partition_uuid,
        input.start_512_sectors,
        input.size_512_sectors,
        input.role,
        input.validated_partition_number,
        input.validated_partition_uuid,
        input.start_lba,
        input.size_lba,
        input.validated_logical_block_size,
        input.validated_image_bytes,
        input.table_sha256,
        limits,
        deadline,
        Some(clock),
    )
}
