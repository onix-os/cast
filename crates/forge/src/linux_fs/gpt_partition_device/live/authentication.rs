use std::{io, time::Instant};

use crate::linux_fs::{
    gpt_partition_role::{
        GptPartitionRole, GptPartitionRoleImage, authenticate_gpt_partition_role_sources_with_interpass_until,
    },
    sysfs_identity::SysfsGptDeviceExpectation,
};

use super::{
    super::{
        BlockDeviceObservation, BlockDeviceObserver, ReconciledGptPartitionDeviceEvidence,
        input::{ExpectedPartition, ValidatedPartition},
        stable,
    },
    observation::RetainedBlockDeviceObserver,
    retained_parent::CanonicalRetainedParentOpening,
};

/// Closed read-provenance evidence from one retained read-only block device.
///
/// Construction requires an opening observation exactly equal to the retained
/// parent's canonical opening, two exact GPT passes with a caller-owned name
/// rebind and a same-descriptor observation between them, a closing
/// observation, and pure reconciliation with one authenticated sysfs
/// expectation. No descriptor, path, image, buffer, observer, callback, or
/// reusable operation authority survives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::linux_fs) struct LiveAuthenticatedGptPartitionDeviceEvidence {
    closed_scalars: ReconciledGptPartitionDeviceEvidence,
}

impl LiveAuthenticatedGptPartitionDeviceEvidence {
    pub(in crate::linux_fs) const fn containing_device(&self) -> u64 {
        self.closed_scalars.containing_device()
    }

    pub(in crate::linux_fs) const fn inode(&self) -> u64 {
        self.closed_scalars.inode()
    }

    pub(in crate::linux_fs) const fn mount_id(&self) -> u64 {
        self.closed_scalars.mount_id()
    }

    pub(in crate::linux_fs) const fn parent_major(&self) -> u32 {
        self.closed_scalars.parent_major()
    }

    pub(in crate::linux_fs) const fn parent_minor(&self) -> u32 {
        self.closed_scalars.parent_minor()
    }

    pub(in crate::linux_fs) const fn logical_block_size(&self) -> u32 {
        self.closed_scalars.logical_block_size()
    }

    pub(in crate::linux_fs) const fn device_byte_length(&self) -> u64 {
        self.closed_scalars.device_byte_length()
    }

    pub(in crate::linux_fs) const fn partition_number(&self) -> u32 {
        self.closed_scalars.partition_number()
    }

    pub(in crate::linux_fs) fn partition_uuid(&self) -> &str {
        self.closed_scalars.partition_uuid()
    }

    pub(in crate::linux_fs) const fn partition_start_bytes(&self) -> u64 {
        self.closed_scalars.partition_start_bytes()
    }

    pub(in crate::linux_fs) const fn partition_size_bytes(&self) -> u64 {
        self.closed_scalars.partition_size_bytes()
    }

    pub(in crate::linux_fs) const fn role(&self) -> GptPartitionRole {
        self.closed_scalars.role()
    }

    pub(in crate::linux_fs) const fn table_sha256(&self) -> &[u8; 32] {
        self.closed_scalars.table_sha256()
    }
}

/// Authenticate GPT reads from one already retained block descriptor.
///
/// The callback is a sealed composition seam for re-opening and revalidating
/// the authenticated relative device name under its retained `/dev` owner. It
/// runs once with the original deadline after pass one and before the
/// same-descriptor inter-pass observation. Failure prevents every pass-two
/// read. The opening must equal the sealed opening from the owner which
/// retained this descriptor; disagreement fails before an image is created or
/// read. This function itself accepts no path and performs no discovery.
pub(super) fn authenticate_retained_gpt_partition_device_with_interpass_until(
    observer: &mut RetainedBlockDeviceObserver<'_>,
    retained_opening: CanonicalRetainedParentOpening,
    expected: &SysfsGptDeviceExpectation<'_>,
    expected_role: GptPartitionRole,
    deadline: Instant,
    rebind_parent_name: &mut impl FnMut(Instant) -> io::Result<()>,
) -> io::Result<LiveAuthenticatedGptPartitionDeviceEvidence> {
    checkpoint(deadline)?;
    let opening = observer.observe_until(deadline)?;
    checkpoint(deadline)?;
    require_retained_opening_until(opening, retained_opening, deadline)?;

    let parent = expected.parent_device();
    let partition_uuid = expected.partition_uuid();
    let expected = ExpectedPartition {
        parent_major: parent.major(),
        parent_minor: parent.minor(),
        partition_number: expected.partition_number().get(),
        partition_uuid: partition_uuid.as_str(),
        start_512_sectors: expected.partition_start_512_sectors(),
        size_512_sectors: expected.partition_size_512_sectors(),
    };
    stable::preflight_opening_observation_until(opening, &expected, deadline)?;
    let mut first_source = observer.image_until(deadline)?;
    let mut second_source = observer.image_until(deadline)?;
    authenticate_after_opening_until(
        observer,
        opening,
        &mut first_source,
        &mut second_source,
        &expected,
        expected_role,
        deadline,
        rebind_parent_name,
    )
}

fn require_retained_opening_until(
    observed: BlockDeviceObservation,
    retained: CanonicalRetainedParentOpening,
    deadline: Instant,
) -> io::Result<()> {
    checkpoint(deadline)?;
    if observed != retained.observation() {
        return Err(invalid(
            "GPT coordinator opening disagrees with the retained parent's canonical opening",
        ));
    }
    checkpoint(deadline)
}

#[allow(clippy::too_many_arguments)]
fn authenticate_after_opening_until(
    observer: &mut impl BlockDeviceObserver,
    opening: BlockDeviceObservation,
    first_source: &mut impl GptPartitionRoleImage,
    second_source: &mut impl GptPartitionRoleImage,
    expected: &ExpectedPartition<'_>,
    expected_role: GptPartitionRole,
    deadline: Instant,
    rebind_parent_name: &mut impl FnMut(Instant) -> io::Result<()>,
) -> io::Result<LiveAuthenticatedGptPartitionDeviceEvidence> {
    let logical_block_size = opening.logical_block_size();
    let mut interpass_completed = false;
    let validated = {
        let mut interpass = |received_deadline| {
            if received_deadline != deadline {
                return Err(invalid("GPT parser replaced the coordinator deadline"));
            }
            rebind_parent_name(received_deadline)?;
            checkpoint(deadline)?;
            let observed = observer.observe_until(received_deadline)?;
            checkpoint(deadline)?;
            if observed != opening {
                return Err(invalid(
                    "retained block-device identity, access, or geometry changed between GPT passes",
                ));
            }
            interpass_completed = true;
            Ok(())
        };
        authenticate_gpt_partition_role_sources_with_interpass_until(
            first_source,
            second_source,
            logical_block_size,
            expected.partition_number,
            expected.partition_uuid,
            expected_role,
            deadline,
            &mut interpass,
        )?
    };
    if !interpass_completed {
        return Err(invalid(
            "GPT authentication omitted its mandatory inter-pass revalidation",
        ));
    }

    checkpoint(deadline)?;
    let closing = observer.observe_until(deadline)?;
    checkpoint(deadline)?;
    let validated = ValidatedPartition {
        role: validated.role(),
        partition_number: validated.partition_number(),
        partition_uuid: validated.partition_uuid(),
        start_lba: validated.start_lba(),
        size_lba: validated.size_lba(),
        logical_block_size: validated.logical_block_size(),
        image_bytes: validated.image_bytes(),
        table_sha256: *validated.table_sha256(),
    };
    let closed_scalars = stable::reconcile_observations_until(opening, closing, expected, &validated, deadline)?;
    checkpoint(deadline)?;
    Ok(LiveAuthenticatedGptPartitionDeviceEvidence { closed_scalars })
}

fn checkpoint(deadline: Instant) -> io::Result<()> {
    if Instant::now() > deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "live GPT-device authentication exceeded its caller deadline",
        ))
    } else {
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(in crate::linux_fs) fn authenticate_retained_gpt_partition_device_sources_fixture_with_interpass_until(
    observer: &mut impl BlockDeviceObserver,
    first_source: &mut impl GptPartitionRoleImage,
    second_source: &mut impl GptPartitionRoleImage,
    retained_opening: BlockDeviceObservation,
    parent_major: u32,
    parent_minor: u32,
    partition_number: u32,
    partition_uuid: &str,
    start_512_sectors: u64,
    size_512_sectors: u64,
    expected_role: GptPartitionRole,
    deadline: Instant,
    rebind_parent_name: &mut impl FnMut(Instant) -> io::Result<()>,
) -> io::Result<LiveAuthenticatedGptPartitionDeviceEvidence> {
    checkpoint(deadline)?;
    let opening = observer.observe_until(deadline)?;
    checkpoint(deadline)?;
    require_retained_opening_until(
        opening,
        CanonicalRetainedParentOpening::fixture(retained_opening),
        deadline,
    )?;
    let expected = ExpectedPartition {
        parent_major,
        parent_minor,
        partition_number,
        partition_uuid,
        start_512_sectors,
        size_512_sectors,
    };
    stable::preflight_opening_observation_until(opening, &expected, deadline)?;
    authenticate_after_opening_until(
        observer,
        opening,
        first_source,
        second_source,
        &expected,
        expected_role,
        deadline,
        rebind_parent_name,
    )
}
