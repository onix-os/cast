//! Retained wrapper identities, outcomes, and namespace states.

use std::{ffi::CString, path::PathBuf};

use thiserror::Error;

use crate::transition_identity::RetainedDirectory;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedStagingWrapperRotationOutcome {
    NotApplied,
    Applied,
    Ambiguous,
}

#[derive(Debug, Error)]
#[error("retained staging-wrapper rotation outcome is {outcome:?}")]
pub(crate) struct RetainedStagingWrapperRotationFailure {
    pub(super) outcome: RetainedStagingWrapperRotationOutcome,
    #[source]
    pub(super) source: StagingWrapperRotationError,
}

impl RetainedStagingWrapperRotationFailure {
    pub(crate) fn outcome(&self) -> RetainedStagingWrapperRotationOutcome {
        self.outcome
    }

    #[cfg(test)]
    pub(crate) fn coordinator_evidence_for_test(
        &self,
    ) -> Option<&crate::transition_identity::journal_coordinator::StatefulTransitionCoordinatorError> {
        match &self.source {
            StagingWrapperRotationError::CoordinatorEvidence(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedStagingWrapperRotationFaultPoint {
    ReplacementPostCreate,
    ReplacementPreparationSync,
    QuarantinePreparationSync,
    FinalPreparationRevalidation,
    OriginalPreSync,
    ReplacementPreSync,
    QuarantinePreSync,
    BeforeExchange,
    AfterExchange,
    OriginalPostSync,
    ReplacementPostSync,
    RootsParentSync,
    QuarantineParentSync,
    FinalRevalidation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::transition_identity) enum StagingWrapperPreparationEvidenceStage {
    FinalCheckpoint,
    EvidenceSandwich,
}

#[derive(Debug, Error)]
pub(in crate::transition_identity) enum StagingWrapperPreparationFailure {
    #[error("staging replacement durability is not established")]
    DurabilityUnproven(#[source] StagingWrapperRotationError),
    #[error("durable staging replacement evidence failed during {stage:?}")]
    DurableReservationEvidenceFailed {
        stage: StagingWrapperPreparationEvidenceStage,
        #[source]
        source: StagingWrapperRotationError,
    },
}

#[derive(Debug, Error)]
pub(in crate::transition_identity) enum StagingWrapperRotationError {
    #[error("{operation} while rotating the retained staging wrapper: {source}")]
    Identity {
        operation: &'static str,
        #[source]
        source: Box<crate::transition_identity::Error>,
    },
    #[error("coordinator evidence failed while reserving the retained staging wrapper")]
    CoordinatorEvidence(
        #[source] Box<crate::transition_identity::journal_coordinator::StatefulTransitionCoordinatorError>,
    ),
    #[error("construct the private staging-wrapper quarantine name")]
    InvalidName(#[source] crate::transition_journal::CodecError),
    #[error("all {limit} private staging-wrapper quarantine names are occupied")]
    DestinationExhausted { limit: usize },
    #[error(
        "staging wrapper and quarantine replacement are on different filesystems: `{}` and `{}`",
        staging.display(),
        quarantine.display()
    )]
    CrossDevice { staging: PathBuf, quarantine: PathBuf },
    #[error("retained staging-wrapper namespace mismatch: staging={staging}, quarantine={quarantine}")]
    NamespaceMismatch {
        staging: &'static str,
        quarantine: &'static str,
    },
    #[error("staging-wrapper exchange reported success without moving either exact wrapper")]
    ReportedSuccessWithoutMove,
    #[error("retained active-reblit staging rotation lock is poisoned")]
    AttemptLockPoisoned,
    #[error("no retained active-reblit staging rotation was reserved")]
    AttemptMissing,
    #[error("an active-reblit staging rotation is already reserved")]
    AttemptAlreadyReserved,
    #[error("park the retained active previous-state slot before the live /usr exchange")]
    ActivePreviousSlotParking(
        #[source]
        Box<crate::transition_identity::active_previous_slot_parking::RetainedActivePreviousSlotParkingFailure>,
    ),
    #[error("exchange retained staging wrapper at `{}`", path.display())]
    Exchange {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("staging-wrapper preflight failed ({primary}) and exact reconciliation failed ({reconciliation})")]
    PreflightReconciliationFailed {
        primary: Box<StagingWrapperRotationError>,
        reconciliation: Box<StagingWrapperRotationError>,
    },
    #[cfg(test)]
    #[error("injected retained staging-wrapper fault at {point:?}")]
    InjectedFault {
        point: RetainedStagingWrapperRotationFaultPoint,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WrapperLayout {
    OriginalStaged,
    OriginalQuarantined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NamedWrapper {
    Absent,
    Original,
    Replacement,
    Foreign,
}

impl NamedWrapper {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Original => "original",
            Self::Replacement => "replacement",
            Self::Foreign => "foreign",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum OriginalWrapperModePolicy {
    NormalizeBeforeJournal,
    RequirePrivateWithJournal,
}

/// Retains both wrapper inodes and both parent namespaces until the exchange
/// and every durability barrier have been proven.
#[derive(Debug)]
pub(in crate::transition_identity) struct RetainedStagingWrapperRotation {
    pub(super) roots: RetainedDirectory,
    pub(super) quarantine: RetainedDirectory,
    pub(super) original: RetainedDirectory,
    pub(super) replacement: RetainedDirectory,
    pub(super) quarantine_name: CString,
    pub(super) quarantine_path: PathBuf,
}
