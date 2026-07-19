use thiserror::Error;

use super::model::{BootTargetRole, ObservationPhase};

/// Pure topology-invariant failures. Domain capture failures deliberately stay
/// in the separate descriptor-retained coordinator error type.
#[derive(Debug, Eq, Error, PartialEq)]
pub(in crate::client) enum ActiveReblitMountedBootTopologyError {
    #[error("{phase:?} {role:?} observation has a zero mount ID")]
    InvalidMountId {
        phase: ObservationPhase,
        role: BootTargetRole,
    },
    #[error("{phase:?} {role:?} destination st_dev is not a canonical Linux device encoding")]
    InvalidDestinationIdentity {
        phase: ObservationPhase,
        role: BootTargetRole,
    },
    #[error("{phase:?} {role:?} destination st_dev disagrees with the authenticated typed device")]
    DestinationDeviceMismatch {
        phase: ObservationPhase,
        role: BootTargetRole,
    },
    #[error("{phase:?} {role:?} declarative PARTUUID disagrees with authenticated partition identity")]
    PartitionUuidMismatch {
        phase: ObservationPhase,
        role: BootTargetRole,
    },
    #[error("{phase:?} distinct ESP and XBOOTLDR selectors alias")]
    DistinctSelectorAlias { phase: ObservationPhase },
    #[error("{phase:?} distinct ESP and XBOOTLDR destination inode identities alias")]
    DistinctAttachmentAlias { phase: ObservationPhase },
    #[error("{phase:?} distinct ESP and XBOOTLDR mount IDs alias")]
    DistinctMountIdAlias { phase: ObservationPhase },
    #[error("{phase:?} distinct ESP and XBOOTLDR typed devices alias")]
    DistinctDeviceAlias { phase: ObservationPhase },
    #[error("{phase:?} distinct ESP and XBOOTLDR PARTUUIDs alias")]
    DistinctPartuuidAlias { phase: ObservationPhase },
    #[error("{phase:?} distinct ESP and XBOOTLDR lack matching revalidated block-parent evidence")]
    BlockParentMismatch { phase: ObservationPhase },
    #[error("mounted boot topology facts changed between {expected_phase:?} and {observed_phase:?}")]
    PassFactsChanged {
        expected_phase: ObservationPhase,
        observed_phase: ObservationPhase,
    },
}
