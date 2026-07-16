use crate::{
    state,
    transition_journal::{Operation, PreviousOrigin},
};

/// Strict classification of the tree replaced by a fresh state.
///
/// Encoding the state-ID relationship in the enum keeps impossible
/// `(PreviousOrigin, Option<state::Id>)` pairs out of coordinator callers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NewStatePrevious {
    Active(state::Id),
    SynthesizedEmpty,
    Unmanaged,
}

/// Immutable intent required to create the first durable transition record.
///
/// The request does not accept `archive_previous`: that option is derived
/// exclusively from the authenticated previous-tree classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StatefulTransitionRequest {
    NewState {
        previous: NewStatePrevious,
        run_system_triggers: bool,
        run_boot_sync: bool,
    },
    ActivateArchived {
        candidate: state::Id,
        previous: state::Id,
        run_system_triggers: bool,
        run_boot_sync: bool,
    },
    ActiveReblit {
        state: state::Id,
        run_system_triggers: bool,
        run_boot_sync: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RequestParts {
    pub(super) operation: Operation,
    pub(super) candidate_id: Option<state::Id>,
    pub(super) previous_id: Option<state::Id>,
    pub(super) previous_origin: PreviousOrigin,
    pub(super) run_system_triggers: bool,
    pub(super) run_boot_sync: bool,
}

impl StatefulTransitionRequest {
    pub(super) fn parts(self) -> RequestParts {
        match self {
            Self::NewState {
                previous,
                run_system_triggers,
                run_boot_sync,
            } => {
                let (previous_id, previous_origin) = match previous {
                    NewStatePrevious::Active(state) => (Some(state), PreviousOrigin::ActiveState),
                    NewStatePrevious::SynthesizedEmpty => (None, PreviousOrigin::SynthesizedEmpty),
                    NewStatePrevious::Unmanaged => (None, PreviousOrigin::Unmanaged),
                };
                RequestParts {
                    operation: Operation::NewState,
                    candidate_id: None,
                    previous_id,
                    previous_origin,
                    run_system_triggers,
                    run_boot_sync,
                }
            }
            Self::ActivateArchived {
                candidate,
                previous,
                run_system_triggers,
                run_boot_sync,
            } => RequestParts {
                operation: Operation::ActivateArchived,
                candidate_id: Some(candidate),
                previous_id: Some(previous),
                previous_origin: PreviousOrigin::ActiveState,
                run_system_triggers,
                run_boot_sync,
            },
            Self::ActiveReblit {
                state,
                run_system_triggers,
                run_boot_sync,
            } => RequestParts {
                operation: Operation::ActiveReblit,
                candidate_id: Some(state),
                previous_id: Some(state),
                previous_origin: PreviousOrigin::ActiveReblitCorrupt,
                run_system_triggers,
                run_boot_sync,
            },
        }
    }
}
