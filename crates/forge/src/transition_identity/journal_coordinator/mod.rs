//! Durable prefix shared by every stateful `/usr` transition.
//!
//! This module owns only the contract through `CandidatePrepared`. It is not
//! wired into live activation until startup can reconcile every record it may
//! publish. Consuming transitions deliberately make an uncertain journal
//! write fail-stop: an error drops the coordinator and its lock rather than
//! allowing another in-process effect to guess which record became durable.

mod error;
mod request;

#[cfg(test)]
mod tests;

pub(crate) use error::StatefulTransitionCoordinatorError;
pub(crate) use request::{NewStatePrevious, StatefulTransitionRequest};

use crate::{
    db,
    state::{self, TransitionId},
    transition_journal::{
        Operation, Phase, Previous, PreviousOrigin, QuarantineName, RuntimeEpoch, RuntimeTreeIdentity, TransitionRecord,
    },
};

use super::{RetainedPreviousClassification, StatefulTreeIdentity, state_tree_metadata::RetainedTreeStateId};

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
        match parts.candidate_id {
            Some(candidate) => self.require_existing_candidate_state(candidate)?,
            None => self.require_unallocated_candidate()?,
        }

        let runtime = capture_runtime_evidence(&self)?;
        let transition_id =
            TransitionId::generate().map_err(StatefulTransitionCoordinatorError::GenerateTransitionId)?;
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
        match parts.candidate_id {
            Some(candidate) => self.require_existing_candidate_state(candidate)?,
            None => self.require_unallocated_candidate()?,
        }
        self.require_no_journal()
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
        self.journal.create(&record)?;
        Ok(StatefulTransitionCoordinator { identity: self, record })
    }

    fn require_existing_candidate_state(&self, expected: state::Id) -> Result<(), StatefulTransitionCoordinatorError> {
        let actual = self.candidate.state_id.as_ref().map(RetainedTreeStateId::state);
        if actual != Some(expected) {
            return Err(StatefulTransitionCoordinatorError::CandidateStateMismatch {
                expected: i32::from(expected),
                actual: actual.map(i32::from),
            });
        }
        self.candidate
            .verify_store_with_state_id(&self.candidate.store)
            .map_err(StatefulTransitionCoordinatorError::Identity)
    }

    fn require_unallocated_candidate(&self) -> Result<(), StatefulTransitionCoordinatorError> {
        let actual = self.candidate.state_id.as_ref().map(RetainedTreeStateId::state);
        if actual.is_some() {
            return Err(StatefulTransitionCoordinatorError::NewStateCandidateAlreadyDecorated {
                actual: actual.map(i32::from),
            });
        }
        self.candidate
            .verify_store_read_only(&self.candidate.store)
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
        RetainedTreeStateId::require_absent(&self.candidate.store).map_err(StatefulTransitionCoordinatorError::Identity)
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
        self.identity.require_unallocated_candidate()?;
        self.require_record_runtime_evidence()?;
        self.identity.require_unallocated_candidate()?;
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
        self.identity.require_unallocated_candidate()?;
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
        self.identity.require_unallocated_candidate()?;
        self.require_fresh_allocation_ownership(state)?;
        self.advance(Some(i32::from(state)))?;
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
        if self.record.operation == Operation::NewState {
            self.identity.require_unallocated_candidate()?;
        }
        self.require_record_runtime_evidence()?;
        if self.record.operation == Operation::NewState {
            let candidate = self.fresh_candidate_state()?;
            self.require_fresh_allocation_ownership(candidate)?;
            self.require_record_runtime_evidence()?;
            self.identity.require_unallocated_candidate()?;
            self.require_fresh_allocation_ownership(candidate)?;
        }
        self.advance(None)?;
        Ok(self)
    }

    /// Publish or revalidate the candidate's exact `.stateID`, then persist
    /// `CandidatePrepared`. NewState publication is authorized only by the
    /// durable `CandidatePrepareStarted` record.
    pub(crate) fn finish_candidate_prepare(mut self) -> Result<Self, StatefulTransitionCoordinatorError> {
        self.require_phase(Phase::CandidatePrepareStarted, FINISH_CANDIDATE_PREPARE)?;
        let candidate = self.record.candidate.id.map(state::Id::from).ok_or(
            StatefulTransitionCoordinatorError::CandidateStateMissing {
                phase: self.record.phase,
            },
        )?;
        before_finish_candidate_runtime_proof();
        self.require_record_runtime_evidence()?;
        match self.record.operation {
            Operation::NewState => {
                self.require_fresh_allocation_ownership(candidate)?;
                self.identity.require_unallocated_candidate()?;
                self.identity.candidate.state_id = Some(RetainedTreeStateId::publish_new(
                    &self.identity.candidate.store,
                    candidate,
                )?);
                self.require_fresh_allocation_ownership(candidate)?;
            }
            Operation::ActivateArchived | Operation::ActiveReblit => {
                self.identity.require_existing_candidate_state(candidate)?;
            }
        }
        self.require_record_runtime_evidence()?;
        self.identity
            .candidate
            .verify_store_with_state_id(&self.identity.candidate.store)
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
        if self.record.operation == Operation::NewState {
            self.require_fresh_allocation_ownership(candidate)?;
        }
        self.advance(None)?;
        Ok(self)
    }

    fn fresh_candidate_state(&self) -> Result<state::Id, StatefulTransitionCoordinatorError> {
        self.record
            .candidate
            .id
            .map(state::Id::from)
            .ok_or(StatefulTransitionCoordinatorError::CandidateStateMissing {
                phase: self.record.phase,
            })
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
