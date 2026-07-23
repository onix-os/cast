//! Exact transfer of a retained state-slot marker between exchanged wrappers.

use super::{
    ArchivedCandidateError, PreparationOutcome, RetainedArchivedCandidateAttempt,
    RetainedArchivedCandidateMoveFaultPoint, before_slot_marker_location, checkpoint, identity,
};
use crate::{
    Installation,
    linux_fs::renameat2_noreplace_once,
    transition_identity::{StatefulTreeIdentity, state_slot_marker::NamedMarkerState},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MarkerLocation {
    Candidate,
    Displaced,
}

impl MarkerLocation {
    fn opposite(self) -> Self {
        match self {
            Self::Candidate => Self::Displaced,
            Self::Displaced => Self::Candidate,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Displaced => "displaced-staging",
        }
    }
}

impl StatefulTreeIdentity {
    pub(super) fn retained_slot_marker_location(
        &self,
        attempt: &RetainedArchivedCandidateAttempt,
    ) -> Result<MarkerLocation, ArchivedCandidateError> {
        let candidate = attempt
            .slot_marker
            .named_state(&attempt.candidate_wrapper)
            .map_err(|source| identity("probe candidate-wrapper state-slot marker", source.into()))?;
        let displaced = attempt
            .slot_marker
            .named_state(&attempt.displaced_staging_wrapper)
            .map_err(|source| identity("probe displaced-wrapper state-slot marker", source.into()))?;
        match (candidate, displaced) {
            (NamedMarkerState::Exact, NamedMarkerState::Absent) => Ok(MarkerLocation::Candidate),
            (NamedMarkerState::Absent, NamedMarkerState::Exact) => Ok(MarkerLocation::Displaced),
            _ => Err(ArchivedCandidateError::SlotMarkerNamespaceMismatch {
                candidate: marker_state_name(candidate),
                displaced: marker_state_name(displaced),
            }),
        }
    }

    pub(super) fn ensure_slot_marker_location(
        &self,
        installation: &Installation,
        attempt: &mut RetainedArchivedCandidateAttempt,
        desired: MarkerLocation,
    ) -> Result<(), (PreparationOutcome, ArchivedCandidateError)> {
        before_slot_marker_location();
        let current = self
            .retained_slot_marker_location(attempt)
            .map_err(|source| (PreparationOutcome::Ambiguous, source))?;
        if attempt.marker_transfer_pending {
            if current != desired {
                return Err((
                    PreparationOutcome::Ambiguous,
                    ArchivedCandidateError::SlotMarkerNamespaceMismatch {
                        candidate: current.as_str(),
                        displaced: desired.as_str(),
                    },
                ));
            }
            return self
                .finish_slot_marker_transfer(installation, attempt, desired)
                .map_err(|source| (PreparationOutcome::Applied, source));
        }
        if current == desired {
            attempt.marker_transfer_pending = true;
            if desired == MarkerLocation::Candidate {
                attempt.rearchive_preparation_applied = true;
            }
            return self
                .finish_slot_marker_transfer(installation, attempt, desired)
                .map_err(|source| (PreparationOutcome::Applied, source));
        }

        checkpoint(RetainedArchivedCandidateMoveFaultPoint::BeforeSlotMarkerTransfer)
            .map_err(|source| (PreparationOutcome::NotApplied, source))?;
        let (source_wrapper, destination_wrapper) = match desired {
            MarkerLocation::Candidate => (&attempt.displaced_staging_wrapper, &attempt.candidate_wrapper),
            MarkerLocation::Displaced => (&attempt.candidate_wrapper, &attempt.displaced_staging_wrapper),
        };
        let syscall_result = renameat2_noreplace_once(
            &source_wrapper.file,
            attempt.slot_marker.name(),
            &destination_wrapper.file,
            attempt.slot_marker.name(),
        )
        .map_err(|source| ArchivedCandidateError::Io {
            operation: "transfer exact retained state-slot marker",
            path: destination_wrapper
                .path
                .join(attempt.slot_marker.name().to_string_lossy().as_ref()),
            source,
        })
        .and_then(|()| checkpoint(RetainedArchivedCandidateMoveFaultPoint::AfterSlotMarkerTransfer));

        match self
            .retained_slot_marker_location(attempt)
            .map_err(|source| (PreparationOutcome::Ambiguous, source))?
        {
            location if location == current => Err((
                PreparationOutcome::NotApplied,
                match syscall_result {
                    Err(source) => source,
                    Ok(()) => ArchivedCandidateError::SlotMarkerTransferReportedSuccessWithoutMove,
                },
            )),
            location if location == desired => {
                attempt.marker_transfer_pending = true;
                if desired == MarkerLocation::Candidate {
                    attempt.rearchive_preparation_applied = true;
                }
                self.finish_slot_marker_transfer(installation, attempt, desired)
                    .map_err(|source| (PreparationOutcome::Applied, source))
            }
            location => Err((
                PreparationOutcome::Ambiguous,
                ArchivedCandidateError::SlotMarkerNamespaceMismatch {
                    candidate: location.as_str(),
                    displaced: desired.opposite().as_str(),
                },
            )),
        }
    }

    fn finish_slot_marker_transfer(
        &self,
        installation: &Installation,
        attempt: &mut RetainedArchivedCandidateAttempt,
        desired: MarkerLocation,
    ) -> Result<(), ArchivedCandidateError> {
        attempt
            .slot_marker
            .sync()
            .map_err(|source| identity("sync exact retained state-slot marker", source.into()))?;
        checkpoint(RetainedArchivedCandidateMoveFaultPoint::SlotMarkerParentSync)?;
        attempt
            .candidate_wrapper
            .sync("sync candidate wrapper after state-slot marker transfer")
            .map_err(|source| identity("sync candidate wrapper after state-slot marker transfer", source))?;
        attempt
            .displaced_staging_wrapper
            .sync("sync displaced wrapper after state-slot marker transfer")
            .map_err(|source| identity("sync displaced wrapper after state-slot marker transfer", source))?;
        checkpoint(RetainedArchivedCandidateMoveFaultPoint::FinalSlotMarkerRevalidation)?;
        self.require_no_journal()
            .map_err(|source| identity("recheck journal after state-slot marker transfer", source))?;
        self.revalidate_base(installation, attempt)?;
        if self.retained_slot_marker_location(attempt)? != desired {
            return Err(ArchivedCandidateError::SlotMarkerTransferReportedSuccessWithoutMove);
        }
        attempt.marker_transfer_pending = false;
        Ok(())
    }
}

fn marker_state_name(state: NamedMarkerState) -> &'static str {
    match state {
        NamedMarkerState::Absent => "absent",
        NamedMarkerState::Exact => "exact",
        NamedMarkerState::Foreign => "foreign",
    }
}
