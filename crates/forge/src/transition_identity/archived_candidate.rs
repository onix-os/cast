//! Retained namespace moves for activating and re-archiving an existing state.
//!
//! The fixed staging wrapper already exists, so replacing it with a pathname
//! rename would destroy an inode after a mutable final-component check.  This
//! module instead exchanges the archived-state and staging wrappers exactly
//! once, retains both inodes, and reconciles their names after every result.

mod slot_lifecycle;
mod slot_marker_transfer;

use std::{
    ffi::{CStr, CString},
    io,
    path::PathBuf,
};

use thiserror::Error;

use super::{
    ROOTS_RELATIVE, RetainedDirectory, RetainedDirectoryWitness, StatefulTreeIdentity, canonical_state_name,
    open_optional_retained_tree, state_slot_marker::RetainedStateSlotMarker,
};
use crate::{Installation, linux_fs::renameat2_exchange_once, state};

use self::slot_marker_transfer::MarkerLocation;

pub(super) use slot_lifecycle::parking_name as archived_candidate_parking_name;

const STAGING_NAME: &CStr = c"staging";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedArchivedCandidateMoveOutcome {
    NotApplied,
    RearchivePreparationApplied,
    Applied,
    Ambiguous,
}

#[derive(Debug, Error)]
#[error("retained archived-candidate move outcome is {outcome:?}")]
pub(crate) struct RetainedArchivedCandidateMoveFailure {
    outcome: RetainedArchivedCandidateMoveOutcome,
    #[source]
    source: ArchivedCandidateError,
}

impl RetainedArchivedCandidateMoveFailure {
    pub(crate) fn outcome(&self) -> RetainedArchivedCandidateMoveOutcome {
        self.outcome
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedArchivedCandidateMoveFaultPoint {
    CandidatePreSync,
    CandidateWrapperSync,
    DisplacedWrapperSync,
    BeforeExchange,
    AfterExchange,
    CandidatePostSync,
    RootsParentSync,
    FinalRevalidation,
    BeforeDisplacedSlotRestore,
    AfterDisplacedSlotRestore,
    RootsAfterDisplacedSlotRestoreSync,
    BeforeSlotMarkerTransfer,
    AfterSlotMarkerTransfer,
    SlotMarkerParentSync,
    FinalSlotMarkerRevalidation,
    BeforeDisplacedSlotRetire,
    AfterDisplacedSlotRetire,
    RootsAfterDisplacedSlotRetireSync,
    FinalDisplacedSlotRetirementRevalidation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MoveDirection {
    Stage,
    Rearchive,
}

impl MoveDirection {
    fn before(self) -> CandidateLayout {
        match self {
            Self::Stage => CandidateLayout::Archived,
            Self::Rearchive => CandidateLayout::Staged,
        }
    }

    fn after(self) -> CandidateLayout {
        match self {
            Self::Stage => CandidateLayout::Staged,
            Self::Rearchive => CandidateLayout::Archived,
        }
    }

    fn marker_before(self) -> MarkerLocation {
        MarkerLocation::Candidate
    }

    fn marker_after(self) -> MarkerLocation {
        match self {
            Self::Stage => MarkerLocation::Displaced,
            Self::Rearchive => MarkerLocation::Candidate,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Stage => "stage",
            Self::Rearchive => "rearchive",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateLayout {
    Archived,
    Staged,
}

impl CandidateLayout {
    fn as_str(self) -> &'static str {
        match self {
            Self::Archived => "candidate-archived",
            Self::Staged => "candidate-staged",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplacedSlotLocation {
    Canonical,
    Parked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PreparationOutcome {
    NotApplied,
    Applied,
    Ambiguous,
}

#[derive(Debug)]
pub(super) struct RetainedArchivedCandidateAttempt {
    state: state::Id,
    state_name: CString,
    parking_name: Option<CString>,
    roots: RetainedDirectory,
    candidate_wrapper: RetainedDirectory,
    displaced_staging_wrapper: RetainedDirectory,
    slot_marker: RetainedStateSlotMarker,
    displaced_restore_pending: bool,
    marker_transfer_pending: bool,
    rearchive_preparation_applied: bool,
}

#[derive(Debug, Error)]
pub(crate) enum ArchivedCandidateError {
    #[error("{operation} while retaining the archived candidate: {source}")]
    Identity {
        operation: &'static str,
        #[source]
        source: Box<super::Error>,
    },
    #[error("{operation} in retained archived-candidate namespace at `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("retained archived-candidate attempt lock is poisoned")]
    AttemptLockPoisoned,
    #[error("no retained archived-candidate attempt exists for state {state}")]
    AttemptMissing { state: i32 },
    #[error("retained archived-candidate attempt is for state {expected}, not {actual}")]
    AttemptChanged { expected: i32, actual: i32 },
    #[error(
        "archived candidate wrappers are on different filesystems: state `{}` and staging `{}`",
        state.display(),
        staging.display()
    )]
    CrossDevice { state: PathBuf, staging: PathBuf },
    #[error("retained archived-candidate namespace mismatch: state={state_name}, staging={staging_name}")]
    NamespaceMismatch {
        state_name: &'static str,
        staging_name: &'static str,
    },
    #[error("{direction} retained archived-candidate move expected {expected}, found {actual}")]
    UnexpectedLayout {
        direction: &'static str,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("{direction} retained archived-candidate exchange reported success without moving either exact wrapper")]
    ReportedSuccessWithoutMove { direction: &'static str },
    #[error(
        "{direction} archived-candidate preflight failed ({primary}) and exact reconciliation failed ({reconciliation})"
    )]
    PreflightReconciliationFailed {
        direction: &'static str,
        primary: Box<ArchivedCandidateError>,
        reconciliation: Box<ArchivedCandidateError>,
    },
    #[error(
        "{direction} archived-candidate preflight failed ({primary}) after the exact exchange applied, and its durability suffix failed ({finish})"
    )]
    AppliedAfterPreflightFailure {
        direction: &'static str,
        primary: Box<ArchivedCandidateError>,
        finish: Box<ArchivedCandidateError>,
    },
    #[error("construct a bounded archived-candidate parking name")]
    InvalidParkingName(#[source] crate::transition_journal::CodecError),
    #[error("all {limit} archived-candidate parking names are occupied for state {state}")]
    ParkingExhausted { state: i32, limit: usize },
    #[error(
        "retained displaced staging wrapper namespace mismatch: canonical `{}` is {canonical}, parking `{}` is {parking}",
        canonical_path.display(),
        parking_path.display()
    )]
    DisplacedSlotNamespaceMismatch {
        canonical_path: PathBuf,
        canonical: &'static str,
        parking_path: PathBuf,
        parking: &'static str,
    },
    #[error("displaced staging wrapper publication reported success without restoring its canonical state name")]
    DisplacedSlotRestoreReportedSuccessWithoutMove,
    #[error("displaced staging wrapper retirement reported success without moving its exact canonical name")]
    DisplacedSlotRetireReportedSuccessWithoutMove,
    #[error("retained state-slot marker namespace mismatch: candidate={candidate}, displaced-staging={displaced}")]
    SlotMarkerNamespaceMismatch {
        candidate: &'static str,
        displaced: &'static str,
    },
    #[error("state-slot marker transfer reported success without moving the exact retained marker")]
    SlotMarkerTransferReportedSuccessWithoutMove,
    #[cfg(test)]
    #[error("injected retained archived-candidate fault at {point:?}")]
    InjectedFault {
        point: RetainedArchivedCandidateMoveFaultPoint,
    },
}

impl StatefulTreeIdentity {
    pub(crate) fn stage_archived_candidate(
        &self,
        installation: &Installation,
        candidate: state::Id,
    ) -> Result<(), RetainedArchivedCandidateMoveFailure> {
        self.move_archived_candidate(installation, candidate, MoveDirection::Stage)
    }

    pub(crate) fn rearchive_archived_candidate(
        &self,
        installation: &Installation,
        candidate: state::Id,
    ) -> Result<(), RetainedArchivedCandidateMoveFailure> {
        self.move_archived_candidate(installation, candidate, MoveDirection::Rearchive)
    }

    pub(crate) fn finish_applied_archived_candidate_stage(
        &self,
        installation: &Installation,
        candidate: state::Id,
    ) -> Result<(), ArchivedCandidateError> {
        self.finish_applied_archived_candidate_move(installation, candidate, MoveDirection::Stage)
    }

    pub(crate) fn finish_applied_archived_candidate_rearchive(
        &self,
        installation: &Installation,
        candidate: state::Id,
    ) -> Result<(), ArchivedCandidateError> {
        self.finish_applied_archived_candidate_move(installation, candidate, MoveDirection::Rearchive)
    }

    fn move_archived_candidate(
        &self,
        installation: &Installation,
        candidate: state::Id,
        direction: MoveDirection,
    ) -> Result<(), RetainedArchivedCandidateMoveFailure> {
        let not_applied = |source| RetainedArchivedCandidateMoveFailure {
            outcome: RetainedArchivedCandidateMoveOutcome::NotApplied,
            source,
        };
        let applied = |source| RetainedArchivedCandidateMoveFailure {
            outcome: RetainedArchivedCandidateMoveOutcome::Applied,
            source,
        };
        let ambiguous = |source| RetainedArchivedCandidateMoveFailure {
            outcome: RetainedArchivedCandidateMoveOutcome::Ambiguous,
            source,
        };
        self.require_no_journal()
            .map_err(|source| not_applied(identity("check journal before archived-candidate move", source)))?;
        installation
            .revalidate_root_directory()
            .map_err(super::Error::from)
            .map_err(|source| not_applied(identity("revalidate installation root", source)))?;

        let mut retained = self
            .archived_candidate_attempt
            .lock()
            .map_err(|_| not_applied(ArchivedCandidateError::AttemptLockPoisoned))?;
        if retained.is_none() {
            if direction == MoveDirection::Rearchive {
                return Err(not_applied(ArchivedCandidateError::AttemptMissing {
                    state: i32::from(candidate),
                }));
            }
            *retained = Some(
                self.create_archived_candidate_attempt(installation, candidate)
                    .map_err(not_applied)?,
            );
        }
        let attempt = retained.as_mut().expect("archived-candidate attempt was established");
        require_attempt_state(attempt, candidate).map_err(not_applied)?;

        if direction == MoveDirection::Rearchive {
            if let Err((outcome, source)) = self.restore_displaced_slot_if_parked(installation, attempt) {
                return Err(preparation_failure(
                    outcome,
                    attempt.rearchive_preparation_applied,
                    source,
                ));
            }
            self.revalidate_base(installation, attempt).map_err(|source| {
                preparation_failure(
                    PreparationOutcome::NotApplied,
                    attempt.rearchive_preparation_applied,
                    source,
                )
            })?;
            let marker_location = self.retained_slot_marker_location(attempt).map_err(ambiguous)?;
            self.require_move_layout(attempt, CandidateLayout::Staged, marker_location)
                .map_err(|source| {
                    preparation_failure(
                        PreparationOutcome::NotApplied,
                        attempt.rearchive_preparation_applied,
                        source,
                    )
                })?;
            if let Err((outcome, source)) =
                self.ensure_slot_marker_location(installation, attempt, MarkerLocation::Candidate)
            {
                return Err(preparation_failure(
                    outcome,
                    attempt.rearchive_preparation_applied,
                    source,
                ));
            }
        }

        let preflight = (|| -> Result<(), ArchivedCandidateError> {
            self.revalidate_base(installation, attempt)?;
            self.require_move_layout(attempt, direction.before(), direction.marker_before())?;
            checkpoint(RetainedArchivedCandidateMoveFaultPoint::CandidatePreSync)?;
            self.candidate
                .store
                .sync_retained_tree()
                .map_err(|source| identity("sync exact archived candidate before wrapper exchange", source.into()))?;
            checkpoint(RetainedArchivedCandidateMoveFaultPoint::CandidateWrapperSync)?;
            attempt
                .candidate_wrapper
                .sync("sync archived-candidate wrapper before exchange")
                .map_err(|source| identity("sync archived-candidate wrapper before exchange", source))?;
            checkpoint(RetainedArchivedCandidateMoveFaultPoint::DisplacedWrapperSync)?;
            attempt
                .displaced_staging_wrapper
                .sync("sync staging wrapper before archived-candidate exchange")
                .map_err(|source| identity("sync staging wrapper before archived-candidate exchange", source))?;
            attempt
                .roots
                .sync("sync roots parent before archived-candidate exchange")
                .map_err(|source| identity("sync roots parent before archived-candidate exchange", source))?;

            before_exchange();
            self.require_no_journal()
                .map_err(|source| identity("recheck journal before archived-candidate exchange", source))?;
            self.revalidate_base(installation, attempt)?;
            self.require_move_layout(attempt, direction.before(), direction.marker_before())?;
            checkpoint(RetainedArchivedCandidateMoveFaultPoint::BeforeExchange)
        })();
        if let Err(source) = preflight {
            let result = self.reconcile_preflight_failure(installation, attempt, direction, source);
            if result.is_ok() && direction == MoveDirection::Rearchive {
                *retained = None;
            }
            return result;
        }

        let syscall_result = renameat2_exchange_once(
            &attempt.roots.file,
            &attempt.state_name,
            &attempt.roots.file,
            STAGING_NAME,
        )
        .map_err(|source| ArchivedCandidateError::Io {
            operation: "exchange archived candidate and staging wrappers",
            path: attempt.roots.path.clone(),
            source,
        })
        .and_then(|()| checkpoint(RetainedArchivedCandidateMoveFaultPoint::AfterExchange));

        let observed = self.wrapper_layout(attempt).map_err(ambiguous)?;
        if observed == direction.before() {
            let source = match syscall_result {
                Err(source) => source,
                Ok(()) => ArchivedCandidateError::ReportedSuccessWithoutMove {
                    direction: direction.as_str(),
                },
            };
            return Err(not_applied(source));
        }
        if observed != direction.after() {
            return Err(ambiguous(ArchivedCandidateError::UnexpectedLayout {
                direction: direction.as_str(),
                expected: direction.after().as_str(),
                actual: observed.as_str(),
            }));
        }

        let finish = self.finish_move(installation, attempt, direction);
        if finish.is_ok() && direction == MoveDirection::Rearchive {
            *retained = None;
        }
        finish.map_err(applied)
    }

    fn create_archived_candidate_attempt(
        &self,
        installation: &Installation,
        candidate: state::Id,
    ) -> Result<RetainedArchivedCandidateAttempt, ArchivedCandidateError> {
        let state_name = canonical_state_name(candidate)
            .map_err(|source| identity("validate archived candidate state name", source))?;
        let roots_path = installation.root_path("");
        let roots = RetainedDirectory::open_beneath(installation.root_directory(), ROOTS_RELATIVE, roots_path.clone())
            .map_err(|source| identity("retain state roots directory", source))?;
        let candidate_wrapper = roots
            .open_child(&state_name, installation.root_path(candidate.to_string()))
            .map_err(|source| identity("retain archived-candidate wrapper", source))?;
        let displaced_staging_wrapper = roots
            .open_child(STAGING_NAME, installation.staging_dir())
            .map_err(|source| identity("retain fixed staging wrapper", source))?;
        if roots.witness.device != candidate_wrapper.witness.device
            || roots.witness.device != displaced_staging_wrapper.witness.device
        {
            return Err(ArchivedCandidateError::CrossDevice {
                state: candidate_wrapper.path.clone(),
                staging: displaced_staging_wrapper.path.clone(),
            });
        }
        let slot_marker = RetainedStateSlotMarker::prepare(
            &candidate_wrapper,
            candidate,
            &self.candidate.store,
            &self.candidate.marker,
        )
        .map_err(|source| identity("prepare archived-candidate state-slot marker", source.into()))?;
        let attempt = RetainedArchivedCandidateAttempt {
            state: candidate,
            state_name,
            parking_name: None,
            roots,
            candidate_wrapper,
            displaced_staging_wrapper,
            slot_marker,
            displaced_restore_pending: false,
            marker_transfer_pending: false,
            rearchive_preparation_applied: false,
        };
        self.revalidate_base(installation, &attempt)?;
        self.require_move_layout(&attempt, CandidateLayout::Archived, MarkerLocation::Candidate)?;
        Ok(attempt)
    }

    fn finish_applied_archived_candidate_move(
        &self,
        installation: &Installation,
        candidate: state::Id,
        direction: MoveDirection,
    ) -> Result<(), ArchivedCandidateError> {
        let mut retained = self
            .archived_candidate_attempt
            .lock()
            .map_err(|_| ArchivedCandidateError::AttemptLockPoisoned)?;
        let attempt = retained.as_mut().ok_or(ArchivedCandidateError::AttemptMissing {
            state: i32::from(candidate),
        })?;
        require_attempt_state(attempt, candidate)?;
        self.revalidate_base(installation, attempt)?;
        self.finish_move(installation, attempt, direction)?;
        if direction == MoveDirection::Rearchive {
            *retained = None;
        }
        Ok(())
    }

    fn finish_move(
        &self,
        installation: &Installation,
        attempt: &mut RetainedArchivedCandidateAttempt,
        direction: MoveDirection,
    ) -> Result<(), ArchivedCandidateError> {
        let marker_location = self.retained_slot_marker_location(attempt)?;
        self.require_move_layout(attempt, direction.after(), marker_location)?;
        self.ensure_slot_marker_location(installation, attempt, direction.marker_after())
            .map_err(|(_, source)| source)?;
        checkpoint(RetainedArchivedCandidateMoveFaultPoint::CandidatePostSync)?;
        self.candidate
            .store
            .sync_retained_tree()
            .map_err(|source| identity("sync archived candidate after wrapper exchange", source.into()))?;
        checkpoint(RetainedArchivedCandidateMoveFaultPoint::RootsParentSync)?;
        attempt
            .roots
            .sync("sync roots parent after archived-candidate exchange")
            .map_err(|source| identity("sync roots parent after archived-candidate exchange", source))?;
        checkpoint(RetainedArchivedCandidateMoveFaultPoint::FinalRevalidation)?;
        self.require_no_journal()
            .map_err(|source| identity("recheck journal after archived-candidate exchange", source))?;
        self.revalidate_base(installation, attempt)?;
        self.require_move_layout(attempt, direction.after(), direction.marker_after())
    }

    fn reconcile_preflight_failure(
        &self,
        installation: &Installation,
        attempt: &mut RetainedArchivedCandidateAttempt,
        direction: MoveDirection,
        primary: ArchivedCandidateError,
    ) -> Result<(), RetainedArchivedCandidateMoveFailure> {
        let layout = self
            .revalidate_base(installation, attempt)
            .and_then(|()| self.wrapper_layout(attempt));
        match layout {
            Ok(layout) if layout == direction.before() => Err(RetainedArchivedCandidateMoveFailure {
                outcome: if direction == MoveDirection::Rearchive && attempt.rearchive_preparation_applied {
                    RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied
                } else {
                    RetainedArchivedCandidateMoveOutcome::NotApplied
                },
                source: primary,
            }),
            Ok(layout) if layout == direction.after() => {
                self.finish_move(installation, attempt, direction).map_err(|finish| {
                    RetainedArchivedCandidateMoveFailure {
                        outcome: RetainedArchivedCandidateMoveOutcome::Applied,
                        source: ArchivedCandidateError::AppliedAfterPreflightFailure {
                            direction: direction.as_str(),
                            primary: Box::new(primary),
                            finish: Box::new(finish),
                        },
                    }
                })
            }
            Ok(layout) => Err(RetainedArchivedCandidateMoveFailure {
                outcome: RetainedArchivedCandidateMoveOutcome::Ambiguous,
                source: ArchivedCandidateError::UnexpectedLayout {
                    direction: direction.as_str(),
                    expected: direction.before().as_str(),
                    actual: layout.as_str(),
                },
            }),
            Err(reconciliation) => Err(RetainedArchivedCandidateMoveFailure {
                outcome: RetainedArchivedCandidateMoveOutcome::Ambiguous,
                source: ArchivedCandidateError::PreflightReconciliationFailed {
                    direction: direction.as_str(),
                    primary: Box::new(primary),
                    reconciliation: Box::new(reconciliation),
                },
            }),
        }
    }

    fn revalidate_base(
        &self,
        installation: &Installation,
        attempt: &RetainedArchivedCandidateAttempt,
    ) -> Result<(), ArchivedCandidateError> {
        installation
            .revalidate_root_directory()
            .map_err(super::Error::from)
            .map_err(|source| identity("revalidate installation root", source))?;
        attempt
            .roots
            .revalidate_beneath(installation.root_directory(), ROOTS_RELATIVE)
            .map_err(|source| identity("revalidate retained roots directory", source))?;
        attempt
            .candidate_wrapper
            .require_retained()
            .map_err(|source| identity("revalidate archived-candidate wrapper", source))?;
        attempt
            .displaced_staging_wrapper
            .require_retained()
            .map_err(|source| identity("revalidate displaced staging wrapper", source))?;
        if attempt.roots.witness.device != attempt.candidate_wrapper.witness.device
            || attempt.roots.witness.device != attempt.displaced_staging_wrapper.witness.device
        {
            return Err(ArchivedCandidateError::CrossDevice {
                state: attempt.candidate_wrapper.path.clone(),
                staging: attempt.displaced_staging_wrapper.path.clone(),
            });
        }
        Ok(())
    }

    fn wrapper_layout(
        &self,
        attempt: &RetainedArchivedCandidateAttempt,
    ) -> Result<CandidateLayout, ArchivedCandidateError> {
        let state_named = attempt
            .roots
            .open_optional_child(&attempt.state_name, canonical_path(attempt))
            .map_err(|source| identity("open canonical archived-candidate wrapper", source))?;
        let staging_named = attempt
            .roots
            .open_optional_child(STAGING_NAME, attempt.roots.path.join("staging"))
            .map_err(|source| identity("open named staging wrapper", source))?;
        let state_role = wrapper_role(
            state_named.as_ref().map(|directory| directory.witness),
            attempt.candidate_wrapper.witness,
            attempt.displaced_staging_wrapper.witness,
        );
        let staging_role = wrapper_role(
            staging_named.as_ref().map(|directory| directory.witness),
            attempt.candidate_wrapper.witness,
            attempt.displaced_staging_wrapper.witness,
        );
        match (state_role, staging_role) {
            ("candidate", "displaced-staging") => Ok(CandidateLayout::Archived),
            ("displaced-staging", "candidate") => Ok(CandidateLayout::Staged),
            _ => Err(ArchivedCandidateError::NamespaceMismatch {
                state_name: state_role,
                staging_name: staging_role,
            }),
        }
    }

    fn require_move_layout(
        &self,
        attempt: &RetainedArchivedCandidateAttempt,
        expected: CandidateLayout,
        marker_location: MarkerLocation,
    ) -> Result<(), ArchivedCandidateError> {
        let actual = self.wrapper_layout(attempt)?;
        if actual != expected {
            return Err(ArchivedCandidateError::UnexpectedLayout {
                direction: "preflight",
                expected: expected.as_str(),
                actual: actual.as_str(),
            });
        }
        match marker_location {
            MarkerLocation::Candidate => {
                attempt
                    .slot_marker
                    .require_named(&attempt.candidate_wrapper)
                    .map_err(|source| identity("authenticate candidate-wrapper state-slot marker", source.into()))?;
                attempt
                    .candidate_wrapper
                    .require_exact_entries(&[attempt.slot_marker.name_bytes(), b"usr"])
                    .map_err(|source| identity("validate exact archived-candidate wrapper entries", source))?;
                attempt
                    .displaced_staging_wrapper
                    .require_exact_entries(&[])
                    .map_err(|source| identity("validate exact displaced staging wrapper entries", source))?;
            }
            MarkerLocation::Displaced => {
                attempt
                    .candidate_wrapper
                    .require_exact_entries(&[b"usr"])
                    .map_err(|source| identity("validate exact archived-candidate wrapper entries", source))?;
                attempt
                    .slot_marker
                    .require_named(&attempt.displaced_staging_wrapper)
                    .map_err(|source| identity("authenticate displaced-wrapper state-slot marker", source.into()))?;
                attempt
                    .displaced_staging_wrapper
                    .require_exact_entries(&[attempt.slot_marker.name_bytes()])
                    .map_err(|source| identity("validate exact displaced staging wrapper entries", source))?;
            }
        }
        let current_path = match expected {
            CandidateLayout::Archived => canonical_path(attempt).join("usr"),
            CandidateLayout::Staged => attempt.roots.path.join("staging/usr"),
        };
        let candidate = open_optional_retained_tree(&attempt.candidate_wrapper, &current_path)
            .map_err(|source| identity("open exact archived candidate beneath retained wrapper", source))?
            .ok_or_else(|| ArchivedCandidateError::NamespaceMismatch {
                state_name: if expected == CandidateLayout::Archived {
                    "candidate-without-usr"
                } else {
                    "displaced-staging"
                },
                staging_name: if expected == CandidateLayout::Staged {
                    "candidate-without-usr"
                } else {
                    "displaced-staging"
                },
            })?;
        self.candidate
            .verify_store_read_only(&candidate)
            .map_err(|source| identity("authenticate archived candidate beneath retained wrapper", source))?;
        self.candidate
            .verify_named_read_only(&current_path)
            .map_err(|source| identity("authenticate current archived-candidate name", source))
    }
}

fn preparation_failure(
    outcome: PreparationOutcome,
    preparation_applied: bool,
    source: ArchivedCandidateError,
) -> RetainedArchivedCandidateMoveFailure {
    let outcome = match outcome {
        PreparationOutcome::NotApplied if preparation_applied => {
            RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied
        }
        PreparationOutcome::NotApplied => RetainedArchivedCandidateMoveOutcome::NotApplied,
        PreparationOutcome::Applied => RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied,
        PreparationOutcome::Ambiguous => RetainedArchivedCandidateMoveOutcome::Ambiguous,
    };
    RetainedArchivedCandidateMoveFailure { outcome, source }
}

fn require_attempt_state(
    attempt: &RetainedArchivedCandidateAttempt,
    state: state::Id,
) -> Result<(), ArchivedCandidateError> {
    if attempt.state == state {
        Ok(())
    } else {
        Err(ArchivedCandidateError::AttemptChanged {
            expected: i32::from(attempt.state),
            actual: i32::from(state),
        })
    }
}

fn canonical_path(attempt: &RetainedArchivedCandidateAttempt) -> PathBuf {
    attempt.roots.path.join(attempt.state_name.to_string_lossy().as_ref())
}

fn wrapper_role(
    witness: Option<RetainedDirectoryWitness>,
    candidate: RetainedDirectoryWitness,
    displaced: RetainedDirectoryWitness,
) -> &'static str {
    match witness {
        None => "absent",
        Some(witness) if witness == candidate => "candidate",
        Some(witness) if witness == displaced => "displaced-staging",
        Some(_) => "foreign",
    }
}

fn identity(operation: &'static str, source: super::Error) -> ArchivedCandidateError {
    ArchivedCandidateError::Identity {
        operation,
        source: Box::new(source),
    }
}

#[cfg(test)]
std::thread_local! {
    static FAULT: std::cell::RefCell<Option<RetainedArchivedCandidateMoveFaultPoint>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_EXCHANGE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_RETIRED_SLOT_MOVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_SLOT_MARKER_LOCATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_retained_archived_candidate_move_fault(point: RetainedArchivedCandidateMoveFaultPoint) {
    FAULT.with(|slot| {
        assert!(
            slot.replace(Some(point)).is_none(),
            "archived-candidate fault already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_before_retained_archived_candidate_exchange(hook: impl FnOnce() + 'static) {
    BEFORE_EXCHANGE.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "archived-candidate hook already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_before_retired_archived_candidate_slot_move(hook: impl FnOnce() + 'static) {
    BEFORE_RETIRED_SLOT_MOVE.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "displaced-slot hook already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_before_archived_candidate_slot_marker_location(hook: impl FnOnce() + 'static) {
    BEFORE_SLOT_MARKER_LOCATION.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "archived-candidate slot-marker-location hook already armed"
        );
    });
}

#[cfg(test)]
fn checkpoint(point: RetainedArchivedCandidateMoveFaultPoint) -> Result<(), ArchivedCandidateError> {
    FAULT.with(|slot| {
        if slot.borrow().as_ref() == Some(&point) {
            slot.replace(None);
            Err(ArchivedCandidateError::InjectedFault { point })
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
fn checkpoint(_: RetainedArchivedCandidateMoveFaultPoint) -> Result<(), ArchivedCandidateError> {
    Ok(())
}

#[cfg(test)]
fn before_exchange() {
    BEFORE_EXCHANGE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_exchange() {}

#[cfg(test)]
fn before_retired_slot_move() {
    BEFORE_RETIRED_SLOT_MOVE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
fn before_slot_marker_location() {
    BEFORE_SLOT_MARKER_LOCATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_slot_marker_location() {}

#[cfg(not(test))]
fn before_retired_slot_move() {}
