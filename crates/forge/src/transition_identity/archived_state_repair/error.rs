use std::{io, path::PathBuf};

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArchivedStateRepairOutcome {
    /// Exact reconciliation proves no requested wrapper move was applied.
    NotApplied,
    /// The repaired or preserved wrapper reached its sticky destination, but
    /// a cleanup, durability, or semantic suffix did not finish.
    Applied,
    /// Retained namespace evidence no longer proves either admitted layout.
    Ambiguous,
}

#[derive(Debug, Error)]
#[error("archived-state repair outcome is {outcome:?}")]
pub(crate) struct ArchivedStateRepairFailure {
    pub(super) outcome: ArchivedStateRepairOutcome,
    #[source]
    pub(super) source: ArchivedStateRepairError,
}

impl ArchivedStateRepairFailure {
    pub(crate) fn outcome(&self) -> ArchivedStateRepairOutcome {
        self.outcome
    }
}

#[derive(Debug, Error)]
pub(crate) enum ArchivedStateRepairError {
    #[error("{operation} while retaining archived-state repair identity: {source}")]
    Identity {
        operation: &'static str,
        #[source]
        source: Box<super::super::Error>,
    },
    #[error("{operation} in archived-state repair namespace at `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("load retained archived-state row {state}")]
    StateLookup {
        state: i32,
        #[source]
        source: crate::db::Error,
    },
    #[error("retained archived-state row {state} changed during repair")]
    StateChanged { state: i32 },
    #[error("load retained active state row {state} during archived repair")]
    ActiveStateLookup {
        state: i32,
        #[source]
        source: crate::db::Error,
    },
    #[error("retained active state row {state} changed during archived repair")]
    ActiveStateChanged { state: i32 },
    #[error("active state selection changed during archived repair: expected {expected:?}, found {actual:?}")]
    ActiveSelectionChanged { expected: Option<i32>, actual: Option<i32> },
    #[error("archived-state repair target {state} became active")]
    TargetBecameActive { state: i32 },
    #[error("live active-state marker for expected state {expected} is missing")]
    LiveActiveStateMissing { expected: i32 },
    #[error("live active-state marker unexpectedly exists at `{}`", path.display())]
    UnexpectedLiveActiveState { path: PathBuf },
    #[error("construct a private archived-state repair quarantine name")]
    InvalidQuarantineName(#[source] crate::transition_journal::CodecError),
    #[error("all {limit} private archived-state repair quarantine names are occupied for state {state}")]
    QuarantineExhausted { state: i32, limit: usize },
    #[error(
        "archived-state repair preparation failed ({primary}) and exact empty-reservation retirement also failed ({cleanup})"
    )]
    PreparationReservationCleanupFailed {
        primary: Box<ArchivedStateRepairError>,
        cleanup: Box<ArchivedStateRepairError>,
    },
    #[error(
        "archived-state repair preparation reservation at `{}` has link count {actual}, expected {expected}",
        path.display()
    )]
    PreparationReservationLinkCount { path: PathBuf, expected: u64, actual: u64 },
    #[error("archived-state repair preparation reservation name changed at `{}`", path.display())]
    PreparationReservationNamespaceChanged { path: PathBuf },
    #[error("archived-state repair wrapper `{}` is unsafe (uid={owner}, mode={mode:04o})", path.display())]
    UnsafeWrapper { path: PathBuf, owner: u32, mode: u32 },
    #[error(
        "archived-state repair wrappers are on different filesystems: roots `{}` and `{}`",
        roots.display(),
        other.display()
    )]
    CrossDevice { roots: PathBuf, other: PathBuf },
    #[error("archived-state repair operation lock is poisoned")]
    OperationLockPoisoned,
    #[error(
        "archived-state repair namespace mismatch: canonical={canonical}, staging={staging}, quarantine={quarantine}"
    )]
    NamespaceMismatch {
        canonical: &'static str,
        staging: &'static str,
        quarantine: &'static str,
    },
    #[error("archived-state repair namespace changed during one bounded C/S/Q observation")]
    NamespaceChangedDuringObservation,
    #[error("{operation} while observing the archived-state repair namespace: {source}")]
    NamespaceObservation {
        operation: &'static str,
        #[source]
        source: Box<super::super::Error>,
    },
    #[error("{operation} while proving the archived-state repair namespace: {source}")]
    NamespaceProof {
        operation: &'static str,
        #[source]
        source: Box<ArchivedStateRepairError>,
    },
    #[error("{operation} reported success without moving an exact retained wrapper")]
    ReportedSuccessWithoutMove { operation: &'static str },
    #[error("{operation} preflight failed ({primary}) and exact reconciliation failed ({reconciliation})")]
    PreflightReconciliationFailed {
        operation: &'static str,
        primary: Box<ArchivedStateRepairError>,
        reconciliation: Box<ArchivedStateRepairError>,
    },
    #[error("{operation} applied after a preflight error ({primary}), then its suffix failed ({finish})")]
    AppliedAfterPreflightFailure {
        operation: &'static str,
        primary: Box<ArchivedStateRepairError>,
        finish: Box<ArchivedStateRepairError>,
    },
    #[cfg(test)]
    #[error("injected archived-state repair fault at {point:?}")]
    InjectedFault {
        point: super::ArchivedStateRepairFaultPoint,
    },
}

impl ArchivedStateRepairError {
    pub(super) fn namespace_is_uncertain(&self) -> bool {
        match self {
            Self::NamespaceMismatch { .. }
            | Self::NamespaceChangedDuringObservation
            | Self::NamespaceObservation { .. }
            | Self::NamespaceProof { .. } => true,
            Self::PreparationReservationCleanupFailed { cleanup, .. } => cleanup.namespace_is_uncertain(),
            Self::PreparationReservationNamespaceChanged { .. } => true,
            _ => false,
        }
    }

    pub(super) fn namespace_observation_was_unstable(&self) -> bool {
        match self {
            Self::NamespaceChangedDuringObservation | Self::NamespaceObservation { .. } => true,
            Self::NamespaceProof { source, .. } => source.namespace_observation_was_unstable(),
            Self::PreparationReservationCleanupFailed { cleanup, .. } => cleanup.namespace_observation_was_unstable(),
            _ => false,
        }
    }
}

pub(super) fn identity(operation: &'static str, source: super::super::Error) -> ArchivedStateRepairError {
    ArchivedStateRepairError::Identity {
        operation,
        source: Box::new(source),
    }
}

pub(super) fn namespace_observation(operation: &'static str, source: super::super::Error) -> ArchivedStateRepairError {
    ArchivedStateRepairError::NamespaceObservation {
        operation,
        source: Box::new(source),
    }
}

pub(super) fn namespace_proof(operation: &'static str, source: ArchivedStateRepairError) -> ArchivedStateRepairError {
    ArchivedStateRepairError::NamespaceProof {
        operation,
        source: Box::new(source),
    }
}

pub(super) fn io_error(
    operation: &'static str,
    path: impl Into<PathBuf>,
    source: io::Error,
) -> ArchivedStateRepairError {
    ArchivedStateRepairError::Io {
        operation,
        path: path.into(),
        source,
    }
}

pub(super) fn failure(
    outcome: ArchivedStateRepairOutcome,
    source: ArchivedStateRepairError,
) -> ArchivedStateRepairFailure {
    ArchivedStateRepairFailure { outcome, source }
}
