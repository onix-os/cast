use std::{io, time::Instant};

use thiserror::Error;

use super::super::boot_file_publication::RetainedBootFilePublicationError;

#[derive(Debug, Error)]
pub(crate) enum RetainedBootFileReplacementError {
    #[error("boot-file replacement exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("boot-file replacement predecessor and successor must name the same canonical leaf")]
    LeafMismatch,
    #[error("boot-file replacement predecessor and successor content identities are equal")]
    IdenticalContent,
    #[error("boot-file replacement failed while {action}: {source}")]
    Publication {
        action: &'static str,
        #[source]
        source: RetainedBootFilePublicationError,
    },
    #[error("boot-file replacement private sidecar already exists")]
    PrivateSidecarOccupied,
    #[error("boot-file replacement private stage creation failed: {source}")]
    PrivateStageCreation {
        #[source]
        source: io::Error,
    },
    #[error("boot-file replacement authority belongs to another retained destination")]
    AuthorityDestinationMismatch,
    #[error("boot-file replacement namespace operation failed while {action}: {source}")]
    Filesystem {
        action: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("the one boot-file exchange did not apply: {source}")]
    ExchangeNotApplied {
        #[source]
        source: io::Error,
    },
    #[error("the one boot-file exchange returned success but exact reconciliation rejected it")]
    ExchangeSuccessUnreconciled,
    #[error("the one boot-file exchange left ambiguous or foreign namespace evidence")]
    ExchangeAmbiguous,
    #[error("the one boot-file sidecar unlink did not apply: {source}")]
    UnlinkNotApplied {
        #[source]
        source: io::Error,
    },
    #[error("the one boot-file sidecar unlink returned success but exact reconciliation rejected it")]
    UnlinkSuccessUnreconciled,
    #[error("the one boot-file sidecar unlink left ambiguous or foreign namespace evidence")]
    UnlinkAmbiguous,
    #[error("the one stale boot-file detach did not apply: {source}")]
    DetachNotApplied {
        #[source]
        source: io::Error,
    },
    #[error("the one stale boot-file detach returned success but exact reconciliation rejected it")]
    DetachSuccessUnreconciled,
    #[error("the one stale boot-file detach left ambiguous or foreign namespace evidence")]
    DetachAmbiguous,
    #[error("boot-file cleanup reconciliation found a different exact lifecycle state")]
    CleanupStateMismatch,
    #[error("injected boot-file replacement stop at {point}")]
    InjectedFault { point: &'static str },
}
