use std::num::NonZeroU64;

use super::{
    error::ActiveReblitMountedBootTopologyError,
    model::{
        ActiveReblitMountedBootTargetFacts, ActiveReblitMountedBootTopology,
        ActiveReblitMountedBootTopologyObservation, BootTargetRole, MountedBootTargetObservation, ObservationPhase,
    },
};

impl ActiveReblitMountedBootTopology {
    /// Validate one complete scalar observation and retain exact owned facts.
    pub(in crate::client) fn from_observation(
        phase: ObservationPhase,
        observation: ActiveReblitMountedBootTopologyObservation<'_>,
    ) -> Result<Self, ActiveReblitMountedBootTopologyError> {
        match observation {
            ActiveReblitMountedBootTopologyObservation::BootAliasesEsp { esp } => Ok(Self::BootAliasesEsp {
                esp: validate_target(phase, BootTargetRole::Esp, esp)?,
            }),
            ActiveReblitMountedBootTopologyObservation::DistinctXbootldr {
                esp,
                xbootldr,
                same_revalidated_block_parent_snapshot,
            } => {
                let esp = validate_target(phase, BootTargetRole::Esp, esp)?;
                let xbootldr = validate_target(phase, BootTargetRole::Xbootldr, xbootldr)?;
                validate_distinct(phase, &esp, &xbootldr, same_revalidated_block_parent_snapshot)?;
                Ok(Self::DistinctXbootldr { esp, xbootldr })
            }
        }
    }

    /// Validate a later complete pass and require every retained scalar fact
    /// and the structural alias/distinct form to remain exact.
    pub(in crate::client) fn require_exact_observation(
        &self,
        expected_phase: ObservationPhase,
        observed_phase: ObservationPhase,
        observation: ActiveReblitMountedBootTopologyObservation<'_>,
    ) -> Result<(), ActiveReblitMountedBootTopologyError> {
        let observed = Self::from_observation(observed_phase, observation)?;
        if self == &observed {
            Ok(())
        } else {
            Err(ActiveReblitMountedBootTopologyError::PassFactsChanged {
                expected_phase,
                observed_phase,
            })
        }
    }
}

fn validate_target(
    phase: ObservationPhase,
    role: BootTargetRole,
    observation: MountedBootTargetObservation<'_>,
) -> Result<ActiveReblitMountedBootTargetFacts, ActiveReblitMountedBootTopologyError> {
    let mount_id = NonZeroU64::new(observation.mount_id)
        .ok_or(ActiveReblitMountedBootTopologyError::InvalidMountId { phase, role })?;
    require_destination_device(phase, role, observation)?;
    if observation.intent.partuuid != observation.partition_uuid.as_str() {
        return Err(ActiveReblitMountedBootTopologyError::PartitionUuidMismatch { phase, role });
    }

    Ok(ActiveReblitMountedBootTargetFacts {
        selector: observation.intent.mount_point_hint.into(),
        partuuid: observation.intent.partuuid.into(),
        destination: observation.destination,
        mount_id,
        mount_policy: observation.mount_policy,
        device: observation.device,
        partition_number: observation.partition_number,
        partition_uuid: observation.partition_uuid,
        disk_sequence: observation.disk_sequence,
    })
}

fn require_destination_device(
    phase: ObservationPhase,
    role: BootTargetRole,
    observation: MountedBootTargetObservation<'_>,
) -> Result<(), ActiveReblitMountedBootTopologyError> {
    if observation.destination.raw_device == 0 || observation.destination.inode == 0 {
        return Err(ActiveReblitMountedBootTopologyError::InvalidDestinationIdentity { phase, role });
    }
    let raw: nix::libc::dev_t = observation
        .destination
        .raw_device
        .try_into()
        .map_err(|_| ActiveReblitMountedBootTopologyError::InvalidDestinationIdentity { phase, role })?;
    let major = nix::libc::major(raw);
    let minor = nix::libc::minor(raw);
    if nix::libc::makedev(major, minor) != raw {
        return Err(ActiveReblitMountedBootTopologyError::InvalidDestinationIdentity { phase, role });
    }
    if major != observation.device.major() || minor != observation.device.minor() {
        return Err(ActiveReblitMountedBootTopologyError::DestinationDeviceMismatch { phase, role });
    }
    Ok(())
}

fn validate_distinct(
    phase: ObservationPhase,
    esp: &ActiveReblitMountedBootTargetFacts,
    xbootldr: &ActiveReblitMountedBootTargetFacts,
    same_revalidated_block_parent_snapshot: bool,
) -> Result<(), ActiveReblitMountedBootTopologyError> {
    if esp.selector == xbootldr.selector {
        return Err(ActiveReblitMountedBootTopologyError::DistinctSelectorAlias { phase });
    }
    if esp.destination == xbootldr.destination {
        return Err(ActiveReblitMountedBootTopologyError::DistinctAttachmentAlias { phase });
    }
    if esp.mount_id == xbootldr.mount_id {
        return Err(ActiveReblitMountedBootTopologyError::DistinctMountIdAlias { phase });
    }
    if esp.device == xbootldr.device {
        return Err(ActiveReblitMountedBootTopologyError::DistinctDeviceAlias { phase });
    }
    if esp.partuuid == xbootldr.partuuid {
        return Err(ActiveReblitMountedBootTopologyError::DistinctPartuuidAlias { phase });
    }
    if !same_revalidated_block_parent_snapshot || esp.disk_sequence != xbootldr.disk_sequence {
        return Err(ActiveReblitMountedBootTopologyError::BlockParentMismatch { phase });
    }
    Ok(())
}
