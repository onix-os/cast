use std::{io, time::Instant};

use thiserror::Error;

use crate::linux_fs::descriptor_boot_namespace::RetainedBootNamespaceAssessmentError;

#[derive(Debug, Error)]
pub(crate) enum RetainedBootFilePublicationError {
    #[error("boot-file publication exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("boot-file publication leaf is not one bounded canonical ASCII component")]
    InvalidCanonicalLeaf,
    #[error("boot-file publication leaf enters the case-insensitive private staging namespace")]
    ReservedPrivatePublicationLeaf,
    #[error("boot-file publication limit {field} must be nonzero and within its hard ceiling")]
    InvalidLimit { field: &'static str },
    #[error("boot-file publication length {length} exceeds the admitted {limit}-byte ceiling")]
    LengthLimitExceeded { length: u64, limit: u64 },
    #[error("retained boot attachment failed revalidation while {action}: {source}")]
    Attachment {
        action: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("retained destination identity changed while {action}")]
    DestinationIdentityChanged { action: &'static str },
    #[error("retained namespace assessment failed while {action}: {source}")]
    Namespace {
        action: &'static str,
        #[source]
        source: RetainedBootNamespaceAssessmentError,
    },
    #[error("canonical boot-file destination already contains different content")]
    DifferentCanonicalDestination,
    #[error("deterministic private boot-file residue contains different or foreign content")]
    DifferentPrivateResidue,
    #[error("deterministic private residue is present beside an already-exact canonical destination")]
    ResidueBesideExactDestination,
    #[error("boot-file source authentication failed: {source}")]
    Source {
        #[source]
        source: RetainedBootNamespaceAssessmentError,
    },
    #[error("boot-file source or destination {field} does not match its declared identity")]
    ContentIdentityMismatch { field: &'static str },
    #[error("boot-file filesystem operation failed while {action}: {source}")]
    Filesystem {
        action: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("exclusive private creation did not produce an exact resumable leaf: {source}")]
    PrivateCreationUnreconciled {
        #[source]
        source: io::Error,
    },
    #[error("the one no-replace publication move did not apply: {source}")]
    RenameNotApplied {
        #[source]
        source: io::Error,
    },
    #[error("the one no-replace publication move returned success but exact reconciliation rejected it")]
    RenameSuccessUnreconciled,
    #[error("the one no-replace publication move left ambiguous or foreign namespace evidence")]
    RenameAmbiguous,
    #[error("injected boot-file publication fault at {point}")]
    InjectedFault { point: &'static str },
}
