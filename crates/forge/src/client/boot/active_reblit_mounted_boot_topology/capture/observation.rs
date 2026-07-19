use std::time::Instant;

use crate::{
    Installation,
    client::active_reblit_boot_topology_intent::{
        BoundActiveReblitBootPartitionSelector, BoundActiveReblitBootTopologyIntent,
        PreparedActiveReblitBootTopologyIntent,
    },
    linux_fs::{
        mount_namespace::{PreparedMountNamespaceAnchor, RevalidatedTaskRootedAttachment},
        mountinfo_attachment::select_mountinfo_attachment_until,
        sysfs_identity::RevalidatedSysfsPartitionIdentity,
    },
};

use super::super::{
    ActiveReblitMountedBootTopologyObservation, BootTargetRole, MountedBootDestinationIdentity,
    MountedBootTargetObservation, ObservationPhase,
};
use super::{
    error::{ActiveReblitMountedBootTopologyCaptureError, ObservationBoundary},
    model::{MountInfoSource, PreparedMountedBootTarget, PreparedMountedBootTargets},
};

type CaptureResult<T> = Result<T, ActiveReblitMountedBootTopologyCaptureError>;

/// Capture one complete pass and consume its borrowed scalar observation before
/// any retained capability can be moved or dropped.
pub(super) fn capture_observation_until<T>(
    installation: &Installation,
    intent: &PreparedActiveReblitBootTopologyIntent,
    anchor: &PreparedMountNamespaceAnchor,
    targets: &PreparedMountedBootTargets,
    mountinfo_source: &MountInfoSource,
    phase: ObservationPhase,
    deadline: Instant,
    now: &mut impl FnMut() -> Instant,
    consume: impl FnOnce(ActiveReblitMountedBootTopologyObservation<'_>) -> CaptureResult<T>,
) -> CaptureResult<T> {
    require_deadline(phase, ObservationBoundary::Opening, deadline, now)?;
    let intent_open = intent.revalidate_until(installation, deadline).map_err(|source| {
        ActiveReblitMountedBootTopologyCaptureError::Intent {
            phase,
            boundary: ObservationBoundary::Opening,
            source,
        }
    })?;
    let _anchor_open = anchor.revalidate_until(deadline).map_err(|source| {
        ActiveReblitMountedBootTopologyCaptureError::MountNamespace {
            phase,
            boundary: ObservationBoundary::Opening,
            source,
        }
    })?;

    match (intent_open.topology(), targets) {
        (
            BoundActiveReblitBootTopologyIntent::BootAliasesEsp { esp: selector },
            PreparedMountedBootTargets::BootAliasesEsp { esp },
        ) => capture_alias(
            installation,
            intent,
            anchor,
            mountinfo_source,
            phase,
            deadline,
            now,
            selector,
            esp,
            consume,
        ),
        (
            BoundActiveReblitBootTopologyIntent::DistinctXbootldr {
                esp: esp_selector,
                xbootldr: xbootldr_selector,
            },
            PreparedMountedBootTargets::DistinctXbootldr { esp, xbootldr },
        ) => capture_distinct(
            installation,
            intent,
            anchor,
            mountinfo_source,
            phase,
            deadline,
            now,
            esp_selector,
            xbootldr_selector,
            esp,
            xbootldr,
            consume,
        ),
        _ => Err(ActiveReblitMountedBootTopologyCaptureError::TopologyFormChanged { phase }),
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_alias<T>(
    installation: &Installation,
    intent: &PreparedActiveReblitBootTopologyIntent,
    anchor: &PreparedMountNamespaceAnchor,
    mountinfo_source: &MountInfoSource,
    phase: ObservationPhase,
    deadline: Instant,
    now: &mut impl FnMut() -> Instant,
    selector: BoundActiveReblitBootPartitionSelector<'_>,
    esp: &PreparedMountedBootTarget,
    consume: impl FnOnce(ActiveReblitMountedBootTopologyObservation<'_>) -> CaptureResult<T>,
) -> CaptureResult<T> {
    let esp_attachment = revalidate_attachment(phase, BootTargetRole::Esp, esp, anchor, deadline)?;
    let snapshot = mountinfo_source
        .read_until(anchor, deadline)
        .map_err(|source| ActiveReblitMountedBootTopologyCaptureError::MountInfo { phase, source })?;
    select_attachment(
        phase,
        BootTargetRole::Esp,
        selector,
        &esp_attachment,
        &snapshot,
        deadline,
    )?;
    let esp_sysfs = revalidate_sysfs(phase, BootTargetRole::Esp, esp, deadline)?;

    reverse_attachment(phase, BootTargetRole::Esp, esp, anchor, deadline)?;
    close_domains(installation, intent, anchor, phase, deadline)?;
    require_deadline(phase, ObservationBoundary::Terminal, deadline, now)?;

    let consumed = consume(ActiveReblitMountedBootTopologyObservation::BootAliasesEsp {
        esp: target_observation(selector, &esp_attachment, &esp_sysfs),
    });
    require_deadline(phase, ObservationBoundary::Terminal, deadline, now)?;
    consumed
}

#[allow(clippy::too_many_arguments)]
fn capture_distinct<T>(
    installation: &Installation,
    intent: &PreparedActiveReblitBootTopologyIntent,
    anchor: &PreparedMountNamespaceAnchor,
    mountinfo_source: &MountInfoSource,
    phase: ObservationPhase,
    deadline: Instant,
    now: &mut impl FnMut() -> Instant,
    esp_selector: BoundActiveReblitBootPartitionSelector<'_>,
    xbootldr_selector: BoundActiveReblitBootPartitionSelector<'_>,
    esp: &PreparedMountedBootTarget,
    xbootldr: &PreparedMountedBootTarget,
    consume: impl FnOnce(ActiveReblitMountedBootTopologyObservation<'_>) -> CaptureResult<T>,
) -> CaptureResult<T> {
    let esp_attachment = revalidate_attachment(phase, BootTargetRole::Esp, esp, anchor, deadline)?;
    let xbootldr_attachment = revalidate_attachment(phase, BootTargetRole::Xbootldr, xbootldr, anchor, deadline)?;
    let snapshot = mountinfo_source
        .read_until(anchor, deadline)
        .map_err(|source| ActiveReblitMountedBootTopologyCaptureError::MountInfo { phase, source })?;
    select_attachment(
        phase,
        BootTargetRole::Esp,
        esp_selector,
        &esp_attachment,
        &snapshot,
        deadline,
    )?;
    select_attachment(
        phase,
        BootTargetRole::Xbootldr,
        xbootldr_selector,
        &xbootldr_attachment,
        &snapshot,
        deadline,
    )?;
    let esp_sysfs = revalidate_sysfs(phase, BootTargetRole::Esp, esp, deadline)?;
    let xbootldr_sysfs = revalidate_sysfs(phase, BootTargetRole::Xbootldr, xbootldr, deadline)?;
    let same_parent = esp_sysfs.has_same_revalidated_block_parent_snapshot(&xbootldr_sysfs);

    reverse_attachment(phase, BootTargetRole::Xbootldr, xbootldr, anchor, deadline)?;
    reverse_attachment(phase, BootTargetRole::Esp, esp, anchor, deadline)?;
    close_domains(installation, intent, anchor, phase, deadline)?;
    require_deadline(phase, ObservationBoundary::Terminal, deadline, now)?;

    let consumed = consume(ActiveReblitMountedBootTopologyObservation::DistinctXbootldr {
        esp: target_observation(esp_selector, &esp_attachment, &esp_sysfs),
        xbootldr: target_observation(xbootldr_selector, &xbootldr_attachment, &xbootldr_sysfs),
        same_revalidated_block_parent_snapshot: same_parent,
    });
    require_deadline(phase, ObservationBoundary::Terminal, deadline, now)?;
    consumed
}

fn revalidate_attachment<'a>(
    phase: ObservationPhase,
    role: BootTargetRole,
    target: &'a PreparedMountedBootTarget,
    anchor: &PreparedMountNamespaceAnchor,
    deadline: Instant,
) -> CaptureResult<RevalidatedTaskRootedAttachment<'a>> {
    target
        .attachment
        .revalidate_against_until(anchor, deadline)
        .map_err(|source| ActiveReblitMountedBootTopologyCaptureError::Attachment {
            phase,
            role,
            boundary: ObservationBoundary::Opening,
            source,
        })
}

fn reverse_attachment(
    phase: ObservationPhase,
    role: BootTargetRole,
    target: &PreparedMountedBootTarget,
    anchor: &PreparedMountNamespaceAnchor,
    deadline: Instant,
) -> CaptureResult<()> {
    target
        .attachment
        .revalidate_against_until(anchor, deadline)
        .map(drop)
        .map_err(|source| ActiveReblitMountedBootTopologyCaptureError::Attachment {
            phase,
            role,
            boundary: ObservationBoundary::Closing,
            source,
        })
}

fn revalidate_sysfs<'a>(
    phase: ObservationPhase,
    role: BootTargetRole,
    target: &'a PreparedMountedBootTarget,
    deadline: Instant,
) -> CaptureResult<RevalidatedSysfsPartitionIdentity<'a>> {
    target
        .sysfs
        .revalidate_until(deadline)
        .map_err(|source| ActiveReblitMountedBootTopologyCaptureError::Sysfs {
            phase,
            role,
            boundary: ObservationBoundary::Opening,
            source,
        })
}

fn select_attachment(
    phase: ObservationPhase,
    role: BootTargetRole,
    selector: BoundActiveReblitBootPartitionSelector<'_>,
    attachment: &RevalidatedTaskRootedAttachment<'_>,
    snapshot: &crate::linux_fs::mount_namespace::AuthenticatedMountInfoSnapshot,
    deadline: Instant,
) -> CaptureResult<()> {
    require_exact_attachment_selector(phase, role, selector.mount_point_hint, attachment.selector())?;
    let device = attachment.destination_sysfs_device_number().map_err(|source| {
        ActiveReblitMountedBootTopologyCaptureError::Attachment {
            phase,
            role,
            boundary: ObservationBoundary::Opening,
            source,
        }
    })?;
    select_mountinfo_attachment_until(
        snapshot.mountinfo(),
        selector.mount_point_hint.as_bytes(),
        attachment.destination_mount_id(),
        device.major(),
        device.minor(),
        deadline,
    )
    .map(drop)
    .map_err(|source| ActiveReblitMountedBootTopologyCaptureError::MountInfoSelection { phase, role, source })
}

fn require_exact_attachment_selector(
    phase: ObservationPhase,
    role: BootTargetRole,
    intent_selector: &str,
    attachment_selector: &str,
) -> CaptureResult<()> {
    if intent_selector == attachment_selector {
        Ok(())
    } else {
        Err(ActiveReblitMountedBootTopologyCaptureError::AttachmentSelectorMismatch { phase, role })
    }
}

#[cfg(test)]
pub(in crate::client) fn validate_fixture_attachment_selector(
    phase: ObservationPhase,
    role: BootTargetRole,
    intent_selector: &str,
    attachment_selector: &str,
) -> CaptureResult<()> {
    require_exact_attachment_selector(phase, role, intent_selector, attachment_selector)
}

fn target_observation<'a>(
    selector: BoundActiveReblitBootPartitionSelector<'a>,
    attachment: &RevalidatedTaskRootedAttachment<'_>,
    sysfs: &RevalidatedSysfsPartitionIdentity<'_>,
) -> MountedBootTargetObservation<'a> {
    MountedBootTargetObservation::new(
        selector,
        MountedBootDestinationIdentity::from_stat_device_and_inode(
            attachment.destination_device(),
            attachment.destination_inode(),
        ),
        attachment.destination_mount_id(),
        sysfs.device(),
        sysfs.partition_number(),
        sysfs.partition_uuid(),
        sysfs.disk_sequence(),
    )
}

fn close_domains(
    installation: &Installation,
    intent: &PreparedActiveReblitBootTopologyIntent,
    anchor: &PreparedMountNamespaceAnchor,
    phase: ObservationPhase,
    deadline: Instant,
) -> CaptureResult<()> {
    anchor.revalidate_until(deadline).map(drop).map_err(|source| {
        ActiveReblitMountedBootTopologyCaptureError::MountNamespace {
            phase,
            boundary: ObservationBoundary::Closing,
            source,
        }
    })?;
    intent
        .revalidate_until(installation, deadline)
        .map(drop)
        .map_err(|source| ActiveReblitMountedBootTopologyCaptureError::Intent {
            phase,
            boundary: ObservationBoundary::Closing,
            source,
        })
}

pub(super) fn require_deadline(
    phase: ObservationPhase,
    boundary: ObservationBoundary,
    deadline: Instant,
    now: &mut impl FnMut() -> Instant,
) -> CaptureResult<()> {
    if now() > deadline {
        Err(ActiveReblitMountedBootTopologyCaptureError::DeadlineExceeded {
            phase,
            boundary,
            deadline,
        })
    } else {
        Ok(())
    }
}
