use std::num::NonZeroU64;

use crate::{
    client::active_reblit_boot_topology_intent::BoundActiveReblitBootPartitionSelector,
    linux_fs::sysfs_block::{SysfsDeviceNumber, SysfsDiskSequence, SysfsPartitionNumber, SysfsPartitionUuid},
};

/// Declarative role assigned to one mounted target observation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::client) enum BootTargetRole {
    Esp,
    Xbootldr,
}

/// One complete observation in the coordinator's consistency schedule.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::client) enum ObservationPhase {
    Bootstrap,
    Pass1,
    Pass2,
    Terminal,
}

/// Exact destination inode identity returned by authenticated attachment work.
///
/// The raw `st_dev` scalar participates only in equality and in the checked
/// conversion to the typed device number. It is not a pathname, descriptor,
/// block-device handle, or mutation capability.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::client) struct MountedBootDestinationIdentity {
    pub(super) raw_device: u64,
    pub(super) inode: u64,
}

impl MountedBootDestinationIdentity {
    pub(in crate::client) const fn from_stat_device_and_inode(raw_device: u64, inode: u64) -> Self {
        Self { raw_device, inode }
    }
}

/// Borrowed input facts for one target in one complete observation pass.
///
/// The declarative selector has already crossed the restricted Gluon intent
/// boundary. The descriptor-retained coordinator supplies the remaining
/// scalars from one authenticated attachment/sysfs observation; this pure type
/// does not establish that provenance itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) struct MountedBootTargetObservation<'a> {
    pub(super) intent: BoundActiveReblitBootPartitionSelector<'a>,
    pub(super) destination: MountedBootDestinationIdentity,
    pub(super) mount_id: u64,
    pub(super) device: SysfsDeviceNumber,
    pub(super) partition_number: SysfsPartitionNumber,
    pub(super) partition_uuid: SysfsPartitionUuid,
    pub(super) disk_sequence: Option<SysfsDiskSequence>,
}

impl<'a> MountedBootTargetObservation<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::client) const fn new(
        intent: BoundActiveReblitBootPartitionSelector<'a>,
        destination: MountedBootDestinationIdentity,
        mount_id: u64,
        device: SysfsDeviceNumber,
        partition_number: SysfsPartitionNumber,
        partition_uuid: SysfsPartitionUuid,
        disk_sequence: Option<SysfsDiskSequence>,
    ) -> Self {
        Self {
            intent,
            destination,
            mount_id,
            device,
            partition_number,
            partition_uuid,
            disk_sequence,
        }
    }
}

/// Closed topology input. Alias intent structurally admits exactly one target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitMountedBootTopologyObservation<'a> {
    BootAliasesEsp {
        esp: MountedBootTargetObservation<'a>,
    },
    DistinctXbootldr {
        esp: MountedBootTargetObservation<'a>,
        xbootldr: MountedBootTargetObservation<'a>,
        /// Result of comparing both freshly revalidated block-parent snapshots.
        same_revalidated_block_parent_snapshot: bool,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) struct ActiveReblitMountedBootTargetFacts {
    pub(super) selector: Box<str>,
    pub(super) partuuid: Box<str>,
    pub(super) destination: MountedBootDestinationIdentity,
    pub(super) mount_id: NonZeroU64,
    pub(super) device: SysfsDeviceNumber,
    pub(super) partition_number: SysfsPartitionNumber,
    pub(super) partition_uuid: SysfsPartitionUuid,
    pub(super) disk_sequence: Option<SysfsDiskSequence>,
}

impl ActiveReblitMountedBootTargetFacts {
    pub(super) fn bound(&self) -> BoundActiveReblitMountedBootTarget<'_> {
        BoundActiveReblitMountedBootTarget {
            selector: &self.selector,
            partuuid: &self.partuuid,
            destination: self.destination,
            mount_id: self.mount_id.get(),
            device: self.device,
            partition_number: self.partition_number,
            partition_uuid: self.partition_uuid,
            disk_sequence: self.disk_sequence,
        }
    }
}

/// Owned, invariant-checked scalar facts retained across observation passes.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitMountedBootTopology {
    BootAliasesEsp {
        esp: ActiveReblitMountedBootTargetFacts,
    },
    DistinctXbootldr {
        esp: ActiveReblitMountedBootTargetFacts,
        xbootldr: ActiveReblitMountedBootTargetFacts,
    },
}

impl ActiveReblitMountedBootTopology {
    /// Borrow semantic facts without exposing any observation authority.
    pub(in crate::client) fn bound(&self) -> BoundActiveReblitMountedBootTopology<'_> {
        match self {
            Self::BootAliasesEsp { esp } => BoundActiveReblitMountedBootTopology::BootAliasesEsp { esp: esp.bound() },
            Self::DistinctXbootldr { esp, xbootldr } => BoundActiveReblitMountedBootTopology::DistinctXbootldr {
                esp: esp.bound(),
                xbootldr: xbootldr.bound(),
            },
        }
    }
}

/// Borrowed scalar facts for one role after complete invariant validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) struct BoundActiveReblitMountedBootTarget<'a> {
    pub(in crate::client) selector: &'a str,
    pub(in crate::client) partuuid: &'a str,
    pub(in crate::client) destination: MountedBootDestinationIdentity,
    pub(in crate::client) mount_id: u64,
    pub(in crate::client) device: SysfsDeviceNumber,
    pub(in crate::client) partition_number: SysfsPartitionNumber,
    pub(in crate::client) partition_uuid: SysfsPartitionUuid,
    pub(in crate::client) disk_sequence: Option<SysfsDiskSequence>,
}

impl BoundActiveReblitMountedBootTarget<'_> {
    pub(in crate::client) const fn device_major(self) -> u32 {
        self.device.major()
    }

    pub(in crate::client) const fn device_minor(self) -> u32 {
        self.device.minor()
    }
}

/// Borrowed closed topology. Alias form contains exactly one target value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum BoundActiveReblitMountedBootTopology<'a> {
    BootAliasesEsp {
        esp: BoundActiveReblitMountedBootTarget<'a>,
    },
    DistinctXbootldr {
        esp: BoundActiveReblitMountedBootTarget<'a>,
        xbootldr: BoundActiveReblitMountedBootTarget<'a>,
    },
}
