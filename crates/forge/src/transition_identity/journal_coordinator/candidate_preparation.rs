//! Proof-bearing candidate preparation under durable journal authority.
//!
//! Only a coordinator at `CandidatePrepareStarted` can begin the neutral
//! metadata publication. The caller is allowed to interpret the exact
//! descriptor-read `os-info.json` bytes, but never receives the publication
//! capability and therefore cannot manufacture or substitute the proof which
//! authorizes `CandidatePrepared`.

#[cfg(test)]
use crate::transition_journal::TransitionRecord;
use crate::{
    state,
    transition_journal::{Operation, Phase},
};

use super::{
    FINISH_CANDIDATE_PREPARE, StatefulTransitionCoordinator, StatefulTransitionCoordinatorError,
    before_finish_candidate_runtime_proof,
};
use crate::transition_identity::state_tree_metadata::RetainedTreeStateId;
use crate::transition_identity::{CandidateMetadataProof, CandidateMetadataPublication, CandidateMetadataVerification};

/// Operation dispatch after the exact candidate metadata proof has become
/// durable journal evidence.
///
/// The variants contain unforgeable wrappers with private fields. Archived
/// activation deliberately has no transaction-trigger runner.
#[derive(Debug)]
pub(crate) enum PreparedStatefulTransitionCoordinator {
    TransactionTriggers(PreparedTransactionTriggerCoordinator),
    Archived(PreparedArchivedTransitionCoordinator),
}

/// Proof-bearing `NewState` or `ActiveReblit` authority.
#[derive(Debug)]
pub(crate) struct PreparedTransactionTriggerCoordinator {
    pub(super) coordinator: StatefulTransitionCoordinator,
    pub(super) metadata: CandidateMetadataProof,
}

/// Proof-bearing archived-activation authority. This type intentionally has
/// no method capable of running stateful transaction triggers.
#[derive(Debug)]
pub(crate) struct PreparedArchivedTransitionCoordinator {
    coordinator: StatefulTransitionCoordinator,
    metadata: CandidateMetadataProof,
}

/// Proof-bearing authority after transaction triggers are durably complete.
#[derive(Debug)]
pub(crate) struct TransactionTriggersCompleteCoordinator {
    pub(super) coordinator: StatefulTransitionCoordinator,
    pub(super) metadata: CandidateMetadataProof,
}

impl StatefulTransitionCoordinator {
    /// Publish fresh metadata or verify archived metadata, publish the exact
    /// new-state `.stateID`, then durably advance to `CandidatePrepared` while
    /// retaining the sole metadata proof.
    ///
    /// `derive_os_release` sees only the optional bytes read through the
    /// publication's retained `usr/lib` descriptor. It supplies semantic
    /// bytes, not filesystem authority. The coordinator alone consumes the
    /// publication and receives the resulting proof.
    pub(crate) fn finish_candidate_prepare<F>(
        mut self,
        snapshot: &[u8],
        derive_os_release: F,
    ) -> Result<PreparedStatefulTransitionCoordinator, StatefulTransitionCoordinatorError>
    where
        F: FnOnce(Option<&[u8]>) -> Vec<u8>,
    {
        self.require_phase(Phase::CandidatePrepareStarted, FINISH_CANDIDATE_PREPARE)?;
        let candidate = self.candidate_state()?;
        before_finish_candidate_runtime_proof();
        self.require_candidate_prepare_started_evidence(candidate)?;

        let candidate_usr = self.identity.candidate.store.retained_directory();
        let candidate_path = self.identity.candidate.store.display_path();
        // Dispatch is fixed by the durable operation: callers cannot select a
        // mutating publication for archived activation or substitute read-only
        // verification for a candidate that still requires publication.
        let metadata = match self.record.operation {
            Operation::ActivateArchived => {
                let verification = CandidateMetadataVerification::begin(candidate_usr, candidate_path, snapshot)?;
                let os_info = verification.read_optional_os_info()?;
                let os_release = derive_os_release(os_info.as_deref());
                verification.prove(&os_release)?
            }
            Operation::NewState | Operation::ActiveReblit => {
                let publication = CandidateMetadataPublication::begin(candidate_usr, candidate_path, snapshot)?;
                let os_info = publication.read_optional_os_info()?;
                let os_release = derive_os_release(os_info.as_deref());
                publication.publish(&os_release)?
            }
        };

        // Metadata proof must bind to the coordinator's exact candidate before
        // NewState is allowed to publish even its state ID.
        metadata.require_same_candidate(candidate_usr, candidate_path)?;
        self.require_candidate_prepare_started_evidence(candidate)?;
        metadata.require_same_candidate(candidate_usr, candidate_path)?;

        match self.record.operation {
            Operation::NewState => {
                self.identity.candidate.state_id = Some(RetainedTreeStateId::publish_new(
                    &self.identity.candidate.store,
                    candidate,
                )?);
            }
            Operation::ActivateArchived | Operation::ActiveReblit => {}
        }

        // Sandwich the owned proof between complete journal, runtime, public
        // name, database, and state-ID evidence. The final proof check is the
        // last observation before the conditional durable journal advance.
        self.require_prepared_candidate_evidence(candidate)?;
        metadata.require_same_candidate(candidate_usr, candidate_path)?;
        self.require_prepared_candidate_evidence(candidate)?;
        metadata.require_same_candidate(candidate_usr, candidate_path)?;
        self.advance(None)?;

        match self.record.operation {
            Operation::NewState | Operation::ActiveReblit => Ok(
                PreparedStatefulTransitionCoordinator::TransactionTriggers(PreparedTransactionTriggerCoordinator {
                    coordinator: self,
                    metadata,
                }),
            ),
            Operation::ActivateArchived => Ok(PreparedStatefulTransitionCoordinator::Archived(
                PreparedArchivedTransitionCoordinator {
                    coordinator: self,
                    metadata,
                },
            )),
        }
    }

    pub(super) fn candidate_state(&self) -> Result<state::Id, StatefulTransitionCoordinatorError> {
        self.record
            .candidate
            .id
            .map(state::Id::from)
            .ok_or(StatefulTransitionCoordinatorError::CandidateStateMissing {
                phase: self.record.phase,
            })
    }

    fn require_candidate_prepare_started_evidence(
        &self,
        candidate: state::Id,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        self.require_canonical_record()?;
        self.require_record_runtime_evidence()?;
        self.require_candidate_tree_names(candidate, false)?;
        self.require_candidate_database_evidence(candidate)?;
        self.require_record_runtime_evidence()?;
        self.require_candidate_tree_names(candidate, false)?;
        self.require_candidate_database_evidence(candidate)?;
        self.require_canonical_record()
    }

    pub(super) fn require_prepared_candidate_evidence(
        &self,
        candidate: state::Id,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        self.require_canonical_record()?;
        self.require_record_runtime_evidence()?;
        self.require_candidate_tree_names(candidate, true)?;
        self.require_candidate_database_evidence(candidate)?;
        self.require_record_runtime_evidence()?;
        self.require_candidate_tree_names(candidate, true)?;
        self.require_candidate_database_evidence(candidate)?;
        self.require_canonical_record()
    }

    fn require_canonical_record(&self) -> Result<(), StatefulTransitionCoordinatorError> {
        let actual = self.identity.journal.load()?;
        if actual.as_ref() == Some(&self.record) {
            Ok(())
        } else {
            Err(StatefulTransitionCoordinatorError::CanonicalRecordChanged {
                transition_id: self.record.transition_id.clone(),
                expected_phase: self.record.phase,
                actual,
            })
        }
    }

    fn require_candidate_tree_names(
        &self,
        candidate: state::Id,
        state_id_published: bool,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        let candidate_path = self.identity.candidate.store.display_path();
        let previous_path = self.identity.previous.store.display_path();
        if state_id_published || self.record.operation != Operation::NewState {
            self.identity.require_existing_candidate_state(candidate)?;
            self.identity
                .candidate
                .verify_named_with_state_id(candidate_path)
                .map_err(StatefulTransitionCoordinatorError::Identity)?;
        } else {
            self.identity.require_unallocated_candidate()?;
            self.identity
                .candidate
                .verify_named_read_only(candidate_path)
                .map_err(StatefulTransitionCoordinatorError::Identity)?;
            RetainedTreeStateId::require_absent(&self.identity.candidate.store)
                .map_err(StatefulTransitionCoordinatorError::Identity)?;
        }
        self.identity
            .previous
            .verify_named_read_only(previous_path)
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
        if state_id_published || self.record.operation != Operation::NewState {
            self.identity
                .candidate
                .verify_named_with_state_id(candidate_path)
                .map_err(StatefulTransitionCoordinatorError::Identity)
        } else {
            self.identity
                .candidate
                .verify_named_read_only(candidate_path)
                .map_err(StatefulTransitionCoordinatorError::Identity)
        }
    }

    fn require_candidate_database_evidence(
        &self,
        candidate: state::Id,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        match self.record.operation {
            Operation::NewState => self.require_fresh_allocation_ownership(candidate),
            Operation::ActivateArchived | Operation::ActiveReblit => {
                self.identity.require_existing_candidate_database_ownership(
                    self.record.operation,
                    candidate,
                    &self.record.transition_id,
                )
            }
        }?;
        self.identity.require_previous_state_database_ownership(
            self.record.operation,
            self.record.previous.id.map(state::Id::from),
            Some(candidate),
            &self.record.transition_id,
        )?;
        let expected = match self.record.operation {
            Operation::NewState => Some((candidate, &self.record.transition_id)),
            Operation::ActivateArchived | Operation::ActiveReblit => None,
        };
        self.identity
            .require_global_transition_audit(self.record.operation, expected)
    }
}

impl PreparedStatefulTransitionCoordinator {
    #[cfg(test)]
    pub(crate) fn record(&self) -> &TransitionRecord {
        match self {
            Self::TransactionTriggers(prepared) => prepared.record(),
            Self::Archived(prepared) => prepared.record(),
        }
    }
}

impl PreparedTransactionTriggerCoordinator {
    #[cfg(test)]
    pub(crate) fn record(&self) -> &TransitionRecord {
        &self.coordinator.record
    }
}

impl PreparedArchivedTransitionCoordinator {
    #[cfg(test)]
    pub(crate) fn record(&self) -> &TransitionRecord {
        // Keeping the proof borrowed here ensures the archived typestate
        // cannot accidentally become a proof-free coordinator accessor.
        let _metadata = &self.metadata;
        &self.coordinator.record
    }
}

impl TransactionTriggersCompleteCoordinator {
    #[cfg(test)]
    pub(crate) fn record(&self) -> &TransitionRecord {
        let _metadata = &self.metadata;
        &self.coordinator.record
    }
}
