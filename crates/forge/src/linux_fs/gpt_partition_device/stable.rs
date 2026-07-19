use std::{io, time::Instant};

use super::{
    ReconciledGptPartitionDeviceEvidence,
    budget::{Limits, Operation},
    geometry,
    input::{ExpectedPartition, ValidatedPartition},
    observation::{BlockDeviceObservation, BlockDeviceObserver, ObservedDeviceAccess, ObservedNodeKind},
};

const OBSERVATION_VALIDATION_WORK: usize = 8;
const EXPECTATION_WORK: usize = 3;
const STABILITY_WORK: usize = 10;
const PARTITION_EVIDENCE_WORK: usize = 4;
const RESULT_WORK: usize = 1;

pub(super) fn reconcile_until(
    observer: &mut impl BlockDeviceObserver,
    expected: &ExpectedPartition<'_>,
    validated: &ValidatedPartition<'_>,
    limits: Limits,
    deadline: Instant,
) -> io::Result<ReconciledGptPartitionDeviceEvidence> {
    reconcile_with_clock_until(observer, expected, validated, limits, deadline, None)
}

pub(super) fn reconcile_with_clock_until(
    observer: &mut impl BlockDeviceObserver,
    expected: &ExpectedPartition<'_>,
    validated: &ValidatedPartition<'_>,
    limits: Limits,
    deadline: Instant,
    clock: Option<&mut dyn FnMut() -> Instant>,
) -> io::Result<ReconciledGptPartitionDeviceEvidence> {
    let mut operation = Operation::new_with_clock(limits, deadline, clock)?;
    let opening = observe(observer, deadline, &mut operation)?;
    let geometry = validate_opening(opening, expected, validated, &mut operation)?;

    let closing = observe(observer, deadline, &mut operation)?;
    finish_reconciliation(opening, closing, expected, validated, geometry, &mut operation)
}

/// Reconcile already captured opening and closing observations without
/// invoking an observer. The live coordinator owns the observation schedule;
/// this helper performs only closed-scalar validation and construction.
pub(super) fn reconcile_observations_until(
    opening: BlockDeviceObservation,
    closing: BlockDeviceObservation,
    expected: &ExpectedPartition<'_>,
    validated: &ValidatedPartition<'_>,
    deadline: Instant,
) -> io::Result<ReconciledGptPartitionDeviceEvidence> {
    let mut operation = Operation::new(Limits::production(), deadline)?;
    let geometry = validate_opening(opening, expected, validated, &mut operation)?;
    finish_reconciliation(opening, closing, expected, validated, geometry, &mut operation)
}

/// Reject a retained opening observation before any GPT image read when its
/// closed identity, access, parent rdev, or basic geometry is not admissible.
pub(super) fn preflight_opening_observation_until(
    opening: BlockDeviceObservation,
    expected: &ExpectedPartition<'_>,
    deadline: Instant,
) -> io::Result<()> {
    let mut operation = Operation::new(Limits::production(), deadline)?;
    require_valid_observation(opening, &mut operation)?;
    require_expected_parent(opening, expected, &mut operation)?;
    geometry::require_sane_parent_observation(
        opening.logical_block_size(),
        opening.byte_length(),
        &mut operation,
    )?;
    operation.finish()
}

fn validate_opening(
    opening: BlockDeviceObservation,
    expected: &ExpectedPartition<'_>,
    validated: &ValidatedPartition<'_>,
    operation: &mut Operation<'_>,
) -> io::Result<geometry::ReconciledGeometry> {
    require_valid_observation(opening, operation)?;
    require_expected_parent(opening, expected, operation)?;

    operation.charge_work(PARTITION_EVIDENCE_WORK)?;
    if validated.partition_uuid != expected.partition_uuid || validated.partition_number != expected.partition_number {
        return Err(invalid(
            "validated GPT selection disagrees with sysfs partition identity",
        ));
    }
    if validated.logical_block_size != opening.logical_block_size() || validated.image_bytes != opening.byte_length() {
        return Err(invalid(
            "validated GPT image geometry disagrees with the parent-device observation",
        ));
    }

    geometry::require_exact_geometry(expected, validated, operation)
}

fn finish_reconciliation(
    opening: BlockDeviceObservation,
    closing: BlockDeviceObservation,
    expected: &ExpectedPartition<'_>,
    validated: &ValidatedPartition<'_>,
    geometry: geometry::ReconciledGeometry,
    operation: &mut Operation<'_>,
) -> io::Result<ReconciledGptPartitionDeviceEvidence> {
    require_valid_observation(closing, operation)?;
    require_expected_parent(closing, expected, operation)?;
    operation.charge_work(STABILITY_WORK)?;
    if opening != closing {
        return Err(invalid("parent block-device observation changed during reconciliation"));
    }

    operation.charge_work(RESULT_WORK)?;
    let result = ReconciledGptPartitionDeviceEvidence {
        containing_device: opening.containing_device(),
        inode: opening.inode(),
        mount_id: opening.mount_id(),
        parent_major: opening.block_major(),
        parent_minor: opening.block_minor(),
        logical_block_size: validated.logical_block_size,
        device_byte_length: validated.image_bytes,
        partition_number: validated.partition_number,
        partition_uuid: copy_uuid(validated.partition_uuid)?,
        partition_start_bytes: geometry.start_bytes,
        partition_size_bytes: geometry.size_bytes,
        role: validated.role,
        table_sha256: validated.table_sha256,
    };
    operation.finish()?;
    Ok(result)
}

fn observe(
    observer: &mut impl BlockDeviceObserver,
    deadline: Instant,
    operation: &mut Operation<'_>,
) -> io::Result<BlockDeviceObservation> {
    operation.reserve_observation()?;
    let observation = observer.observe_until(deadline)?;
    operation.checkpoint()?;
    Ok(observation)
}

fn require_valid_observation(observation: BlockDeviceObservation, operation: &mut Operation<'_>) -> io::Result<()> {
    operation.charge_work(OBSERVATION_VALIDATION_WORK)?;
    if observation.node_kind() != ObservedNodeKind::BlockDevice {
        return Err(invalid("retained parent node is not a block device"));
    }
    if observation.access() != ObservedDeviceAccess::ReadOnly {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "retained parent block-device descriptor is write-capable",
        ));
    }
    if observation.containing_device() == 0 || observation.inode() == 0 || observation.mount_id() == 0 {
        return Err(invalid("retained parent block-device identity contains a zero scalar"));
    }
    if observation.byte_length() == 0 {
        return Err(invalid("retained parent block-device length is zero"));
    }
    operation.checkpoint()
}

fn require_expected_parent(
    observation: BlockDeviceObservation,
    expected: &ExpectedPartition<'_>,
    operation: &mut Operation<'_>,
) -> io::Result<()> {
    operation.charge_work(EXPECTATION_WORK)?;
    if observation.block_major() != expected.parent_major || observation.block_minor() != expected.parent_minor {
        return Err(invalid(
            "retained block node rdev disagrees with authenticated sysfs parent",
        ));
    }
    operation.checkpoint()
}

fn copy_uuid(uuid: &str) -> io::Result<[u8; 36]> {
    let mut bytes = [0_u8; 36];
    if uuid.len() != bytes.len() {
        return Err(invalid("partition UUID is not canonical length"));
    }
    bytes.copy_from_slice(uuid.as_bytes());
    Ok(bytes)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
