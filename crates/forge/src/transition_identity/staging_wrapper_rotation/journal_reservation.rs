//! Sealed journal-aware ActiveReblit reservation contract.
//!
//! This is deliberately separate from the legacy lifecycle. The sole mutating
//! entry point requires a coordinator-private seal and caller-owned semantic
//! validation, so no-journal guards remain intact everywhere else.

use crate::{Installation, State};
use thiserror::Error as ThisError;

use super::{
    RetainedStagingWrapperRotation, RetainedStagingWrapperRotationFailure, RetainedStagingWrapperRotationOutcome,
    model::{StagingWrapperPreparationFailure, StagingWrapperRotationError},
    state_snapshot::same_state_snapshot,
};
use crate::transition_identity::active_previous_slot_parking::{
    ActivePreviousSlotParkingError, RetainedActivePreviousSlotParkingFailure,
};
use crate::transition_identity::journal_coordinator::StatefulTransitionCoordinatorError;
use crate::transition_identity::{Error, StatefulTreeIdentity};

/// Reservation failure classified by the last aggregate fact established.
///
/// A previous-slot failure cannot be mistaken for an entirely unapplied
/// reservation once the replacement wrapper is already durable.
#[derive(Debug, ThisError)]
pub(in crate::transition_identity) enum ActiveReblitReservationError {
    #[error("ActiveReblit reservation failed before any reservation mutation")]
    Preflight(#[source] ActiveReblitReservationPreflightError),
    #[error("ActiveReblit replacement-wrapper reservation failed")]
    Replacement(#[source] RetainedStagingWrapperRotationFailure),
    #[error("ActiveReblit replacement-wrapper preparation failed")]
    ReplacementPreparation(#[source] StagingWrapperPreparationFailure),
    #[error("ActiveReblit previous-slot parking failed after the replacement wrapper became durable")]
    PreviousSlotAfterDurableReplacement(#[source] RetainedActivePreviousSlotParkingFailure),
    #[error("final aggregate ActiveReblit reservation evidence failed after reservation effects")]
    FinalReservationEvidenceAfterEffects(#[source] RetainedActiveReblitReservationEvidenceFailure),
    #[error("final coordinator evidence failed after ActiveReblit reservation effects")]
    FinalCoordinatorEvidenceAfterEffects(#[source] Box<StatefulTransitionCoordinatorError>),
}

#[derive(Debug, ThisError)]
pub(in crate::transition_identity) enum ActiveReblitReservationPreflightError {
    #[error("authenticate the retained ActiveReblit identity before reservation")]
    Identity(#[source] Error),
    #[error("validate coordinator evidence before ActiveReblit reservation")]
    CoordinatorEvidence(#[source] Box<StatefulTransitionCoordinatorError>),
}

/// Exact component which failed while revalidating a retained aggregate
/// reservation. No single outcome label is used because replacement and slot
/// effects are independent durable facts.
#[derive(Debug, ThisError)]
pub(crate) enum RetainedActiveReblitReservationEvidenceFailure {
    #[error("{operation} while revalidating the retained ActiveReblit reservation")]
    Identity {
        operation: &'static str,
        #[source]
        source: Error,
    },
    #[error("revalidate the retained ActiveReblit replacement wrapper")]
    Replacement(#[source] RetainedStagingWrapperRotationFailure),
    #[error("revalidate the retained ActiveReblit previous slot")]
    PreviousSlot(#[source] ActivePreviousSlotParkingError),
}

/// Retained proof bundle for the exact pre-trigger namespace reservation.
///
/// Construction remains private to this module. The coordinator receives it
/// only after both the empty replacement and any authenticated previous slot
/// have reached their durability boundaries. Keeping the retained
/// `Installation` and full database state snapshot here lets every later
/// typestate revalidate the same root/selection/state evidence without asking
/// a caller to supply a substitute capability.
#[derive(Debug)]
pub(in crate::transition_identity) struct RetainedActiveReblitReservation {
    installation: Installation,
    expected_state: State,
}

impl RetainedActiveReblitReservation {
    pub(in crate::transition_identity) fn require_staged(
        &self,
        identity: &StatefulTreeIdentity,
        seal: &crate::transition_identity::journal_coordinator::ActiveReblitReservationSeal,
    ) -> Result<(), RetainedActiveReblitReservationEvidenceFailure> {
        identity.require_journal_active_reblit_reservation(seal, &self.installation, &self.expected_state, false)
    }

    pub(in crate::transition_identity) fn require_live(
        &self,
        identity: &StatefulTreeIdentity,
        seal: &crate::transition_identity::journal_coordinator::ActiveReblitReservationSeal,
    ) -> Result<(), RetainedActiveReblitReservationEvidenceFailure> {
        identity.require_journal_active_reblit_reservation(seal, &self.installation, &self.expected_state, true)
    }

    pub(in crate::transition_identity) fn installation(&self) -> &Installation {
        &self.installation
    }
}

impl StatefulTreeIdentity {
    /// Reserve all ActiveReblit cleanup capacity while CandidatePrepared is
    /// canonical. Any error leaves that durable phase in place and returns no
    /// retained identity or reservation authority.
    pub(in crate::transition_identity) fn reserve_active_reblit_with_journal(
        &self,
        seal: &crate::transition_identity::journal_coordinator::ActiveReblitReservationSeal,
        installation: &Installation,
        expected_state: State,
        validate_transition: &impl Fn() -> Result<(), StatefulTransitionCoordinatorError>,
    ) -> Result<RetainedActiveReblitReservation, ActiveReblitReservationError> {
        let state = expected_state.id;
        self.require_journal_active_reblit_pre_reservation(seal, installation, &expected_state)
            .map_err(ActiveReblitReservationPreflightError::Identity)
            .map_err(ActiveReblitReservationError::Preflight)?;
        validate_transition()
            .map_err(|source| ActiveReblitReservationPreflightError::CoordinatorEvidence(Box::new(source)))
            .map_err(ActiveReblitReservationError::Preflight)?;

        let mut retained = self
            .active_reblit_rotation
            .lock()
            .map_err(|_| not_applied(StagingWrapperRotationError::AttemptLockPoisoned))
            .map_err(ActiveReblitReservationError::Replacement)?;
        if retained.is_some() {
            return Err(ActiveReblitReservationError::Replacement(not_applied(
                StagingWrapperRotationError::AttemptAlreadyReserved,
            )));
        }
        let rotation = RetainedStagingWrapperRotation::reserve_with_journal(
            seal,
            installation,
            "active-reblit-wrapper",
            state,
            self.previous.marker.token().as_str(),
            validate_transition,
        )
        .map_err(ActiveReblitReservationError::Replacement)?;
        *retained = Some(rotation);

        // Store the retained inode before entering any fallible durability
        // suffix. A failure therefore cannot accidentally authorize triggers,
        // but in-process reconciliation still owns the exact partial attempt
        // until the coordinator is dropped.
        let rotation = retained.as_ref().expect("journal reservation was retained");
        let mut retried = false;
        loop {
            match rotation.finish_preparation_with_journal(seal, installation, validate_transition) {
                Ok(()) => break,
                Err(StagingWrapperPreparationFailure::DurabilityUnproven(_)) if !retried => {
                    retried = true;
                }
                Err(failure) => {
                    return Err(ActiveReblitReservationError::ReplacementPreparation(failure));
                }
            }
        }
        drop(retained);

        self.prepare_active_previous_slot_parking_with_journal(seal, installation, state, validate_transition)
            .map_err(ActiveReblitReservationError::PreviousSlotAfterDurableReplacement)?;

        let reservation = RetainedActiveReblitReservation {
            installation: installation.clone(),
            expected_state,
        };
        reservation
            .require_staged(self, seal)
            .map_err(ActiveReblitReservationError::FinalReservationEvidenceAfterEffects)?;
        validate_transition()
            .map_err(Box::new)
            .map_err(ActiveReblitReservationError::FinalCoordinatorEvidenceAfterEffects)?;
        reservation
            .require_staged(self, seal)
            .map_err(ActiveReblitReservationError::FinalReservationEvidenceAfterEffects)?;
        Ok(reservation)
    }

    fn require_journal_active_reblit_pre_reservation(
        &self,
        seal: &crate::transition_identity::journal_coordinator::ActiveReblitReservationSeal,
        installation: &Installation,
        expected: &State,
    ) -> Result<(), Error> {
        installation.revalidate_root_directory()?;
        self.require_active_reblit_state_snapshot(installation, expected)?;
        self.require_active_previous_slot_unchanged_for_reservation(seal, installation, expected.id)
            .map_err(|source| Error::ActivePreviousSlotParking {
                source: Box::new(source),
            })?;
        self.candidate
            .verify_named_read_only(&installation.staging_path("usr"))?;
        self.previous.verify_named_read_only(&installation.root.join("usr"))?;
        self.require_active_previous_slot_unchanged_for_reservation(seal, installation, expected.id)
            .map_err(|source| Error::ActivePreviousSlotParking {
                source: Box::new(source),
            })?;
        self.require_active_reblit_state_snapshot(installation, expected)?;
        installation.revalidate_root_directory().map_err(Into::into)
    }

    fn require_journal_active_reblit_reservation(
        &self,
        seal: &crate::transition_identity::journal_coordinator::ActiveReblitReservationSeal,
        installation: &Installation,
        expected: &State,
        live: bool,
    ) -> Result<(), RetainedActiveReblitReservationEvidenceFailure> {
        installation
            .revalidate_root_directory()
            .map_err(Error::from)
            .map_err(evidence_identity("revalidate reservation installation"))?;
        self.require_active_reblit_state_snapshot(installation, expected)
            .map_err(evidence_identity("revalidate reserved ActiveReblit state"))?;

        let retained = self
            .active_reblit_rotation
            .lock()
            .map_err(|_| ambiguous_replacement(StagingWrapperRotationError::AttemptLockPoisoned))?;
        let rotation = retained
            .as_ref()
            .ok_or_else(|| ambiguous_replacement(StagingWrapperRotationError::AttemptMissing))?;
        rotation
            .require_reserved(installation)
            .map_err(RetainedActiveReblitReservationEvidenceFailure::Replacement)?;
        self.require_active_previous_slot_parked_with_journal(seal, installation, expected.id)
            .map_err(RetainedActiveReblitReservationEvidenceFailure::PreviousSlot)?;

        let candidate_path = if live {
            installation.root.join("usr")
        } else {
            installation.staging_path("usr")
        };
        let previous_path = if live {
            installation.staging_path("usr")
        } else {
            installation.root.join("usr")
        };
        self.verify_candidate_named_with_state_id(&candidate_path)
            .map_err(evidence_identity("authenticate reserved ActiveReblit candidate"))?;
        self.previous
            .verify_named_read_only(&previous_path)
            .map_err(evidence_identity("authenticate reserved ActiveReblit previous tree"))?;
        rotation
            .require_reserved(installation)
            .map_err(RetainedActiveReblitReservationEvidenceFailure::Replacement)?;
        self.require_active_previous_slot_parked_with_journal(seal, installation, expected.id)
            .map_err(RetainedActiveReblitReservationEvidenceFailure::PreviousSlot)?;
        self.require_active_reblit_state_snapshot(installation, expected)
            .map_err(evidence_identity("revalidate final reserved ActiveReblit state"))
    }

    fn require_active_reblit_state_snapshot(&self, installation: &Installation, expected: &State) -> Result<(), Error> {
        let actual = self
            .state_database
            .get(expected.id)
            .map_err(|source| Error::ActiveReblitStateLookup {
                state: i32::from(expected.id),
                source,
            })?;
        if !same_state_snapshot(expected, &actual) {
            return Err(Error::ActiveReblitStateChanged {
                state: i32::from(expected.id),
            });
        }
        if installation.active_state != Some(expected.id) {
            return Err(Error::ActiveReblitSelectionChanged {
                expected: i32::from(expected.id),
                actual: installation.active_state.map(i32::from),
            });
        }
        Ok(())
    }
}

fn not_applied(source: StagingWrapperRotationError) -> RetainedStagingWrapperRotationFailure {
    RetainedStagingWrapperRotationFailure {
        outcome: RetainedStagingWrapperRotationOutcome::NotApplied,
        source,
    }
}

fn ambiguous_replacement(source: StagingWrapperRotationError) -> RetainedActiveReblitReservationEvidenceFailure {
    RetainedActiveReblitReservationEvidenceFailure::Replacement(RetainedStagingWrapperRotationFailure {
        outcome: RetainedStagingWrapperRotationOutcome::Ambiguous,
        source,
    })
}

fn evidence_identity(operation: &'static str) -> impl FnOnce(Error) -> RetainedActiveReblitReservationEvidenceFailure {
    move |source| RetainedActiveReblitReservationEvidenceFailure::Identity { operation, source }
}
