use std::io;

use thiserror::Error;

use crate::{
    db,
    state::TransitionId,
    transition_journal::{
        CodecError, Operation, Phase, PreviousOrigin, RuntimeEvidenceError, StorageError, TransitionRecord,
    },
};

use super::super::{CandidateInventoryError, CandidateMetadataError};

#[derive(Debug, Error)]
pub(crate) enum StatefulTransitionCoordinatorError {
    #[error("generate a durable state-transition ID from the kernel CSPRNG")]
    GenerateTransitionId(#[source] io::Error),
    #[error("capture state-transition runtime evidence")]
    RuntimeEvidence(#[from] RuntimeEvidenceError),
    #[error("the boot or mount-namespace epoch changed while transition runtime evidence was captured")]
    RuntimeEpochChanged,
    #[error("the retained {tree} /usr runtime identity changed after journal creation")]
    RuntimeTreeIdentityChanged { tree: &'static str },
    #[error("revalidate retained state-transition tree identity")]
    Identity(#[source] super::super::Error),
    #[error("construct or advance the canonical state-transition record")]
    Record(#[from] CodecError),
    #[error("create or advance the durable state-transition journal")]
    Journal(#[from] StorageError),
    #[error(
        "canonical journal for transition {transition_id} changed while {expected_phase:?} authority was retained: {actual:?}"
    )]
    CanonicalRecordChanged {
        transition_id: TransitionId,
        expected_phase: Phase,
        actual: Option<TransitionRecord>,
    },
    #[error("publish or revalidate exact candidate metadata under CandidatePrepareStarted authority")]
    CandidateMetadata(#[from] CandidateMetadataError),
    #[error("{action} requires operation {expected:?}, found {actual:?}")]
    UnexpectedOperation {
        action: &'static str,
        expected: Operation,
        actual: Operation,
    },
    #[error("{action} requires journal phase {expected:?}, found {actual:?}")]
    UnexpectedPhase {
        action: &'static str,
        expected: Phase,
        actual: Phase,
    },
    #[error(
        "{operation:?} requires candidate authority {expected_kind} state={expected_state:?}, retained {retained_kind} state={retained_state:?}"
    )]
    CandidateAuthorityMismatch {
        operation: Operation,
        expected_kind: &'static str,
        expected_state: Option<i32>,
        retained_kind: &'static str,
        retained_state: Option<i32>,
    },
    #[error("the {phase:?} journal record does not contain a candidate state ID")]
    CandidateStateMissing { phase: Phase },
    #[error(
        "{operation:?} previous-tree request ({request_origin:?}, state={request_state:?}) does not match retained classification ({retained_origin:?}, state={retained_state:?})"
    )]
    PreviousClassificationMismatch {
        operation: Operation,
        request_origin: PreviousOrigin,
        request_state: Option<i32>,
        retained_origin: PreviousOrigin,
        retained_state: Option<i32>,
    },
    #[error("NewState previous origin Unmanaged is not authenticated by current identity preparation")]
    UnmanagedPreviousUnsupported,
    #[error(
        "fresh-state completion was offered a different state database capability than identity preparation retained"
    )]
    StateDatabaseCapabilityMismatch,
    #[error("inspect exact fresh-state transition ownership")]
    StateEvidence(#[from] db::state::TransitionEvidenceError),
    #[error("fresh state {state} has {ownership:?} transition ownership instead of Matching")]
    FreshAllocationOwnershipMismatch {
        state: i32,
        ownership: db::state::TransitionOwnership,
    },
    #[error("{operation:?} candidate state {state} has {ownership:?} transition ownership instead of Cleared")]
    ExistingCandidateOwnershipMismatch {
        operation: Operation,
        state: i32,
        ownership: db::state::TransitionOwnership,
    },
    #[error("{operation:?} previous state {state} has {ownership:?} transition ownership instead of Cleared")]
    PreviousStateOwnershipMismatch {
        operation: Operation,
        state: i32,
        ownership: db::state::TransitionOwnership,
    },
    #[error(
        "{operation:?} global transition audit mismatch: expected state={expected_state:?}, transition={expected_transition:?}; found {actual:?}"
    )]
    TransitionAuditMismatch {
        operation: Operation,
        expected_state: Option<i32>,
        expected_transition: Option<TransitionId>,
        actual: Option<db::state::InFlightTransition>,
    },
    #[error("durably seal the exact existing marked candidate before a forward journal boundary")]
    PreparedCandidateDurability(#[source] CandidateInventoryError),
    #[error("publish the exact absent candidate state ID under CandidatePrepareStarted authority")]
    StateIdPublication(#[from] super::super::state_tree_metadata::StateIdPublicationFailure),
}
