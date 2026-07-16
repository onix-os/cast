//! Durable prefix shared by every stateful `/usr` transition.
//!
//! This module owns the shared contract through `CandidatePrepared` and the
//! stateful transaction-trigger boundary for `NewState` and `ActiveReblit`,
//! then records the common intent-only `/usr` exchange boundary.
//! It is not wired into live activation until startup can reconcile every
//! record it may publish. Consuming transitions deliberately make an uncertain
//! journal write fail-stop: an error drops the coordinator and its lock rather
//! than allowing another in-process effect to guess which record became
//! durable. The transaction-trigger runner is deliberately visible only inside
//! this contract module. Candidate preparation transfers its owned metadata
//! proof into an operation-specific typestate: only `NewState` and
//! `ActiveReblit` can reach transaction triggers, while archived activation
//! cannot. Live wiring remains deferred until startup reconciliation consumes
//! every record this contract may publish.

mod active_reblit_reservation;
mod candidate_preparation;
mod error;
mod request;
mod transaction_triggers;
mod usr_exchange_effect;
mod usr_exchange_intent;

#[cfg(test)]
mod tests;

#[cfg(test)]
use active_reblit_reservation::ActiveReblitReservationFailure;
use candidate_preparation::TransactionTriggerReadiness;
#[allow(unused_imports)] // contract-only typestates until live lifecycle wiring
pub(crate) use candidate_preparation::{
    PreparedActiveReblitReservationCoordinator, PreparedArchivedTransitionCoordinator,
    PreparedStatefulTransitionCoordinator, PreparedTransactionTriggerCoordinator,
    TransactionTriggersCompleteCoordinator,
};
pub(crate) use error::StatefulTransitionCoordinatorError;
pub(crate) use request::{NewStatePrevious, StatefulTransitionRequest};
#[cfg(test)]
use transaction_triggers::StatefulTransactionTriggerFailure;
#[cfg(test)]
#[allow(unused_imports)] // imported into the descendant contract tests
use usr_exchange_effect::UsrExchangeEffectFailure;
#[allow(unused_imports)] // contract-only until live recovery executes this phase
pub(crate) use usr_exchange_effect::UsrExchangedCoordinator;
#[allow(unused_imports)] // contract-only typestate until the exchange effect exists
pub(crate) use usr_exchange_intent::UsrExchangeIntentCoordinator;
#[cfg(test)]
use usr_exchange_intent::UsrExchangeIntentFailure;

use crate::{
    db,
    state::{self, TransitionId},
    transition_journal::{
        Operation, Phase, Previous, PreviousOrigin, QuarantineName, RuntimeEpoch, RuntimeTreeIdentity, TransitionRecord,
    },
};

use super::{
    RetainedPreviousClassification, StatefulTreeIdentity, candidate_state_authority::RetainedCandidateStateId,
};

/// Unforgeable token accepted only by the coordinator-aware one-shot exchange
/// core.  Its private field can be constructed only inside this module and its
/// descendants; legacy callers cannot use it to bypass journal-absence guards.
#[derive(Debug)]
pub(super) struct UsrExchangeEffectSeal {
    _private: (),
}

/// Private capability accepted only by journal-aware ActiveReblit reservation
/// primitives. Legacy lifecycle methods cannot construct it and retain their
/// strict clean-baseline and journal-absence guards.
#[derive(Debug)]
pub(super) struct ActiveReblitReservationSeal {
    _private: (),
}

const BEGIN_FRESH_ALLOCATION: &str = "begin fresh-state allocation";
const FINISH_FRESH_ALLOCATION: &str = "finish fresh-state allocation";
const BEGIN_CANDIDATE_PREPARE: &str = "begin candidate preparation";
const FINISH_CANDIDATE_PREPARE: &str = "finish candidate preparation";

/// Exclusive, fail-stop owner of one durable state-transition prefix.
///
/// The retained identity is intentionally private and this type does not
/// implement `Deref`. Existing tree effects still require journal absence;
/// later integration must expose phase-authorized coordinator operations
/// rather than weakening those guards.
#[derive(Debug)]
pub(crate) struct StatefulTransitionCoordinator {
    identity: StatefulTreeIdentity,
    record: TransitionRecord,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CapturedRuntimeEvidence {
    epoch: RuntimeEpoch,
    candidate: RuntimeTreeIdentity,
    previous: RuntimeTreeIdentity,
}

impl StatefulTreeIdentity {
    /// Persist the immutable `Preparing` record and transfer exclusive tree
    /// authority into the durable coordinator.
    pub(crate) fn begin_transition(
        self,
        request: StatefulTransitionRequest,
    ) -> Result<StatefulTransitionCoordinator, StatefulTransitionCoordinatorError> {
        self.require_no_journal()
            .map_err(StatefulTransitionCoordinatorError::Identity)?;

        let parts = request.parts();
        self.require_request_previous(parts)?;
        self.require_request_candidate(parts)?;

        let runtime = capture_runtime_evidence(&self)?;
        let transition_id =
            TransitionId::generate().map_err(StatefulTransitionCoordinatorError::GenerateTransitionId)?;
        if let Some(candidate) = parts.candidate_id {
            self.require_existing_candidate_database_ownership(parts.operation, candidate, &transition_id)?;
        }
        self.require_previous_state_database_ownership(
            parts.operation,
            parts.previous_id,
            parts.candidate_id,
            &transition_id,
        )?;
        self.require_global_transition_audit(parts.operation, None)?;
        let quarantine_name = transition_quarantine_name(&transition_id)?;
        let record = TransitionRecord::preparing(
            transition_id,
            runtime.epoch.clone(),
            parts.operation,
            parts.candidate_id.map(i32::from),
            self.candidate.marker.token().clone(),
            runtime.candidate,
            Previous {
                id: parts.previous_id.map(i32::from),
                tree_token: self.previous.marker.token().clone(),
                usr_runtime_identity: runtime.previous,
                origin: parts.previous_origin,
            },
            parts.run_system_triggers,
            parts.run_boot_sync,
            quarantine_name,
        )?;

        // Epoch/tree capture may traverse authenticated procfs and therefore
        // is not treated as instantaneous. Repeat every retained witness and
        // the journal-absence proof immediately before durable creation.
        require_record_runtime_evidence(&self, &record)?;
        self.require_request_candidate(parts)?;
        if let Some(candidate) = parts.candidate_id {
            self.require_existing_candidate_database_ownership(parts.operation, candidate, &record.transition_id)?;
        }
        self.require_previous_state_database_ownership(
            parts.operation,
            parts.previous_id,
            parts.candidate_id,
            &record.transition_id,
        )?;
        self.require_global_transition_audit(parts.operation, None)?;
        self.require_no_journal()
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
        self.journal.create(&record)?;
        Ok(StatefulTransitionCoordinator { identity: self, record })
    }

    fn require_request_candidate(
        &self,
        parts: request::RequestParts,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        match (parts.operation, parts.candidate_id) {
            (Operation::NewState, None) => self.require_unknown_id_absent(Operation::NewState),
            (Operation::ActivateArchived, Some(candidate)) => {
                self.require_existing_candidate_id(Operation::ActivateArchived, candidate)
            }
            (Operation::ActiveReblit, Some(candidate)) => {
                self.require_known_id_absent(Operation::ActiveReblit, candidate)
            }
            (operation, candidate) => {
                Err(self.candidate_authority_mismatch(operation, "operation-specific", candidate))
            }
        }
    }

    fn require_unknown_id_absent(&self, operation: Operation) -> Result<(), StatefulTransitionCoordinatorError> {
        if !matches!(self.candidate_state_id, RetainedCandidateStateId::UnknownIdAbsent) {
            return Err(self.candidate_authority_mismatch(operation, "unknown-ID/absent", None));
        }
        self.candidate_state_id
            .verify_initial(&self.candidate)
            .map_err(StatefulTransitionCoordinatorError::Identity)
    }

    fn require_known_id_absent(
        &self,
        operation: Operation,
        expected: state::Id,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        if !matches!(
            self.candidate_state_id,
            RetainedCandidateStateId::KnownIdAbsent(actual) if actual == expected
        ) {
            return Err(self.candidate_authority_mismatch(operation, "known-ID/absent", Some(expected)));
        }
        self.candidate_state_id
            .verify_initial(&self.candidate)
            .map_err(StatefulTransitionCoordinatorError::Identity)
    }

    fn require_existing_candidate_id(
        &self,
        operation: Operation,
        expected: state::Id,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        if !matches!(
            &self.candidate_state_id,
            RetainedCandidateStateId::ExistingId(actual) if actual.state() == expected
        ) {
            return Err(self.candidate_authority_mismatch(operation, "existing-ID", Some(expected)));
        }
        self.verify_candidate_store_with_state_id()
            .map_err(StatefulTransitionCoordinatorError::Identity)
    }

    fn candidate_authority_mismatch(
        &self,
        operation: Operation,
        expected_kind: &'static str,
        expected_state: Option<state::Id>,
    ) -> StatefulTransitionCoordinatorError {
        let (retained_kind, retained_state) = self.candidate_state_id.kind_and_state();
        StatefulTransitionCoordinatorError::CandidateAuthorityMismatch {
            operation,
            expected_kind,
            expected_state: expected_state.map(i32::from),
            retained_kind,
            retained_state: retained_state.map(i32::from),
        }
    }

    fn require_existing_candidate_database_ownership(
        &self,
        operation: Operation,
        candidate: state::Id,
        transition: &TransitionId,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        debug_assert!(matches!(
            operation,
            Operation::ActivateArchived | Operation::ActiveReblit
        ));
        let ownership = self.state_database.transition_ownership(candidate, transition)?;
        if ownership == db::state::TransitionOwnership::Cleared {
            Ok(())
        } else {
            Err(StatefulTransitionCoordinatorError::ExistingCandidateOwnershipMismatch {
                operation,
                state: i32::from(candidate),
                ownership,
            })
        }
    }

    fn require_previous_state_database_ownership(
        &self,
        operation: Operation,
        previous: Option<state::Id>,
        candidate: Option<state::Id>,
        transition: &TransitionId,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        let Some(previous) = previous.filter(|previous| Some(*previous) != candidate) else {
            return Ok(());
        };
        let ownership = self.state_database.transition_ownership(previous, transition)?;
        if ownership == db::state::TransitionOwnership::Cleared {
            Ok(())
        } else {
            Err(StatefulTransitionCoordinatorError::PreviousStateOwnershipMismatch {
                operation,
                state: i32::from(previous),
                ownership,
            })
        }
    }

    fn require_global_transition_audit(
        &self,
        operation: Operation,
        expected: Option<(state::Id, &TransitionId)>,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        let actual = self.state_database.audit_in_flight_transition()?;
        let matches = match (expected, actual.as_ref()) {
            (None, None) => true,
            (Some((expected_state, expected_transition)), Some(actual)) => {
                actual.state_id == expected_state && actual.transition_id == *expected_transition
            }
            _ => false,
        };
        if matches {
            return Ok(());
        }
        Err(StatefulTransitionCoordinatorError::TransitionAuditMismatch {
            operation,
            expected_state: expected.map(|(state, _)| i32::from(state)),
            expected_transition: expected.map(|(_, transition)| transition.clone()),
            actual,
        })
    }

    fn require_request_previous(&self, parts: request::RequestParts) -> Result<(), StatefulTransitionCoordinatorError> {
        if parts.previous_origin == PreviousOrigin::Unmanaged {
            return Err(StatefulTransitionCoordinatorError::UnmanagedPreviousUnsupported);
        }

        let (retained_origin, retained_state) = match self.previous_classification {
            RetainedPreviousClassification::Active(state) => (PreviousOrigin::ActiveState, Some(state)),
            RetainedPreviousClassification::SynthesizedEmpty => (PreviousOrigin::SynthesizedEmpty, None),
        };
        let matches = match (parts.operation, self.previous_classification) {
            (Operation::NewState, RetainedPreviousClassification::Active(state)) => {
                parts.previous_origin == PreviousOrigin::ActiveState && parts.previous_id == Some(state)
            }
            (Operation::NewState, RetainedPreviousClassification::SynthesizedEmpty) => {
                parts.previous_origin == PreviousOrigin::SynthesizedEmpty && parts.previous_id.is_none()
            }
            (Operation::ActivateArchived, RetainedPreviousClassification::Active(state)) => {
                parts.previous_origin == PreviousOrigin::ActiveState && parts.previous_id == Some(state)
            }
            (Operation::ActiveReblit, RetainedPreviousClassification::Active(state)) => {
                parts.previous_origin == PreviousOrigin::ActiveReblitCorrupt
                    && parts.previous_id == Some(state)
                    && parts.candidate_id == Some(state)
            }
            (
                Operation::ActivateArchived | Operation::ActiveReblit,
                RetainedPreviousClassification::SynthesizedEmpty,
            ) => false,
        };
        if matches {
            Ok(())
        } else {
            Err(StatefulTransitionCoordinatorError::PreviousClassificationMismatch {
                operation: parts.operation,
                request_origin: parts.previous_origin,
                request_state: parts.previous_id.map(i32::from),
                retained_origin,
                retained_state: retained_state.map(i32::from),
            })
        }
    }
}

impl StatefulTransitionCoordinator {
    /// Advance `NewState` from `Preparing` to the durable allocation intent.
    pub(crate) fn begin_fresh_allocation(mut self) -> Result<Self, StatefulTransitionCoordinatorError> {
        self.require_operation(Operation::NewState, BEGIN_FRESH_ALLOCATION)?;
        self.require_phase(Phase::Preparing, BEGIN_FRESH_ALLOCATION)?;
        self.identity.require_unknown_id_absent(Operation::NewState)?;
        self.require_record_runtime_evidence()?;
        self.identity.require_unknown_id_absent(Operation::NewState)?;
        self.advance(None)?;
        Ok(self)
    }

    /// Return the database correlation token only while fresh allocation is
    /// durably authorized. Existing-state operations can never obtain it.
    pub(crate) fn transition_id_for_allocation(&self) -> Result<&TransitionId, StatefulTransitionCoordinatorError> {
        self.require_operation(Operation::NewState, BEGIN_FRESH_ALLOCATION)?;
        self.require_phase(Phase::FreshStateAllocating, BEGIN_FRESH_ALLOCATION)?;
        Ok(&self.record.transition_id)
    }

    /// Persist allocation completion only after the exact state row proves it
    /// carries this journal's transition ID.
    pub(crate) fn finish_fresh_allocation(
        mut self,
        database: &db::state::Database,
        state: state::Id,
    ) -> Result<Self, StatefulTransitionCoordinatorError> {
        self.require_operation(Operation::NewState, FINISH_FRESH_ALLOCATION)?;
        self.require_phase(Phase::FreshStateAllocating, FINISH_FRESH_ALLOCATION)?;
        if !self.identity.state_database.same_instance(database) {
            return Err(StatefulTransitionCoordinatorError::StateDatabaseCapabilityMismatch);
        }
        self.identity.require_unknown_id_absent(Operation::NewState)?;
        self.require_record_runtime_evidence()?;

        let ownership = self
            .identity
            .state_database
            .transition_ownership(state, &self.record.transition_id)?;
        if ownership != db::state::TransitionOwnership::Matching {
            return Err(StatefulTransitionCoordinatorError::FreshAllocationOwnershipMismatch {
                state: i32::from(state),
                ownership,
            });
        }

        // Sandwich the database observation between retained runtime proofs.
        // The installation-wide cooperating-writer lock supplies the mutation
        // exclusion; this second proof rejects an identity/epoch change.
        self.require_record_runtime_evidence()?;
        self.identity.require_unknown_id_absent(Operation::NewState)?;
        self.require_fresh_allocation_ownership(state)?;
        self.advance(Some(i32::from(state)))?;
        self.identity.candidate_state_id = RetainedCandidateStateId::KnownIdAbsent(state);
        Ok(self)
    }

    /// Persist candidate-preparation intent from the operation-specific exact
    /// predecessor. New states must first complete correlated DB allocation.
    pub(crate) fn begin_candidate_prepare(mut self) -> Result<Self, StatefulTransitionCoordinatorError> {
        let expected = match self.record.operation {
            Operation::NewState => Phase::FreshStateAllocated,
            Operation::ActivateArchived | Operation::ActiveReblit => Phase::Preparing,
        };
        self.require_phase(expected, BEGIN_CANDIDATE_PREPARE)?;
        let candidate = self.candidate_state()?;
        self.require_candidate_before_publication(candidate)?;
        self.require_record_runtime_evidence()?;
        self.require_candidate_before_publication(candidate)?;
        match self.record.operation {
            Operation::NewState => {
                self.require_fresh_allocation_ownership(candidate)?;
                self.require_record_runtime_evidence()?;
                self.identity.require_known_id_absent(Operation::NewState, candidate)?;
                self.require_fresh_allocation_ownership(candidate)?;
            }
            Operation::ActivateArchived | Operation::ActiveReblit => {
                self.identity.require_existing_candidate_database_ownership(
                    self.record.operation,
                    candidate,
                    &self.record.transition_id,
                )?;
            }
        }
        self.advance(None)?;
        Ok(self)
    }

    fn require_candidate_before_publication(
        &self,
        candidate: state::Id,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        match self.record.operation {
            Operation::NewState => self.identity.require_known_id_absent(Operation::NewState, candidate),
            Operation::ActivateArchived => self
                .identity
                .require_existing_candidate_id(Operation::ActivateArchived, candidate),
            Operation::ActiveReblit => self
                .identity
                .require_known_id_absent(Operation::ActiveReblit, candidate),
        }
    }

    fn require_fresh_allocation_ownership(&self, state: state::Id) -> Result<(), StatefulTransitionCoordinatorError> {
        let ownership = self
            .identity
            .state_database
            .transition_ownership(state, &self.record.transition_id)?;
        if ownership == db::state::TransitionOwnership::Matching {
            Ok(())
        } else {
            Err(StatefulTransitionCoordinatorError::FreshAllocationOwnershipMismatch {
                state: i32::from(state),
                ownership,
            })
        }
    }

    fn require_operation(
        &self,
        expected: Operation,
        action: &'static str,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        if self.record.operation == expected {
            Ok(())
        } else {
            Err(StatefulTransitionCoordinatorError::UnexpectedOperation {
                action,
                expected,
                actual: self.record.operation,
            })
        }
    }

    fn require_phase(&self, expected: Phase, action: &'static str) -> Result<(), StatefulTransitionCoordinatorError> {
        if self.record.phase == expected {
            Ok(())
        } else {
            Err(StatefulTransitionCoordinatorError::UnexpectedPhase {
                action,
                expected,
                actual: self.record.phase,
            })
        }
    }

    fn require_record_runtime_evidence(&self) -> Result<(), StatefulTransitionCoordinatorError> {
        require_record_runtime_evidence(&self.identity, &self.record)
    }

    fn advance(&mut self, candidate: Option<i32>) -> Result<(), StatefulTransitionCoordinatorError> {
        let next = self.record.forward_successor(candidate)?;
        self.identity.journal.advance(&self.record, &next)?;
        self.record = next;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn record(&self) -> &TransitionRecord {
        &self.record
    }
}

fn capture_runtime_evidence(
    identity: &StatefulTreeIdentity,
) -> Result<CapturedRuntimeEvidence, StatefulTransitionCoordinatorError> {
    identity
        .candidate
        .revalidate_retained()
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    identity
        .previous
        .revalidate_retained()
        .map_err(StatefulTransitionCoordinatorError::Identity)?;

    let before = RuntimeEpoch::capture()?;
    let candidate = RuntimeTreeIdentity::capture_directory(identity.candidate.store.retained_directory())?;
    let previous = RuntimeTreeIdentity::capture_directory(identity.previous.store.retained_directory())?;
    let after = RuntimeEpoch::capture()?;

    identity
        .candidate
        .revalidate_retained()
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    identity
        .previous
        .revalidate_retained()
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    if before != after {
        return Err(StatefulTransitionCoordinatorError::RuntimeEpochChanged);
    }
    Ok(CapturedRuntimeEvidence {
        epoch: before,
        candidate,
        previous,
    })
}

fn require_record_runtime_evidence(
    identity: &StatefulTreeIdentity,
    record: &TransitionRecord,
) -> Result<(), StatefulTransitionCoordinatorError> {
    let actual = capture_runtime_evidence(identity)?;
    if actual.epoch != record.creation_epoch {
        return Err(StatefulTransitionCoordinatorError::RuntimeEpochChanged);
    }
    if actual.candidate != record.candidate.usr_runtime_identity {
        return Err(StatefulTransitionCoordinatorError::RuntimeTreeIdentityChanged { tree: "candidate" });
    }
    if actual.previous != record.previous.usr_runtime_identity {
        return Err(StatefulTransitionCoordinatorError::RuntimeTreeIdentityChanged { tree: "previous" });
    }
    Ok(())
}

fn transition_quarantine_name(transition: &TransitionId) -> Result<QuarantineName, StatefulTransitionCoordinatorError> {
    QuarantineName::parse(format!("failed-transition-{transition}")).map_err(StatefulTransitionCoordinatorError::Record)
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINISH_CANDIDATE_RUNTIME_PROOF: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_before_finish_candidate_runtime_proof(hook: impl FnOnce() + 'static) {
    BEFORE_FINISH_CANDIDATE_RUNTIME_PROOF.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_finish_candidate_runtime_proof() {
    BEFORE_FINISH_CANDIDATE_RUNTIME_PROOF.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_finish_candidate_runtime_proof() {}
