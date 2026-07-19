use std::{
    io,
    time::{Duration, Instant},
};

use thiserror::Error;

use super::super::{ActiveReblitMountedBootTopologyError, BootTargetRole, ObservationPhase};
use crate::client::active_reblit_boot_topology_intent::ActiveReblitBootTopologyIntentError;

/// Whether a retained domain failed while entering or closing one observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ObservationBoundary {
    Preparation,
    Opening,
    Closing,
    Terminal,
}

/// Typed failures from the descriptor-retained topology coordinator.
///
/// Sources remain available for diagnosis, but no variant contains a file,
/// descriptor, reopen closure, or mutation capability.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitMountedBootTopologyCaptureError {
    #[error("mounted boot topology deadline {timeout:?} cannot be represented")]
    InvalidDeadline { timeout: Duration },
    #[error("{phase:?} mounted boot topology exceeded caller deadline {deadline:?} at {boundary:?}")]
    DeadlineExceeded {
        phase: ObservationPhase,
        boundary: ObservationBoundary,
        deadline: Instant,
    },
    #[error("{phase:?} declarative intent failed at {boundary:?}")]
    Intent {
        phase: ObservationPhase,
        boundary: ObservationBoundary,
        #[source]
        source: ActiveReblitBootTopologyIntentError,
    },
    #[error("{phase:?} mount-context anchor failed at {boundary:?}")]
    MountNamespace {
        phase: ObservationPhase,
        boundary: ObservationBoundary,
        #[source]
        source: io::Error,
    },
    #[error("{phase:?} {role:?} retained attachment failed at {boundary:?}")]
    Attachment {
        phase: ObservationPhase,
        role: BootTargetRole,
        boundary: ObservationBoundary,
        #[source]
        source: io::Error,
    },
    #[error("{phase:?} {role:?} retained attachment selector disagrees with declarative intent")]
    AttachmentSelectorMismatch {
        phase: ObservationPhase,
        role: BootTargetRole,
    },
    #[error("{phase:?} authenticated mountinfo snapshot failed")]
    MountInfo {
        phase: ObservationPhase,
        #[source]
        source: io::Error,
    },
    #[error("{phase:?} {role:?} exact mountinfo selection failed")]
    MountInfoSelection {
        phase: ObservationPhase,
        role: BootTargetRole,
        #[source]
        source: io::Error,
    },
    #[error("{phase:?} {role:?} retained sysfs partition identity failed at {boundary:?}")]
    Sysfs {
        phase: ObservationPhase,
        role: BootTargetRole,
        boundary: ObservationBoundary,
        #[source]
        source: io::Error,
    },
    #[error("{phase:?} declarative topology form disagrees with retained target capabilities")]
    TopologyFormChanged { phase: ObservationPhase },
    #[error("{phase:?} mounted boot scalar invariants failed")]
    Topology {
        phase: ObservationPhase,
        #[source]
        source: ActiveReblitMountedBootTopologyError,
    },
}
