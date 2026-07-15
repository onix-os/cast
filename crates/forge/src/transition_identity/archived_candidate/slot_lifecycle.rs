//! Parking and restoration of the staging wrapper displaced by activation.

use std::{ffi::CString, path::PathBuf};

use super::{
    ArchivedCandidateError, CandidateLayout, DisplacedSlotLocation, PreparationOutcome,
    RetainedArchivedCandidateAttempt, RetainedArchivedCandidateMoveFaultPoint, canonical_path, checkpoint, identity,
    require_attempt_state,
};
use crate::{
    Installation,
    linux_fs::renameat2_noreplace_once,
    state,
    transition_identity::{MAX_PREVIOUS_SLOT_PARKING_CANDIDATES, RetainedDirectoryWitness, StatefulTreeIdentity},
    transition_journal::QuarantineName,
};

impl StatefulTreeIdentity {
    pub(crate) fn retire_displaced_archived_candidate_slot(
        &self,
        installation: &Installation,
        candidate: state::Id,
    ) -> Result<(), ArchivedCandidateError> {
        let mut retained = self
            .archived_candidate_attempt
            .lock()
            .map_err(|_| ArchivedCandidateError::AttemptLockPoisoned)?;
        let attempt = retained.as_mut().ok_or(ArchivedCandidateError::AttemptMissing {
            state: i32::from(candidate),
        })?;
        require_attempt_state(attempt, candidate)?;

        if attempt.parking_name.is_some()
            && matches!(self.displaced_slot_location(attempt), Ok(DisplacedSlotLocation::Parked))
        {
            self.finish_displaced_slot_retirement(installation, attempt)?;
            *retained = None;
            return Ok(());
        }

        self.require_retirement_layout(installation, attempt)?;
        self.select_parking_name(attempt)?;
        let parking_name = attempt.parking_name.as_ref().expect("parking name was selected");
        match self.displaced_slot_location(attempt)? {
            DisplacedSlotLocation::Canonical => {
                checkpoint(RetainedArchivedCandidateMoveFaultPoint::BeforeDisplacedSlotRetire)?;
                super::before_retired_slot_move();
                let syscall_result = renameat2_noreplace_once(
                    &attempt.roots.file,
                    &attempt.state_name,
                    &attempt.roots.file,
                    parking_name,
                )
                .map_err(|source| ArchivedCandidateError::Io {
                    operation: "retire exact displaced staging wrapper",
                    path: parking_path(attempt),
                    source,
                })
                .and_then(|()| checkpoint(RetainedArchivedCandidateMoveFaultPoint::AfterDisplacedSlotRetire));
                if self.displaced_slot_location(attempt)? != DisplacedSlotLocation::Parked {
                    return Err(match syscall_result {
                        Err(source) => source,
                        Ok(()) => ArchivedCandidateError::DisplacedSlotRetireReportedSuccessWithoutMove,
                    });
                }
            }
            DisplacedSlotLocation::Parked => {}
        }

        self.finish_displaced_slot_retirement(installation, attempt)?;
        *retained = None;
        Ok(())
    }

    pub(super) fn restore_displaced_slot_if_parked(
        &self,
        installation: &Installation,
        attempt: &mut RetainedArchivedCandidateAttempt,
    ) -> Result<(), (PreparationOutcome, ArchivedCandidateError)> {
        let Some(parking_name) = attempt.parking_name.as_ref() else {
            return Ok(());
        };
        let location = self
            .displaced_slot_location(attempt)
            .map_err(|source| (PreparationOutcome::Ambiguous, source))?;
        if attempt.displaced_restore_pending {
            if location != DisplacedSlotLocation::Canonical {
                return Err((
                    PreparationOutcome::Ambiguous,
                    ArchivedCandidateError::DisplacedSlotRestoreReportedSuccessWithoutMove,
                ));
            }
            return self
                .finish_displaced_slot_restore(installation, attempt)
                .map_err(|source| (PreparationOutcome::Applied, source));
        }
        if location == DisplacedSlotLocation::Canonical {
            attempt.rearchive_preparation_applied = true;
            return Ok(());
        }

        checkpoint(RetainedArchivedCandidateMoveFaultPoint::BeforeDisplacedSlotRestore)
            .map_err(|source| (PreparationOutcome::NotApplied, source))?;
        let syscall_result = renameat2_noreplace_once(
            &attempt.roots.file,
            parking_name,
            &attempt.roots.file,
            &attempt.state_name,
        )
        .map_err(|source| ArchivedCandidateError::Io {
            operation: "restore displaced staging wrapper before candidate rearchive",
            path: canonical_path(attempt),
            source,
        })
        .and_then(|()| checkpoint(RetainedArchivedCandidateMoveFaultPoint::AfterDisplacedSlotRestore));
        match self
            .displaced_slot_location(attempt)
            .map_err(|source| (PreparationOutcome::Ambiguous, source))?
        {
            DisplacedSlotLocation::Parked => Err((
                PreparationOutcome::NotApplied,
                syscall_result.unwrap_err_or(ArchivedCandidateError::DisplacedSlotRestoreReportedSuccessWithoutMove),
            )),
            DisplacedSlotLocation::Canonical => {
                attempt.displaced_restore_pending = true;
                attempt.rearchive_preparation_applied = true;
                self.finish_displaced_slot_restore(installation, attempt)
                    .map_err(|source| (PreparationOutcome::Applied, source))
            }
        }
    }

    fn finish_displaced_slot_restore(
        &self,
        installation: &Installation,
        attempt: &mut RetainedArchivedCandidateAttempt,
    ) -> Result<(), ArchivedCandidateError> {
        checkpoint(RetainedArchivedCandidateMoveFaultPoint::RootsAfterDisplacedSlotRestoreSync)?;
        attempt
            .roots
            .sync("sync roots after displaced staging wrapper restoration")
            .map_err(|source| identity("sync roots after displaced staging wrapper restoration", source))?;
        self.revalidate_base(installation, attempt)?;
        if self.displaced_slot_location(attempt)? != DisplacedSlotLocation::Canonical {
            return Err(ArchivedCandidateError::DisplacedSlotRestoreReportedSuccessWithoutMove);
        }
        attempt.displaced_restore_pending = false;
        Ok(())
    }

    fn require_retirement_layout(
        &self,
        installation: &Installation,
        attempt: &RetainedArchivedCandidateAttempt,
    ) -> Result<(), ArchivedCandidateError> {
        self.require_no_journal()
            .map_err(|source| identity("check journal before displaced-slot retirement", source))?;
        self.revalidate_base(installation, attempt)?;
        let actual = self.wrapper_layout(attempt)?;
        if actual != CandidateLayout::Staged {
            return Err(ArchivedCandidateError::UnexpectedLayout {
                direction: "retire-displaced-slot",
                expected: CandidateLayout::Staged.as_str(),
                actual: actual.as_str(),
            });
        }
        attempt
            .candidate_wrapper
            .require_exact_entries(&[])
            .map_err(|source| identity("require empty staging wrapper before retirement", source))?;
        attempt
            .slot_marker
            .require_named(&attempt.displaced_staging_wrapper)
            .map_err(|source| identity("authenticate displaced-wrapper state-slot marker", source.into()))?;
        attempt
            .displaced_staging_wrapper
            .require_exact_entries(&[attempt.slot_marker.name_bytes()])
            .map_err(|source| identity("require marker-only displaced wrapper before retirement", source))
    }

    fn select_parking_name(
        &self,
        attempt: &mut RetainedArchivedCandidateAttempt,
    ) -> Result<(), ArchivedCandidateError> {
        if let Some(name) = attempt.parking_name.as_ref() {
            let path = attempt.roots.path.join(name.to_string_lossy().as_ref());
            if !attempt
                .roots
                .child_name_exists(name, path)
                .map_err(|source| identity("probe selected archived-candidate parking name", source))?
            {
                return Ok(());
            }
        }
        attempt.parking_name = None;
        for index in 0..MAX_PREVIOUS_SLOT_PARKING_CANDIDATES {
            let name = parking_name(attempt.state, self.candidate.marker.token().as_str(), index)
                .map_err(ArchivedCandidateError::InvalidParkingName)?;
            let path = attempt.roots.path.join(name.to_string_lossy().as_ref());
            if !attempt
                .roots
                .child_name_exists(&name, path)
                .map_err(|source| identity("scan archived-candidate parking names", source))?
            {
                attempt.parking_name = Some(name);
                return Ok(());
            }
        }
        Err(ArchivedCandidateError::ParkingExhausted {
            state: i32::from(attempt.state),
            limit: MAX_PREVIOUS_SLOT_PARKING_CANDIDATES,
        })
    }

    fn displaced_slot_location(
        &self,
        attempt: &RetainedArchivedCandidateAttempt,
    ) -> Result<DisplacedSlotLocation, ArchivedCandidateError> {
        let parking_name = attempt
            .parking_name
            .as_ref()
            .ok_or(ArchivedCandidateError::ParkingExhausted {
                state: i32::from(attempt.state),
                limit: MAX_PREVIOUS_SLOT_PARKING_CANDIDATES,
            })?;
        let canonical = attempt
            .roots
            .open_optional_child(&attempt.state_name, canonical_path(attempt))
            .map_err(|source| identity("open displaced wrapper canonical name", source))?;
        let parking = attempt
            .roots
            .open_optional_child(parking_name, parking_path(attempt))
            .map_err(|source| identity("open displaced wrapper parking name", source))?;
        let canonical_state = named_state(canonical.as_ref().map(|entry| entry.witness), attempt);
        let parking_state = named_state(parking.as_ref().map(|entry| entry.witness), attempt);
        match (canonical_state, parking_state) {
            ("exact", "absent" | "foreign") => Ok(DisplacedSlotLocation::Canonical),
            ("absent", "exact") => Ok(DisplacedSlotLocation::Parked),
            _ => Err(ArchivedCandidateError::DisplacedSlotNamespaceMismatch {
                canonical_path: canonical_path(attempt),
                canonical: canonical_state,
                parking_path: parking_path(attempt),
                parking: parking_state,
            }),
        }
    }

    fn finish_displaced_slot_retirement(
        &self,
        installation: &Installation,
        attempt: &RetainedArchivedCandidateAttempt,
    ) -> Result<(), ArchivedCandidateError> {
        if self.displaced_slot_location(attempt)? != DisplacedSlotLocation::Parked {
            return Err(ArchivedCandidateError::DisplacedSlotRetireReportedSuccessWithoutMove);
        }
        checkpoint(RetainedArchivedCandidateMoveFaultPoint::RootsAfterDisplacedSlotRetireSync)?;
        attempt
            .roots
            .sync("sync roots after displaced staging wrapper retirement")
            .map_err(|source| identity("sync roots after displaced staging wrapper retirement", source))?;
        checkpoint(RetainedArchivedCandidateMoveFaultPoint::FinalDisplacedSlotRetirementRevalidation)?;
        self.require_no_journal()
            .map_err(|source| identity("recheck journal after displaced-slot retirement", source))?;
        self.revalidate_base(installation, attempt)?;
        attempt
            .candidate_wrapper
            .require_exact_entries(&[])
            .map_err(|source| identity("revalidate empty staging wrapper after retirement", source))?;
        attempt
            .slot_marker
            .require_named(&attempt.displaced_staging_wrapper)
            .map_err(|source| identity("revalidate parked state-slot marker", source.into()))?;
        if self.displaced_slot_location(attempt)? == DisplacedSlotLocation::Parked {
            Ok(())
        } else {
            Err(ArchivedCandidateError::DisplacedSlotRetireReportedSuccessWithoutMove)
        }
    }
}

pub(in crate::transition_identity) fn parking_name(
    state: state::Id,
    candidate_token: &str,
    index: usize,
) -> Result<CString, crate::transition_journal::CodecError> {
    let name = QuarantineName::parse(format!(
        ".archived-candidate-slot-{}-{candidate_token}-{index}",
        i32::from(state)
    ))?;
    Ok(CString::new(name.as_str()).expect("validated archived-candidate parking name contains no NUL"))
}

fn parking_path(attempt: &RetainedArchivedCandidateAttempt) -> PathBuf {
    attempt.roots.path.join(
        attempt
            .parking_name
            .as_ref()
            .expect("parking path requires selected name")
            .to_string_lossy()
            .as_ref(),
    )
}

fn named_state(witness: Option<RetainedDirectoryWitness>, attempt: &RetainedArchivedCandidateAttempt) -> &'static str {
    match witness {
        None => "absent",
        Some(witness) if witness == attempt.displaced_staging_wrapper.witness => "exact",
        Some(_) => "foreign",
    }
}

trait ResultErrorOr<T, E> {
    fn unwrap_err_or(self, fallback: E) -> E;
}

impl<T, E> ResultErrorOr<T, E> for Result<T, E> {
    fn unwrap_err_or(self, fallback: E) -> E {
        match self {
            Err(source) => source,
            Ok(_) => fallback,
        }
    }
}
