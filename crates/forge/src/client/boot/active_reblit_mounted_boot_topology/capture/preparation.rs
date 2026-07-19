use std::{
    io,
    marker::PhantomData,
    rc::Rc,
    time::{Duration, Instant},
};

use crate::{
    Installation,
    client::active_reblit_boot_topology_intent::{
        BoundActiveReblitBootTopologyIntent, PreparedActiveReblitBootTopologyIntent,
    },
    linux_fs::{
        mount_namespace::PreparedMountNamespaceAnchor, sysfs_block::SysfsDeviceNumber,
        sysfs_identity::PreparedSysfsPartitionIdentity,
    },
};

#[cfg(test)]
use crate::linux_fs::sysfs_identity::{FixtureSysfsIdentityLimits, FixtureSysfsTree};

use super::super::{ActiveReblitMountedBootTopology, BootTargetRole, ObservationPhase};
use super::{
    error::{ActiveReblitMountedBootTopologyCaptureError, ObservationBoundary},
    model::{
        MountInfoSource, PreparedActiveReblitMountedBootTopology, PreparedMountedBootTarget,
        PreparedMountedBootTargets, RevalidatedActiveReblitMountedBootTopology,
    },
    observation::{capture_observation_until, require_deadline},
};

#[cfg(test)]
use super::model::FixtureMountInfoFeed;

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);

type CaptureResult<T> = Result<T, ActiveReblitMountedBootTopologyCaptureError>;

impl PreparedActiveReblitMountedBootTopology {
    /// Prepare production intent, mount-context, attachment, and sysfs
    /// capabilities under one finite operation deadline.
    pub(in crate::client) fn prepare(installation: &Installation) -> CaptureResult<Self> {
        let deadline = deadline_after(CAPTURE_TIMEOUT)?;
        Self::prepare_until(installation, deadline)
    }

    /// Prepare without replacing the caller's absolute deadline in any stage.
    pub(in crate::client) fn prepare_until(installation: &Installation, deadline: Instant) -> CaptureResult<Self> {
        let mut now = Instant::now;
        require_deadline(
            ObservationPhase::Bootstrap,
            ObservationBoundary::Preparation,
            deadline,
            &mut now,
        )?;
        let intent =
            PreparedActiveReblitBootTopologyIntent::prepare_until(installation, deadline).map_err(|source| {
                ActiveReblitMountedBootTopologyCaptureError::Intent {
                    phase: ObservationPhase::Bootstrap,
                    boundary: ObservationBoundary::Preparation,
                    source,
                }
            })?;
        let anchor = PreparedMountNamespaceAnchor::prepare_until(deadline).map_err(|source| {
            ActiveReblitMountedBootTopologyCaptureError::MountNamespace {
                phase: ObservationPhase::Bootstrap,
                boundary: ObservationBoundary::Preparation,
                source,
            }
        })?;
        let mut prepare_sysfs = |device, _role| PreparedSysfsPartitionIdentity::prepare_until(device, deadline);
        prepare_from_capabilities_until(
            installation,
            intent,
            anchor,
            MountInfoSource::Production,
            deadline,
            &mut now,
            &mut prepare_sysfs,
        )
    }

    /// Revalidate three complete observations before exposing scalar facts.
    pub(in crate::client) fn revalidate<'a>(
        &'a self,
        installation: &'a Installation,
    ) -> CaptureResult<RevalidatedActiveReblitMountedBootTopology<'a>> {
        let deadline = deadline_after(CAPTURE_TIMEOUT)?;
        self.revalidate_until(installation, deadline)
    }

    /// Revalidate without resetting the caller-owned absolute deadline.
    pub(in crate::client) fn revalidate_until<'a>(
        &'a self,
        installation: &'a Installation,
        deadline: Instant,
    ) -> CaptureResult<RevalidatedActiveReblitMountedBootTopology<'a>> {
        let mut now = Instant::now;
        self.revalidate_until_with_clock(installation, deadline, &mut now)
    }

    fn revalidate_until_with_clock<'a>(
        &'a self,
        installation: &'a Installation,
        deadline: Instant,
        now: &mut impl FnMut() -> Instant,
    ) -> CaptureResult<RevalidatedActiveReblitMountedBootTopology<'a>> {
        for phase in [
            ObservationPhase::Pass1,
            ObservationPhase::Pass2,
            ObservationPhase::Terminal,
        ] {
            capture_observation_until(
                installation,
                &self.intent,
                &self.anchor,
                &self.targets,
                &self.mountinfo_source,
                phase,
                deadline,
                now,
                |observation| {
                    self.facts
                        .require_exact_observation(ObservationPhase::Bootstrap, phase, observation)
                        .map_err(|source| ActiveReblitMountedBootTopologyCaptureError::Topology { phase, source })
                },
            )?;
        }
        Ok(RevalidatedActiveReblitMountedBootTopology {
            prepared: self,
            _installation: installation,
            deadline,
            _same_thread: PhantomData::<Rc<()>>,
        })
    }

    #[cfg(test)]
    pub(in crate::client) fn revalidate_fixture_until_with_clock<'a>(
        &'a self,
        installation: &'a Installation,
        deadline: Instant,
        now: &mut impl FnMut() -> Instant,
    ) -> CaptureResult<RevalidatedActiveReblitMountedBootTopology<'a>> {
        self.revalidate_until_with_clock(installation, deadline, now)
    }

    #[cfg(test)]
    pub(in crate::client) fn prepare_fixture_until(
        installation: &Installation,
        anchor: PreparedMountNamespaceAnchor,
        sysfs_tree: &FixtureSysfsTree,
        mountinfo: FixtureMountInfoFeed,
        deadline: Instant,
    ) -> CaptureResult<Self> {
        let mut now = Instant::now;
        require_deadline(
            ObservationPhase::Bootstrap,
            ObservationBoundary::Preparation,
            deadline,
            &mut now,
        )?;
        let intent =
            PreparedActiveReblitBootTopologyIntent::prepare_until(installation, deadline).map_err(|source| {
                ActiveReblitMountedBootTopologyCaptureError::Intent {
                    phase: ObservationPhase::Bootstrap,
                    boundary: ObservationBoundary::Preparation,
                    source,
                }
            })?;
        let mut prepare_sysfs = |device, _role| {
            let mut hook = |_| Ok(());
            sysfs_tree.prepare_with(device, FixtureSysfsIdentityLimits::default(), deadline, &mut hook)
        };
        prepare_from_capabilities_until(
            installation,
            intent,
            anchor,
            MountInfoSource::Fixture(mountinfo),
            deadline,
            &mut now,
            &mut prepare_sysfs,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_from_capabilities_until(
    installation: &Installation,
    intent: PreparedActiveReblitBootTopologyIntent,
    anchor: PreparedMountNamespaceAnchor,
    mountinfo_source: MountInfoSource,
    deadline: Instant,
    now: &mut impl FnMut() -> Instant,
    prepare_sysfs: &mut impl FnMut(SysfsDeviceNumber, BootTargetRole) -> io::Result<PreparedSysfsPartitionIdentity>,
) -> CaptureResult<PreparedActiveReblitMountedBootTopology> {
    let targets = prepare_targets_until(installation, &intent, &anchor, deadline, prepare_sysfs)?;
    let facts = capture_observation_until(
        installation,
        &intent,
        &anchor,
        &targets,
        &mountinfo_source,
        ObservationPhase::Bootstrap,
        deadline,
        now,
        |observation| {
            ActiveReblitMountedBootTopology::from_observation(ObservationPhase::Bootstrap, observation).map_err(
                |source| ActiveReblitMountedBootTopologyCaptureError::Topology {
                    phase: ObservationPhase::Bootstrap,
                    source,
                },
            )
        },
    )?;
    let prepared = PreparedActiveReblitMountedBootTopology {
        intent,
        anchor,
        targets,
        mountinfo_source,
        facts,
    };
    prepared
        .revalidate_until_with_clock(installation, deadline, now)
        .map(drop)?;
    Ok(prepared)
}

fn prepare_targets_until(
    installation: &Installation,
    intent: &PreparedActiveReblitBootTopologyIntent,
    anchor: &PreparedMountNamespaceAnchor,
    deadline: Instant,
    prepare_sysfs: &mut impl FnMut(SysfsDeviceNumber, BootTargetRole) -> io::Result<PreparedSysfsPartitionIdentity>,
) -> CaptureResult<PreparedMountedBootTargets> {
    let intent_view = intent.revalidate_until(installation, deadline).map_err(|source| {
        ActiveReblitMountedBootTopologyCaptureError::Intent {
            phase: ObservationPhase::Bootstrap,
            boundary: ObservationBoundary::Preparation,
            source,
        }
    })?;
    let anchor_view = anchor.revalidate_until(deadline).map_err(|source| {
        ActiveReblitMountedBootTopologyCaptureError::MountNamespace {
            phase: ObservationPhase::Bootstrap,
            boundary: ObservationBoundary::Preparation,
            source,
        }
    })?;
    match intent_view.topology() {
        BoundActiveReblitBootTopologyIntent::BootAliasesEsp { esp } => Ok(PreparedMountedBootTargets::BootAliasesEsp {
            esp: prepare_target_until(
                &anchor_view,
                anchor,
                esp.mount_point_hint,
                BootTargetRole::Esp,
                deadline,
                prepare_sysfs,
            )?,
        }),
        BoundActiveReblitBootTopologyIntent::DistinctXbootldr { esp, xbootldr } => {
            Ok(PreparedMountedBootTargets::DistinctXbootldr {
                esp: prepare_target_until(
                    &anchor_view,
                    anchor,
                    esp.mount_point_hint,
                    BootTargetRole::Esp,
                    deadline,
                    prepare_sysfs,
                )?,
                xbootldr: prepare_target_until(
                    &anchor_view,
                    anchor,
                    xbootldr.mount_point_hint,
                    BootTargetRole::Xbootldr,
                    deadline,
                    prepare_sysfs,
                )?,
            })
        }
    }
}

fn prepare_target_until(
    anchor_view: &crate::linux_fs::mount_namespace::RevalidatedMountNamespaceAnchor<'_>,
    anchor: &PreparedMountNamespaceAnchor,
    selector: &str,
    role: BootTargetRole,
    deadline: Instant,
    prepare_sysfs: &mut impl FnMut(SysfsDeviceNumber, BootTargetRole) -> io::Result<PreparedSysfsPartitionIdentity>,
) -> CaptureResult<PreparedMountedBootTarget> {
    let attachment = anchor_view
        .prepare_task_rooted_attachment_until(selector, deadline)
        .map_err(|source| ActiveReblitMountedBootTopologyCaptureError::Attachment {
            phase: ObservationPhase::Bootstrap,
            role,
            boundary: ObservationBoundary::Preparation,
            source,
        })?;
    let device = attachment
        .revalidate_against_until(anchor, deadline)
        .and_then(|view| view.destination_sysfs_device_number())
        .map_err(|source| ActiveReblitMountedBootTopologyCaptureError::Attachment {
            phase: ObservationPhase::Bootstrap,
            role,
            boundary: ObservationBoundary::Preparation,
            source,
        })?;
    let sysfs = prepare_sysfs(device, role).map_err(|source| ActiveReblitMountedBootTopologyCaptureError::Sysfs {
        phase: ObservationPhase::Bootstrap,
        role,
        boundary: ObservationBoundary::Preparation,
        source,
    })?;
    Ok(PreparedMountedBootTarget { attachment, sysfs })
}

fn deadline_after(timeout: Duration) -> CaptureResult<Instant> {
    Instant::now()
        .checked_add(timeout)
        .ok_or(ActiveReblitMountedBootTopologyCaptureError::InvalidDeadline { timeout })
}
