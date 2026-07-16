//! Sealed pre-trigger ActiveReblit capacity reservation.
//!
//! Candidate preparation cannot run triggers for ActiveReblit. This module
//! consumes that non-ready typestate, reserves the exact empty replacement,
//! parks any authenticated second previous-marker link, and only then returns
//! the common trigger-ready authority.

use thiserror::Error;

use crate::{Installation, state::TransitionId, transition_journal::Phase};

use super::super::{Error as IdentityError, staging_wrapper_rotation::ActiveReblitReservationError};
use super::{
    ActiveReblitReservationSeal, PreparedActiveReblitReservationCoordinator, PreparedTransactionTriggerCoordinator,
    StatefulTransitionCoordinatorError, TransactionTriggerReadiness,
};

const RESERVE_ACTIVE_REBLIT: &str = "reserve ActiveReblit pre-trigger capacity";

/// Fail-stop reservation result. No failure variant retains the coordinator,
/// metadata proof, installation capability, or reservation bundle.
#[derive(Debug, Error)]
pub(super) enum ActiveReblitReservationFailure {
    #[error("transition {transition_id} failed ActiveReblit reservation preflight")]
    Preflight {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error("transition {transition_id} failed ActiveReblit namespace reservation")]
    Reservation {
        transition_id: TransitionId,
        #[source]
        source: ActiveReblitReservationError,
    },
    #[error("transition {transition_id} failed final ActiveReblit reservation evidence")]
    FinalEvidence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
}

impl PreparedActiveReblitReservationCoordinator {
    /// Consume CandidatePrepared authority and return trigger-ready authority
    /// only after exact reservation/parking durability is proven.
    pub(super) fn reserve_for_transaction_triggers(
        self,
        installation: &Installation,
    ) -> Result<PreparedTransactionTriggerCoordinator, ActiveReblitReservationFailure> {
        let Self {
            coordinator,
            metadata,
            provenance,
        } = self;
        let transition_id = coordinator.record.transition_id.clone();
        let preflight = |source| ActiveReblitReservationFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };
        coordinator
            .require_operation(
                crate::transition_journal::Operation::ActiveReblit,
                RESERVE_ACTIVE_REBLIT,
            )
            .map_err(preflight)?;
        coordinator
            .require_phase(Phase::CandidatePrepared, RESERVE_ACTIVE_REBLIT)
            .map_err(preflight)?;
        let candidate = coordinator.candidate_state().map_err(preflight)?;
        coordinator
            .require_prepared_metadata_sandwich(candidate, &metadata, &provenance)
            .map_err(preflight)?;
        let expected_state = coordinator
            .identity
            .state_database
            .get(candidate)
            .map_err(|source| IdentityError::ActiveReblitStateLookup {
                state: i32::from(candidate),
                source,
            })
            .map_err(StatefulTransitionCoordinatorError::Identity)
            .map_err(preflight)?;
        coordinator
            .require_prepared_metadata_sandwich(candidate, &metadata, &provenance)
            .map_err(preflight)?;

        let seal = ActiveReblitReservationSeal { _private: () };
        let validate = || coordinator.require_prepared_metadata_sandwich(candidate, &metadata, &provenance);
        let reservation = coordinator
            .identity
            .reserve_active_reblit_with_journal(&seal, installation, expected_state, &validate)
            .map_err(|source| ActiveReblitReservationFailure::Reservation {
                transition_id: transition_id.clone(),
                source,
            })?;

        coordinator
            .require_prepared_metadata_sandwich(candidate, &metadata, &provenance)
            .map_err(|source| ActiveReblitReservationFailure::FinalEvidence {
                transition_id: transition_id.clone(),
                source,
            })?;
        reservation
            .require_staged(&coordinator.identity, &seal)
            .map_err(StatefulTransitionCoordinatorError::ActiveReblitReservation)
            .map_err(|source| ActiveReblitReservationFailure::FinalEvidence { transition_id, source })?;
        Ok(PreparedTransactionTriggerCoordinator {
            coordinator,
            metadata,
            provenance,
            readiness: TransactionTriggerReadiness::ActiveReblit(reservation),
        })
    }
}

impl TransactionTriggerReadiness {
    pub(super) fn require_staged(
        &self,
        identity: &super::super::StatefulTreeIdentity,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        match self {
            Self::NewState => Ok(()),
            Self::ActiveReblit(reservation) => reservation
                .require_staged(identity, &ActiveReblitReservationSeal { _private: () })
                .map_err(StatefulTransitionCoordinatorError::ActiveReblitReservation),
        }
    }

    pub(super) fn require_live(
        &self,
        identity: &super::super::StatefulTreeIdentity,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        match self {
            Self::NewState => Ok(()),
            Self::ActiveReblit(reservation) => reservation
                .require_live(identity, &ActiveReblitReservationSeal { _private: () })
                .map_err(StatefulTransitionCoordinatorError::ActiveReblitReservation),
        }
    }

    pub(super) fn installation(&self) -> Option<&Installation> {
        match self {
            Self::NewState => None,
            Self::ActiveReblit(reservation) => Some(reservation.installation()),
        }
    }
}
