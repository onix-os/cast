use std::io;

use thiserror::Error;

use crate::{
    db,
    transition_journal::{CodecError, Operation, Phase, PreviousOrigin, RuntimeEvidenceError, StorageError},
};

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
    #[error("candidate state identity mismatch: expected {expected}, found {actual:?}")]
    CandidateStateMismatch { expected: i32, actual: Option<i32> },
    #[error("NewState requires a state-ID-unallocated candidate, found retained state ID {actual:?}")]
    NewStateCandidateAlreadyDecorated { actual: Option<i32> },
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
    #[error("publish the exact fresh-state ID under CandidatePrepareStarted authority")]
    StateIdPublication(#[from] super::super::state_tree_metadata::StateIdPublicationFailure),
}
