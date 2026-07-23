//! Monotonic parking for an active tree's recovered canonical slot wrapper.
//!
//! An active-state reblit can inherit an authorized second tree-marker link in
//! the canonical decimal state wrapper. Leaving that marker-only wrapper at
//! the canonical name blocks the next ordinary archive. This module moves only
//! the exact retained wrapper to the bounded archived-candidate parking
//! namespace. Foreign occupants are never adopted, replaced, or unlinked.

use std::{ffi::CString, io, path::PathBuf};

use thiserror::Error;

use super::{
    MAX_PREVIOUS_SLOT_PARKING_CANDIDATES, ROOTS_RELATIVE, RetainedDirectory, StatefulTreeIdentity,
    archived_candidate::archived_candidate_parking_name,
    canonical_state_name,
    slot_link_recovery::{RecoveredSlotLink, WrapperKind},
    state_slot_marker::RetainedStateSlotMarker,
};
use crate::{Installation, linux_fs::renameat2_noreplace_once, state};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedActivePreviousSlotParkingFaultPoint {
    MarkerPreSync,
    WrapperPreSync,
    RootsPreSync,
    BeforeRename,
    AfterRename,
    MarkerPostSync,
    WrapperPostSync,
    RootsPostSync,
    FinalRevalidation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RetainedActivePreviousSlotParkingOutcome {
    NotApplied,
    Applied,
    Ambiguous,
}

#[derive(Debug, Error)]
#[error("retained active previous-slot parking outcome is {outcome:?}")]
pub(super) struct RetainedActivePreviousSlotParkingFailure {
    outcome: RetainedActivePreviousSlotParkingOutcome,
    #[source]
    source: ActivePreviousSlotParkingError,
}

impl RetainedActivePreviousSlotParkingFailure {
    pub(super) fn outcome(&self) -> RetainedActivePreviousSlotParkingOutcome {
        self.outcome
    }
}

#[derive(Debug, Error)]
pub(crate) enum ActivePreviousSlotParkingError {
    #[error("{operation} while parking the retained active previous-state slot: {source}")]
    Identity {
        operation: &'static str,
        #[source]
        source: Box<super::Error>,
    },
    #[error("coordinator evidence failed while parking the retained active previous-state slot")]
    CoordinatorEvidence(#[source] Box<super::journal_coordinator::StatefulTransitionCoordinatorError>),
    #[error("construct a bounded active previous-state parking name")]
    InvalidParkingName(#[source] crate::transition_journal::CodecError),
    #[error("all {limit} active previous-state parking names are occupied for state {state}")]
    ParkingExhausted { state: i32, limit: usize },
    #[error("retained active previous-state slot attempt lock is poisoned")]
    AttemptLockPoisoned,
    #[error("retained active previous-state slot belongs to state {expected}, not {actual}")]
    AttemptStateChanged { expected: i32, actual: i32 },
    #[error("no active previous-state parking name was selected for state {state}")]
    ParkingNameMissing { state: i32 },
    #[error("active previous-state parking destination already exists at `{}`", path.display())]
    DestinationOccupied { path: PathBuf },
    #[error(
        "retained active previous-state slot namespace mismatch: canonical `{}` is {canonical}, parking `{}` is {parking}",
        canonical_path.display(),
        parking_path.display()
    )]
    NamespaceMismatch {
        canonical_path: PathBuf,
        canonical: &'static str,
        parking_path: PathBuf,
        parking: &'static str,
    },
    #[error("active previous-state slot rename reported success without moving the exact wrapper")]
    ReportedSuccessWithoutMove,
    #[error("rename retained active previous-state slot to `{}`", path.display())]
    Rename {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("active previous-state slot preflight failed ({primary}) and reconciliation failed ({reconciliation})")]
    PreflightReconciliationFailed {
        primary: Box<ActivePreviousSlotParkingError>,
        reconciliation: Box<ActivePreviousSlotParkingError>,
    },
    #[error("active previous-state slot preflight failed ({primary}) after parking applied; suffix failed ({finish})")]
    AppliedAfterPreflightFailure {
        primary: Box<ActivePreviousSlotParkingError>,
        finish: Box<ActivePreviousSlotParkingError>,
    },
    #[cfg(test)]
    #[error("injected retained active previous-state slot fault at {point:?}")]
    InjectedFault {
        point: RetainedActivePreviousSlotParkingFaultPoint,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotLocation {
    Canonical,
    Parked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NamedWrapper {
    Absent,
    Exact,
    Foreign,
}

impl NamedWrapper {
    fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Exact => "exact",
            Self::Foreign => "foreign",
        }
    }
}

/// Retains the exact authorized wrapper, marker, and roots directory across
/// the no-replace move and every later active-reblit boundary.
#[derive(Debug)]
pub(super) struct RetainedActivePreviousSlotParking {
    roots: RetainedDirectory,
    state: state::Id,
    canonical_name: CString,
    parking_name: Option<CString>,
    wrapper: RetainedDirectory,
    marker: RetainedStateSlotMarker,
}

impl RetainedActivePreviousSlotParking {
    pub(super) fn from_recovered(recovered: RecoveredSlotLink) -> Result<Self, super::Error> {
        let canonical_name = canonical_state_name(recovered.state)?;
        let parking_name = match recovered.kind {
            WrapperKind::Canonical => {
                if recovered.name != canonical_name {
                    return Err(super::Error::PreviousArchiveAttemptChanged {
                        expected: canonical_name.to_string_lossy().into_owned(),
                        actual: recovered.name.to_string_lossy().into_owned(),
                    });
                }
                None
            }
            WrapperKind::Parked => Some(recovered.name),
        };
        Ok(Self {
            roots: recovered.roots,
            state: recovered.state,
            canonical_name,
            parking_name,
            wrapper: recovered.wrapper,
            marker: recovered.marker,
        })
    }

    fn require_state(&self, state: state::Id) -> Result<(), ActivePreviousSlotParkingError> {
        if self.state == state {
            Ok(())
        } else {
            Err(ActivePreviousSlotParkingError::AttemptStateChanged {
                expected: i32::from(self.state),
                actual: i32::from(state),
            })
        }
    }

    fn canonical_path(&self) -> PathBuf {
        self.roots.path.join(self.canonical_name.to_string_lossy().as_ref())
    }

    fn parking_path(&self) -> PathBuf {
        self.roots.path.join(
            self.parking_name
                .as_ref()
                .expect("active previous-slot parking name was selected")
                .to_string_lossy()
                .as_ref(),
        )
    }
}

impl StatefulTreeIdentity {
    pub(super) fn prepare_active_previous_slot_parking(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), RetainedActivePreviousSlotParkingFailure> {
        self.prepare_active_previous_slot_parking_validated(installation, state, &|| {
            self.require_no_journal()
                .map_err(|source| identity("check journal while parking active previous-state slot", source))
        })
    }

    /// Journal-aware counterpart used only by the sealed ActiveReblit
    /// reservation typestate. It never relaxes the legacy no-journal entry
    /// point above; instead the coordinator supplies a complete semantic proof
    /// immediately before the no-replace move and after its durability suffix.
    pub(super) fn prepare_active_previous_slot_parking_with_journal(
        &self,
        _seal: &super::journal_coordinator::ActiveReblitReservationSeal,
        installation: &Installation,
        state: state::Id,
        validate_transition: &impl Fn() -> Result<(), super::journal_coordinator::StatefulTransitionCoordinatorError>,
    ) -> Result<(), RetainedActivePreviousSlotParkingFailure> {
        let validate = || {
            validate_transition()
                .map_err(|source| ActivePreviousSlotParkingError::CoordinatorEvidence(Box::new(source)))
        };
        self.prepare_active_previous_slot_parking_validated(installation, state, &validate)
    }

    fn prepare_active_previous_slot_parking_validated(
        &self,
        installation: &Installation,
        state: state::Id,
        validate_transition: &impl Fn() -> Result<(), ActivePreviousSlotParkingError>,
    ) -> Result<(), RetainedActivePreviousSlotParkingFailure> {
        let mut retried = false;
        loop {
            match self.park_active_previous_state_slot_validated(installation, state, validate_transition) {
                Ok(()) => return Ok(()),
                Err(failure)
                    if failure.outcome() != RetainedActivePreviousSlotParkingOutcome::Ambiguous && !retried =>
                {
                    retried = true;
                }
                Err(failure) => return Err(failure),
            }
        }
    }

    fn park_active_previous_state_slot_validated(
        &self,
        installation: &Installation,
        state: state::Id,
        validate_transition: &impl Fn() -> Result<(), ActivePreviousSlotParkingError>,
    ) -> Result<(), RetainedActivePreviousSlotParkingFailure> {
        let not_applied = |source| RetainedActivePreviousSlotParkingFailure {
            outcome: RetainedActivePreviousSlotParkingOutcome::NotApplied,
            source,
        };
        let applied = |source| RetainedActivePreviousSlotParkingFailure {
            outcome: RetainedActivePreviousSlotParkingOutcome::Applied,
            source,
        };
        let ambiguous = |source| RetainedActivePreviousSlotParkingFailure {
            outcome: RetainedActivePreviousSlotParkingOutcome::Ambiguous,
            source,
        };

        let mut retained = self
            .active_previous_slot_parking
            .lock()
            .map_err(|_| not_applied(ActivePreviousSlotParkingError::AttemptLockPoisoned))?;
        let Some(attempt) = retained.as_mut() else {
            return Ok(());
        };
        attempt.require_state(state).map_err(not_applied)?;
        self.select_active_previous_slot_parking_name(attempt)
            .map_err(not_applied)?;

        self.revalidate_active_previous_slot_base(installation, attempt)
            .map_err(ambiguous)?;
        match self.active_previous_slot_location(attempt).map_err(ambiguous)? {
            SlotLocation::Parked => {
                return self
                    .finish_active_previous_slot_parking(installation, attempt, validate_transition)
                    .map_err(applied);
            }
            SlotLocation::Canonical => {}
        }

        let preflight = (|| -> Result<(), ActivePreviousSlotParkingError> {
            checkpoint(RetainedActivePreviousSlotParkingFaultPoint::MarkerPreSync)?;
            attempt
                .marker
                .sync()
                .map_err(|source| identity("sync active previous-state slot marker", source.into()))?;
            checkpoint(RetainedActivePreviousSlotParkingFaultPoint::WrapperPreSync)?;
            attempt
                .wrapper
                .sync("sync active previous-state wrapper before parking")
                .map_err(|source| identity("sync active previous-state wrapper before parking", source))?;
            checkpoint(RetainedActivePreviousSlotParkingFaultPoint::RootsPreSync)?;
            attempt
                .roots
                .sync("sync roots before active previous-state slot parking")
                .map_err(|source| identity("sync roots before active previous-state slot parking", source))?;
            before_rename();
            self.revalidate_active_previous_slot_base(installation, attempt)?;
            self.require_active_previous_slot_location(attempt, SlotLocation::Canonical)?;
            if self.named_active_previous_slot_wrapper(attempt, false)? != NamedWrapper::Absent {
                return Err(ActivePreviousSlotParkingError::DestinationOccupied {
                    path: attempt.parking_path(),
                });
            }
            validate_transition()?;
            checkpoint(RetainedActivePreviousSlotParkingFaultPoint::BeforeRename)
        })();
        if let Err(source) = preflight {
            return self.reconcile_active_previous_slot_preflight(installation, attempt, validate_transition, source);
        }

        let parking_name = attempt
            .parking_name
            .as_ref()
            .expect("active previous-slot parking name was selected");
        let syscall_result = renameat2_noreplace_once(
            &attempt.roots.file,
            &attempt.canonical_name,
            &attempt.roots.file,
            parking_name,
        )
        .map_err(|source| ActivePreviousSlotParkingError::Rename {
            path: attempt.parking_path(),
            source,
        })
        .and_then(|()| checkpoint(RetainedActivePreviousSlotParkingFaultPoint::AfterRename));

        match self.active_previous_slot_location(attempt).map_err(ambiguous)? {
            SlotLocation::Canonical => Err(not_applied(
                syscall_result
                    .err()
                    .unwrap_or(ActivePreviousSlotParkingError::ReportedSuccessWithoutMove),
            )),
            SlotLocation::Parked => self
                .finish_active_previous_slot_parking(installation, attempt, validate_transition)
                .map_err(applied),
        }
    }

    pub(super) fn require_active_previous_slot_parked(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), ActivePreviousSlotParkingError> {
        self.require_no_journal()
            .map_err(|source| identity("check journal while proving parked active previous-state slot", source))?;
        self.require_active_previous_slot_parked_core(installation, state)
    }

    /// Prove the completed parked layout while the reservation journal is
    /// active. The private seal prevents legacy callers from bypassing their
    /// mandatory journal-absence check.
    pub(super) fn require_active_previous_slot_parked_with_journal(
        &self,
        _seal: &super::journal_coordinator::ActiveReblitReservationSeal,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), ActivePreviousSlotParkingError> {
        self.require_active_previous_slot_parked_core(installation, state)
    }

    /// Read-only proof of an authorized second marker link while a durable
    /// coordinator journal is active.  It preserves the exact recovered
    /// canonical-or-parked location and performs no reservation or move.
    pub(super) fn require_active_previous_slot_unchanged_with_journal(
        &self,
        _seal: &super::journal_coordinator::UsrExchangeEffectSeal,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), ActivePreviousSlotParkingError> {
        self.require_active_previous_slot_unchanged_core(installation, state)
    }

    /// Reservation-sealed form of the same canonical-or-parked proof. It is
    /// used before the monotonic no-replace move, while CandidatePrepared may
    /// legitimately retain either exact location.
    pub(super) fn require_active_previous_slot_unchanged_for_reservation(
        &self,
        _seal: &super::journal_coordinator::ActiveReblitReservationSeal,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), ActivePreviousSlotParkingError> {
        self.require_active_previous_slot_unchanged_core(installation, state)
    }

    fn require_active_previous_slot_unchanged_core(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), ActivePreviousSlotParkingError> {
        let retained = self
            .active_previous_slot_parking
            .lock()
            .map_err(|_| ActivePreviousSlotParkingError::AttemptLockPoisoned)?;
        let Some(attempt) = retained.as_ref() else {
            return Ok(());
        };
        attempt.require_state(state)?;
        self.revalidate_active_previous_slot_base(installation, attempt)?;
        if attempt.parking_name.is_some() {
            self.require_active_previous_slot_location(attempt, SlotLocation::Parked)
        } else if self.named_active_previous_slot_wrapper(attempt, true)? == NamedWrapper::Exact {
            Ok(())
        } else {
            Err(self.active_previous_slot_namespace_mismatch_without_parking(attempt)?)
        }
    }

    fn require_active_previous_slot_parked_core(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), ActivePreviousSlotParkingError> {
        let retained = self
            .active_previous_slot_parking
            .lock()
            .map_err(|_| ActivePreviousSlotParkingError::AttemptLockPoisoned)?;
        let Some(attempt) = retained.as_ref() else {
            return Ok(());
        };
        attempt.require_state(state)?;
        if attempt.parking_name.is_none() {
            return Err(ActivePreviousSlotParkingError::ParkingNameMissing {
                state: i32::from(state),
            });
        }
        self.revalidate_active_previous_slot_base(installation, attempt)?;
        self.require_active_previous_slot_location(attempt, SlotLocation::Parked)
    }

    fn select_active_previous_slot_parking_name(
        &self,
        attempt: &mut RetainedActivePreviousSlotParking,
    ) -> Result<(), ActivePreviousSlotParkingError> {
        if attempt.parking_name.is_some() {
            return Ok(());
        }
        for index in 0..MAX_PREVIOUS_SLOT_PARKING_CANDIDATES {
            let name = archived_candidate_parking_name(attempt.state, self.previous.marker.token().as_str(), index)
                .map_err(ActivePreviousSlotParkingError::InvalidParkingName)?;
            let path = attempt.roots.path.join(name.to_string_lossy().as_ref());
            if !attempt
                .roots
                .child_name_exists(&name, path)
                .map_err(|source| identity("scan active previous-state parking names", source))?
            {
                attempt.parking_name = Some(name);
                return Ok(());
            }
        }
        Err(ActivePreviousSlotParkingError::ParkingExhausted {
            state: i32::from(attempt.state),
            limit: MAX_PREVIOUS_SLOT_PARKING_CANDIDATES,
        })
    }

    fn reconcile_active_previous_slot_preflight(
        &self,
        installation: &Installation,
        attempt: &RetainedActivePreviousSlotParking,
        validate_transition: &impl Fn() -> Result<(), ActivePreviousSlotParkingError>,
        source: ActivePreviousSlotParkingError,
    ) -> Result<(), RetainedActivePreviousSlotParkingFailure> {
        let reconciled = self
            .revalidate_active_previous_slot_base(installation, attempt)
            .and_then(|()| self.active_previous_slot_location(attempt));
        match reconciled {
            Ok(SlotLocation::Canonical) => Err(RetainedActivePreviousSlotParkingFailure {
                outcome: RetainedActivePreviousSlotParkingOutcome::NotApplied,
                source,
            }),
            Ok(SlotLocation::Parked) => self
                .finish_active_previous_slot_parking(installation, attempt, validate_transition)
                .map_err(|finish| RetainedActivePreviousSlotParkingFailure {
                    outcome: RetainedActivePreviousSlotParkingOutcome::Applied,
                    source: ActivePreviousSlotParkingError::AppliedAfterPreflightFailure {
                        primary: Box::new(source),
                        finish: Box::new(finish),
                    },
                }),
            Err(reconciliation) => Err(RetainedActivePreviousSlotParkingFailure {
                outcome: RetainedActivePreviousSlotParkingOutcome::Ambiguous,
                source: ActivePreviousSlotParkingError::PreflightReconciliationFailed {
                    primary: Box::new(source),
                    reconciliation: Box::new(reconciliation),
                },
            }),
        }
    }

    fn finish_active_previous_slot_parking(
        &self,
        installation: &Installation,
        attempt: &RetainedActivePreviousSlotParking,
        validate_transition: &impl Fn() -> Result<(), ActivePreviousSlotParkingError>,
    ) -> Result<(), ActivePreviousSlotParkingError> {
        checkpoint(RetainedActivePreviousSlotParkingFaultPoint::MarkerPostSync)?;
        attempt
            .marker
            .sync()
            .map_err(|source| identity("sync parked active previous-state marker", source.into()))?;
        checkpoint(RetainedActivePreviousSlotParkingFaultPoint::WrapperPostSync)?;
        attempt
            .wrapper
            .sync("sync parked active previous-state wrapper")
            .map_err(|source| identity("sync parked active previous-state wrapper", source))?;
        checkpoint(RetainedActivePreviousSlotParkingFaultPoint::RootsPostSync)?;
        attempt
            .roots
            .sync("sync roots after active previous-state slot parking")
            .map_err(|source| identity("sync roots after active previous-state slot parking", source))?;
        checkpoint(RetainedActivePreviousSlotParkingFaultPoint::FinalRevalidation)?;
        validate_transition()?;
        self.revalidate_active_previous_slot_base(installation, attempt)?;
        self.require_active_previous_slot_location(attempt, SlotLocation::Parked)
    }

    fn revalidate_active_previous_slot_base(
        &self,
        installation: &Installation,
        attempt: &RetainedActivePreviousSlotParking,
    ) -> Result<(), ActivePreviousSlotParkingError> {
        installation
            .revalidate_root_directory()
            .map_err(super::Error::from)
            .map_err(|source| identity("revalidate installation root", source))?;
        attempt
            .roots
            .revalidate_beneath(installation.root_directory(), ROOTS_RELATIVE)
            .map_err(|source| identity("revalidate roots directory", source))?;
        attempt
            .wrapper
            .require_retained()
            .map_err(|source| identity("revalidate active previous-state wrapper", source))?;
        attempt
            .marker
            .require_retained()
            .map_err(|source| identity("revalidate active previous-state slot marker", source.into()))?;
        attempt
            .marker
            .require_named(&attempt.wrapper)
            .map_err(|source| identity("authenticate active previous-state slot marker", source.into()))?;
        attempt
            .wrapper
            .require_exact_entries(&[attempt.marker.name_bytes()])
            .map_err(|source| identity("require marker-only active previous-state wrapper", source))?;
        self.previous
            .revalidate_retained()
            .map_err(|source| identity("revalidate active previous tree marker", source))
    }

    fn require_active_previous_slot_location(
        &self,
        attempt: &RetainedActivePreviousSlotParking,
        expected: SlotLocation,
    ) -> Result<(), ActivePreviousSlotParkingError> {
        let actual = self.active_previous_slot_location(attempt)?;
        if actual == expected {
            Ok(())
        } else {
            Err(self.active_previous_slot_namespace_mismatch(attempt)?)
        }
    }

    fn active_previous_slot_location(
        &self,
        attempt: &RetainedActivePreviousSlotParking,
    ) -> Result<SlotLocation, ActivePreviousSlotParkingError> {
        let canonical = self.named_active_previous_slot_wrapper(attempt, true)?;
        let parking = self.named_active_previous_slot_wrapper(attempt, false)?;
        match (canonical, parking) {
            (NamedWrapper::Exact, NamedWrapper::Absent | NamedWrapper::Foreign) => Ok(SlotLocation::Canonical),
            (NamedWrapper::Absent, NamedWrapper::Exact) => Ok(SlotLocation::Parked),
            _ => Err(ActivePreviousSlotParkingError::NamespaceMismatch {
                canonical_path: attempt.canonical_path(),
                canonical: canonical.as_str(),
                parking_path: attempt.parking_path(),
                parking: parking.as_str(),
            }),
        }
    }

    fn active_previous_slot_namespace_mismatch(
        &self,
        attempt: &RetainedActivePreviousSlotParking,
    ) -> Result<ActivePreviousSlotParkingError, ActivePreviousSlotParkingError> {
        let canonical = self.named_active_previous_slot_wrapper(attempt, true)?;
        let parking = self.named_active_previous_slot_wrapper(attempt, false)?;
        Ok(ActivePreviousSlotParkingError::NamespaceMismatch {
            canonical_path: attempt.canonical_path(),
            canonical: canonical.as_str(),
            parking_path: attempt.parking_path(),
            parking: parking.as_str(),
        })
    }

    fn active_previous_slot_namespace_mismatch_without_parking(
        &self,
        attempt: &RetainedActivePreviousSlotParking,
    ) -> Result<ActivePreviousSlotParkingError, ActivePreviousSlotParkingError> {
        let canonical = self.named_active_previous_slot_wrapper(attempt, true)?;
        Ok(ActivePreviousSlotParkingError::NamespaceMismatch {
            canonical_path: attempt.canonical_path(),
            canonical: canonical.as_str(),
            parking_path: attempt.canonical_path(),
            parking: NamedWrapper::Absent.as_str(),
        })
    }

    fn named_active_previous_slot_wrapper(
        &self,
        attempt: &RetainedActivePreviousSlotParking,
        canonical: bool,
    ) -> Result<NamedWrapper, ActivePreviousSlotParkingError> {
        let (name, path) = if canonical {
            (&attempt.canonical_name, attempt.canonical_path())
        } else {
            (
                attempt
                    .parking_name
                    .as_ref()
                    .expect("active previous-slot parking name was selected"),
                attempt.parking_path(),
            )
        };
        if !attempt
            .roots
            .child_name_exists(name, path.clone())
            .map_err(|source| identity("probe active previous-state slot name", source))?
        {
            return Ok(NamedWrapper::Absent);
        }
        match attempt.roots.open_child(name, path) {
            Ok(named) if named.witness == attempt.wrapper.witness => Ok(NamedWrapper::Exact),
            Ok(_) => Ok(NamedWrapper::Foreign),
            Err(source) if skippable_foreign_wrapper(&source) => Ok(NamedWrapper::Foreign),
            Err(source) => Err(identity("open active previous-state slot name", source)),
        }
    }
}

fn skippable_foreign_wrapper(source: &super::Error) -> bool {
    match source {
        super::Error::UnsafeQuarantineDirectory { .. } => true,
        super::Error::Quarantine {
            operation: "pin retained directory",
            source,
            ..
        } => matches!(
            source.raw_os_error(),
            Some(nix::libc::ENOTDIR) | Some(nix::libc::ELOOP) | Some(nix::libc::EXDEV)
        ),
        super::Error::Quarantine {
            operation: "reject access ACL on retained directory" | "reject default ACL on retained directory",
            source,
            ..
        } => source.kind() == io::ErrorKind::PermissionDenied && source.raw_os_error().is_none(),
        _ => false,
    }
}

fn identity(operation: &'static str, source: super::Error) -> ActivePreviousSlotParkingError {
    ActivePreviousSlotParkingError::Identity {
        operation,
        source: Box::new(source),
    }
}

#[cfg(test)]
std::thread_local! {
    static FAULT: std::cell::RefCell<Vec<RetainedActivePreviousSlotParkingFaultPoint>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_RENAME: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_active_previous_slot_parking_faults(
    points: impl IntoIterator<Item = RetainedActivePreviousSlotParkingFaultPoint>,
) {
    let mut points = points.into_iter().collect::<Vec<_>>();
    points.reverse();
    FAULT.with(|fault| *fault.borrow_mut() = points);
}

#[cfg(test)]
pub(crate) fn arm_before_active_previous_slot_parking_rename(hook: impl FnOnce() + 'static) {
    BEFORE_RENAME.with(|armed| *armed.borrow_mut() = Some(Box::new(hook)));
}

fn before_rename() {
    #[cfg(test)]
    BEFORE_RENAME.with(|armed| {
        if let Some(hook) = armed.borrow_mut().take() {
            hook();
        }
    });
}

fn checkpoint(point: RetainedActivePreviousSlotParkingFaultPoint) -> Result<(), ActivePreviousSlotParkingError> {
    #[cfg(test)]
    if FAULT.with(|fault| fault.borrow_mut().last().copied()) == Some(point) {
        FAULT.with(|fault| {
            fault.borrow_mut().pop();
        });
        return Err(ActivePreviousSlotParkingError::InjectedFault { point });
    }
    let _ = point;
    Ok(())
}
