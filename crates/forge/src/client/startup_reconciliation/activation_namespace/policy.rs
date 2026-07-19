use crate::transition_journal::{
    AbortDisposition, ForwardPhase, Operation, Phase, PreviousOrigin, RecoveryDisposition, RollbackAction,
    RollbackPlan, TransitionRecord,
};

use super::capture::{NamespaceSnapshot, StateIdObservation, TreeLocation, UsrFingerprint};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum NamespacePolicyConflict {
    #[error("candidate token occurs at {actual} activation-tree locations")]
    CandidateCount { actual: usize },
    #[error("previous token occurs at {actual} activation-tree locations")]
    PreviousCount { actual: usize },
    #[error("candidate/previous trees do not form one phase-authorized layout")]
    PhaseLayout {
        candidate: TreeLocation,
        previous: Option<TreeLocation>,
    },
    #[error("rollback action outcomes contradict one another")]
    RollbackActions,
    #[error("fixed staging wrapper does not match the phase-authorized tree")]
    StagingWrapper,
    #[error("exact transition quarantine wrapper does not match the authorized tree")]
    TransitionQuarantine,
    #[error("active-reblit reserved wrapper evidence is not exact")]
    ActiveReblitWrapper,
    #[error("active-reblit previous-slot evidence is not exact for the journal phase")]
    ActiveReblitPreviousSlot,
    #[error("candidate state-ID evidence is incompatible: expected {expected:?}, found {actual:?}")]
    CandidateStateId {
        expected: StateIdExpectation,
        actual: StateIdObservation,
    },
    #[error("previous state-ID evidence is incompatible: expected {expected:?}, found {actual:?}")]
    PreviousStateId {
        expected: PreviousStateIdExpectation,
        actual: StateIdObservation,
    },
    #[error("candidate runtime identity conflicts with the current journal epoch")]
    CandidateRuntime,
    #[error("previous runtime identity conflicts with the current journal epoch")]
    PreviousRuntime,
    #[error("merged-/usr root ABI links are incomplete after their durable completion phase")]
    RootAbiIncomplete,
    #[error("merged-/usr isolation ABI links are incomplete after their durable completion phase")]
    IsolationAbiIncomplete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation) enum StateIdExpectation {
    Absent,
    Optional(i32),
    Present(i32),
    MarkerOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation) enum PreviousStateIdExpectation {
    Exact(Option<i32>),
    ActiveReblitCorrupt(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidatePlace {
    Live,
    Staging,
    Destination,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PreviousPlace {
    Live,
    Staging,
    Archived,
    TransitionQuarantine,
    ActiveReblitWrapper,
    Absent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveReblitReservation {
    Absent,
    Optional,
    Required,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LayoutAlternative {
    pub(super) candidate: CandidatePlace,
    pub(super) previous: PreviousPlace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation) enum UsrExchangeLayout {
    Pre,
    Post,
}

impl LayoutAlternative {
    pub(super) fn usr_exchange_layout(self) -> Option<UsrExchangeLayout> {
        if self == PRE_EXCHANGE {
            Some(UsrExchangeLayout::Pre)
        } else if self == POST_EXCHANGE {
            Some(UsrExchangeLayout::Post)
        } else {
            None
        }
    }
}

const PRE_EXCHANGE: LayoutAlternative = LayoutAlternative {
    candidate: CandidatePlace::Staging,
    previous: PreviousPlace::Live,
};
const POST_EXCHANGE: LayoutAlternative = LayoutAlternative {
    candidate: CandidatePlace::Live,
    previous: PreviousPlace::Staging,
};
const PREVIOUS_ARCHIVED: LayoutAlternative = LayoutAlternative {
    candidate: CandidatePlace::Live,
    previous: PreviousPlace::Archived,
};

#[cfg(test)]
pub(super) fn assess_snapshot(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<(), NamespacePolicyConflict> {
    assess_snapshot_layout(record, snapshot).map(|_| ())
}

pub(super) fn assess_snapshot_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<LayoutAlternative, NamespacePolicyConflict> {
    let candidate = trees_for_token(snapshot, record.candidate.tree_token.as_str());
    if candidate.len() != 1 {
        return Err(NamespacePolicyConflict::CandidateCount {
            actual: candidate.len(),
        });
    }
    let previous = trees_for_token(snapshot, record.previous.tree_token.as_str());
    let alternatives = expected_layouts(record)?;
    let previous_may_be_absent = alternatives
        .iter()
        .any(|layout| layout.previous == PreviousPlace::Absent);
    if previous.len() > 1 || (previous.is_empty() && !previous_may_be_absent) {
        return Err(NamespacePolicyConflict::PreviousCount { actual: previous.len() });
    }

    let candidate = candidate[0];
    let previous = previous.first().copied();
    let selected = alternatives.iter().copied().find(|layout| {
        candidate_place_matches(record, layout.candidate, &candidate.location)
            && previous_place_matches(record, layout.previous, previous.map(|tree| &tree.location))
    });
    let Some(selected) = selected else {
        return Err(NamespacePolicyConflict::PhaseLayout {
            candidate: candidate.location.clone(),
            previous: previous.map(|tree| tree.location.clone()),
        });
    };
    require_fixed_wrapper_layout(record, snapshot, selected, candidate, previous)?;

    let expected_candidate_state = candidate_state_id_expectation(record);
    let actual_candidate_state = candidate.state_id_observation();
    if !state_id_matches(expected_candidate_state, actual_candidate_state) {
        return Err(NamespacePolicyConflict::CandidateStateId {
            expected: expected_candidate_state,
            actual: actual_candidate_state,
        });
    }
    if let Some(previous) = previous {
        let expected_previous_state = previous_state_id_expectation(record);
        let actual_previous_state = previous.state_id_observation();
        if !previous_state_id_matches(expected_previous_state, actual_previous_state) {
            return Err(NamespacePolicyConflict::PreviousStateId {
                expected: expected_previous_state,
                actual: actual_previous_state,
            });
        }
    }

    // Runtime witnesses are authoritative only in the exact creation epoch.
    // Across reboot or mount-namespace change, durable tokens and state IDs
    // remain authoritative and historical runtime numbers are ignored.
    if snapshot.epoch() == &record.creation_epoch {
        if candidate.runtime != record.candidate.usr_runtime_identity {
            return Err(NamespacePolicyConflict::CandidateRuntime);
        }
        if let Some(previous) = previous
            && previous.runtime != record.previous.usr_runtime_identity
        {
            return Err(NamespacePolicyConflict::PreviousRuntime);
        }
    }
    if root_abi_must_be_complete(record) && !snapshot.root_abi().is_complete() {
        return Err(NamespacePolicyConflict::RootAbiIncomplete);
    }
    if isolation_abi_must_be_complete(record) && !snapshot.isolation_abi().is_complete() {
        return Err(NamespacePolicyConflict::IsolationAbiIncomplete);
    }
    Ok(selected)
}

fn trees_for_token<'a>(snapshot: &'a NamespaceSnapshot, token: &str) -> Vec<&'a UsrFingerprint> {
    snapshot.trees().filter(|tree| tree.token == token).collect()
}

fn expected_layouts(record: &TransitionRecord) -> Result<Vec<LayoutAlternative>, NamespacePolicyConflict> {
    if let Some(rollback) = &record.rollback {
        return rollback_layouts(record, rollback);
    }
    Ok(forward_layouts(record))
}

pub(super) fn forward_layouts(record: &TransitionRecord) -> Vec<LayoutAlternative> {
    match record.phase {
        Phase::Preparing
        | Phase::FreshStateAllocating
        | Phase::FreshStateAllocated
        | Phase::CandidatePrepareStarted
        | Phase::CandidatePrepared
        | Phase::TransactionTriggersStarted
        | Phase::TransactionTriggersComplete => vec![PRE_EXCHANGE],
        Phase::UsrExchangeIntent => vec![PRE_EXCHANGE, POST_EXCHANGE],
        Phase::UsrExchanged
        | Phase::RootLinksComplete
        | Phase::SystemTriggersStarted
        | Phase::SystemTriggersComplete => vec![POST_EXCHANGE],
        Phase::PreviousArchiveIntent => vec![POST_EXCHANGE, PREVIOUS_ARCHIVED],
        Phase::PreviousArchived => vec![PREVIOUS_ARCHIVED],
        Phase::BootSyncStarted | Phase::BootSyncComplete => {
            if record.options.archive_previous {
                vec![PREVIOUS_ARCHIVED]
            } else {
                vec![POST_EXCHANGE]
            }
        }
        Phase::CommitDecided => commit_layouts(record, true),
        Phase::CommitCleanupComplete | Phase::Complete => commit_layouts(record, false),
        _ => Vec::new(),
    }
}

fn commit_layouts(record: &TransitionRecord, intent: bool) -> Vec<LayoutAlternative> {
    let completed = LayoutAlternative {
        candidate: CandidatePlace::Live,
        previous: match record.previous.origin {
            PreviousOrigin::ActiveState => PreviousPlace::Archived,
            PreviousOrigin::ActiveReblitCorrupt => PreviousPlace::ActiveReblitWrapper,
            PreviousOrigin::SynthesizedEmpty => PreviousPlace::Absent,
            PreviousOrigin::Unmanaged => PreviousPlace::TransitionQuarantine,
        },
    };
    if intent && record.previous.origin != PreviousOrigin::ActiveState && completed != POST_EXCHANGE {
        vec![POST_EXCHANGE, completed]
    } else {
        vec![completed]
    }
}

pub(super) fn rollback_layouts(
    record: &TransitionRecord,
    rollback: &RollbackPlan,
) -> Result<Vec<LayoutAlternative>, NamespacePolicyConflict> {
    let preserved = LayoutAlternative {
        candidate: CandidatePlace::Destination,
        previous: PreviousPlace::Live,
    };
    Ok(match record.phase {
        Phase::RollbackDecided => vec![layout_at_rollback_decision(rollback)?],
        Phase::PreviousRestoreIntent => vec![PREVIOUS_ARCHIVED, POST_EXCHANGE],
        Phase::PreviousRestoredToStaging => vec![POST_EXCHANGE],
        Phase::ReverseExchangeIntent => vec![POST_EXCHANGE, PRE_EXCHANGE],
        Phase::UsrRestored => vec![PRE_EXCHANGE],
        Phase::CandidatePreserveIntent => vec![PRE_EXCHANGE, preserved],
        Phase::CandidatePreserved
        | Phase::FreshDbInvalidationIntent
        | Phase::FreshDbInvalidated
        | Phase::BootRepairRequired
        | Phase::BootRepairStarted
        | Phase::BootRepairComplete
        | Phase::BootRepairUnverified
        | Phase::RollbackComplete => vec![preserved],
        _ => Vec::new(),
    })
}

fn layout_at_rollback_decision(rollback: &RollbackPlan) -> Result<LayoutAlternative, NamespacePolicyConflict> {
    let previous_pending = rollback.previous_archive == RollbackAction::Pending;
    let usr_pending = rollback.usr_exchange == RollbackAction::Pending;
    let candidate_pending = rollback.candidate.action == RollbackAction::Pending;
    if (previous_pending && !usr_pending) || (usr_pending && !candidate_pending) {
        return Err(NamespacePolicyConflict::RollbackActions);
    }
    if previous_pending {
        Ok(PREVIOUS_ARCHIVED)
    } else if usr_pending {
        Ok(POST_EXCHANGE)
    } else if candidate_pending {
        Ok(PRE_EXCHANGE)
    } else {
        Ok(LayoutAlternative {
            candidate: CandidatePlace::Destination,
            previous: PreviousPlace::Live,
        })
    }
}

fn candidate_place_matches(record: &TransitionRecord, expected: CandidatePlace, actual: &TreeLocation) -> bool {
    match expected {
        CandidatePlace::Live => matches!(actual, TreeLocation::Live),
        CandidatePlace::Staging => matches!(actual, TreeLocation::Staging),
        CandidatePlace::Destination => candidate_destination(record, actual),
    }
}

fn previous_place_matches(record: &TransitionRecord, expected: PreviousPlace, actual: Option<&TreeLocation>) -> bool {
    match (expected, actual) {
        (PreviousPlace::Absent, None) => true,
        (PreviousPlace::Live, Some(TreeLocation::Live)) => true,
        (PreviousPlace::Staging, Some(TreeLocation::Staging)) => true,
        (PreviousPlace::Archived, Some(TreeLocation::State(actual))) => record.previous.id == Some(*actual),
        (PreviousPlace::TransitionQuarantine, Some(TreeLocation::TransitionQuarantine)) => true,
        (PreviousPlace::ActiveReblitWrapper, Some(TreeLocation::ActiveReblitWrapper { state, .. })) => {
            record.previous.id == Some(*state)
        }
        _ => false,
    }
}

pub(super) fn candidate_destination(record: &TransitionRecord, actual: &TreeLocation) -> bool {
    let disposition = record.rollback.as_ref().map(|rollback| rollback.candidate.disposition);
    match record.operation {
        Operation::ActivateArchived if disposition == Some(AbortDisposition::Quarantine) => {
            matches!(actual, TreeLocation::TransitionQuarantine)
        }
        Operation::ActivateArchived => record
            .candidate
            .id
            .is_some_and(|candidate| matches!(actual, TreeLocation::State(state) if *state == candidate)),
        Operation::NewState => matches!(actual, TreeLocation::TransitionQuarantine),
        Operation::ActiveReblit => matches!(actual, TreeLocation::TransitionQuarantine)
            || record.previous.id.is_some_and(
                |state| matches!(actual, TreeLocation::ActiveReblitWrapper { state: actual, .. } if *actual == state),
            ),
    }
}

fn require_fixed_wrapper_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    layout: LayoutAlternative,
    candidate: &UsrFingerprint,
    previous: Option<&UsrFingerprint>,
) -> Result<(), NamespacePolicyConflict> {
    let staging = snapshot
        .wrappers()
        .find(|wrapper| matches!(wrapper.role, TreeLocation::Staging))
        .expect("capture requires the fixed staging wrapper");
    let expected_staging = expected_token_at(layout, CandidatePlace::Staging, PreviousPlace::Staging, record);
    if wrapper_tree_token(staging) != expected_staging {
        return Err(NamespacePolicyConflict::StagingWrapper);
    }

    let transition_wrappers = snapshot
        .wrappers()
        .filter(|wrapper| matches!(wrapper.role, TreeLocation::TransitionQuarantine))
        .collect::<Vec<_>>();
    let expected_transition = if layout.candidate == CandidatePlace::Destination
        && matches!(candidate.location, TreeLocation::TransitionQuarantine)
    {
        Some(record.candidate.tree_token.as_str())
    } else if layout.previous == PreviousPlace::TransitionQuarantine {
        Some(record.previous.tree_token.as_str())
    } else {
        None
    };
    if transition_wrappers.len() > 1
        || transition_wrappers
            .first()
            .map(|wrapper| wrapper_tree_token(wrapper))
            .unwrap_or(None)
            != expected_transition
        || (expected_transition.is_some() && transition_wrappers.is_empty())
    {
        return Err(NamespacePolicyConflict::TransitionQuarantine);
    }

    if record.operation == Operation::ActiveReblit {
        let wrappers = snapshot
            .wrappers()
            .filter(|wrapper| matches!(wrapper.role, TreeLocation::ActiveReblitWrapper { .. }))
            .collect::<Vec<_>>();
        let expected_state = record.previous.id;
        if wrappers.iter().any(|wrapper| {
            !matches!(
                wrapper.role,
                TreeLocation::ActiveReblitWrapper { state, .. } if Some(state) == expected_state
            )
        }) {
            return Err(NamespacePolicyConflict::ActiveReblitWrapper);
        }
        let candidate_in_reserved = layout.candidate == CandidatePlace::Destination
            && matches!(candidate.location, TreeLocation::ActiveReblitWrapper { .. });
        let candidate_in_transition = layout.candidate == CandidatePlace::Destination
            && matches!(candidate.location, TreeLocation::TransitionQuarantine);
        let expected = if candidate_in_reserved {
            Some(record.candidate.tree_token.as_str())
        } else if layout.previous == PreviousPlace::ActiveReblitWrapper {
            Some(record.previous.tree_token.as_str())
        } else {
            None
        };
        let exact_contents = wrappers
            .first()
            .map(|wrapper| wrapper_tree_token(wrapper))
            .unwrap_or(None);
        let reservation = active_reblit_reservation(record);
        let shape_matches = if candidate_in_reserved || layout.previous == PreviousPlace::ActiveReblitWrapper {
            reservation != ActiveReblitReservation::Absent && wrappers.len() == 1 && exact_contents == expected
        } else if candidate_in_transition {
            // Generic quarantine is the fallback only when reservation never
            // completed. Once the exact wrapper exists, recovery consumes it.
            reservation != ActiveReblitReservation::Required && wrappers.is_empty()
        } else {
            match reservation {
                ActiveReblitReservation::Absent => wrappers.is_empty(),
                ActiveReblitReservation::Optional => {
                    wrappers.is_empty() || (wrappers.len() == 1 && exact_contents.is_none())
                }
                ActiveReblitReservation::Required => wrappers.len() == 1 && exact_contents.is_none(),
            }
        };
        if !shape_matches {
            return Err(NamespacePolicyConflict::ActiveReblitWrapper);
        }
        require_active_reblit_previous_slot_layout(record, snapshot, !wrappers.is_empty())?;
    }

    // The selected pair has already bound both durable tokens.  Keep these
    // borrows explicit so a future layout variant cannot silently skip the
    // fixed-wrapper relationship above.
    let _ = (candidate, previous);
    Ok(())
}

fn require_active_reblit_previous_slot_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    replacement_present: bool,
) -> Result<(), NamespacePolicyConflict> {
    let previous_state = record.previous.id.expect("validated active-reblit previous state ID");
    let previous_token = record.previous.tree_token.as_str();
    let previous_slots = snapshot
        .wrappers()
        .filter(|wrapper| {
            wrapper
                .slot_identity()
                .is_some_and(|(state, token)| state == previous_state && token == previous_token)
        })
        .collect::<Vec<_>>();
    if previous_slots.len() > 1 {
        return Err(NamespacePolicyConflict::ActiveReblitPreviousSlot);
    }

    let slot = previous_slots.first().copied();
    let parked = slot.is_some_and(|wrapper| {
        matches!(
            &wrapper.role,
            TreeLocation::ArchivedCandidateParking { state, token, .. }
                if *state == previous_state && token == previous_token
        )
    });
    let canonical =
        slot.is_some_and(|wrapper| matches!(&wrapper.role, TreeLocation::State(state) if *state == previous_state));
    if slot.is_some() && !canonical && !parked {
        return Err(NamespacePolicyConflict::ActiveReblitPreviousSlot);
    }

    let parking_wrappers = snapshot
        .wrappers()
        .filter(|wrapper| {
            matches!(
                wrapper.role,
                TreeLocation::ArchivedCandidateParking { .. } | TreeLocation::PreviousParking { .. }
            )
        })
        .collect::<Vec<_>>();
    let parking_shape_is_exact = if parked {
        parking_wrappers.len() == 1 && slot == parking_wrappers.first().copied()
    } else {
        parking_wrappers.is_empty()
    };
    if !parking_shape_is_exact {
        return Err(NamespacePolicyConflict::ActiveReblitPreviousSlot);
    }

    match active_reblit_reservation(record) {
        ActiveReblitReservation::Absent if parked => {
            return Err(NamespacePolicyConflict::ActiveReblitPreviousSlot);
        }
        ActiveReblitReservation::Optional if parked && !replacement_present => {
            return Err(NamespacePolicyConflict::ActiveReblitPreviousSlot);
        }
        ActiveReblitReservation::Required if canonical => {
            return Err(NamespacePolicyConflict::ActiveReblitPreviousSlot);
        }
        ActiveReblitReservation::Absent | ActiveReblitReservation::Optional | ActiveReblitReservation::Required => {}
    }
    Ok(())
}

fn expected_token_at<'a>(
    layout: LayoutAlternative,
    candidate_place: CandidatePlace,
    previous_place: PreviousPlace,
    record: &'a TransitionRecord,
) -> Option<&'a str> {
    if layout.candidate == candidate_place {
        Some(record.candidate.tree_token.as_str())
    } else if layout.previous == previous_place {
        Some(record.previous.tree_token.as_str())
    } else {
        None
    }
}

fn wrapper_tree_token(wrapper: &super::capture::WrapperFingerprint) -> Option<&str> {
    wrapper.usr.as_ref().map(|usr| usr.token.as_str())
}

pub(super) fn candidate_state_id_expectation(record: &TransitionRecord) -> StateIdExpectation {
    // Once rollback is durable, movement and preservation are deliberately
    // marker-only. A trigger-damaged, missing, or conflicting `.stateID` is
    // opaque payload to quarantine/rearchive, never valid forward evidence.
    if record.rollback.is_some() || matches!(record.recovery_disposition(), RecoveryDisposition::BeginRollback { .. }) {
        return StateIdExpectation::MarkerOnly;
    }
    let Some(candidate) = record.candidate.id else {
        return StateIdExpectation::Absent;
    };
    if record.operation == Operation::ActivateArchived {
        return StateIdExpectation::Present(candidate);
    }
    let phase = record
        .rollback
        .as_ref()
        .map(|rollback| rollback.source)
        .map(forward_ordinal)
        .unwrap_or_else(|| forward_phase_ordinal(record.phase));
    match phase {
        0..=2 => StateIdExpectation::Absent,
        3 => StateIdExpectation::Optional(candidate),
        _ => StateIdExpectation::Present(candidate),
    }
}

fn previous_state_id_expectation(record: &TransitionRecord) -> PreviousStateIdExpectation {
    if record.operation == Operation::ActiveReblit {
        PreviousStateIdExpectation::ActiveReblitCorrupt(
            record.previous.id.expect("validated active-reblit previous state ID"),
        )
    } else {
        PreviousStateIdExpectation::Exact(record.previous.id)
    }
}

fn state_id_matches(expected: StateIdExpectation, actual: StateIdObservation) -> bool {
    match expected {
        StateIdExpectation::Absent => actual == StateIdObservation::Absent,
        StateIdExpectation::Optional(state) => {
            matches!(actual, StateIdObservation::Absent) || actual == StateIdObservation::Canonical(state)
        }
        StateIdExpectation::Present(state) => actual == StateIdObservation::Canonical(state),
        StateIdExpectation::MarkerOnly => true,
    }
}

fn previous_state_id_matches(expected: PreviousStateIdExpectation, actual: StateIdObservation) -> bool {
    match expected {
        PreviousStateIdExpectation::Exact(state) => {
            actual == state.map_or(StateIdObservation::Absent, StateIdObservation::Canonical)
        }
        PreviousStateIdExpectation::ActiveReblitCorrupt(state) => {
            matches!(actual, StateIdObservation::Absent | StateIdObservation::Corrupt)
                || actual == StateIdObservation::Canonical(state)
        }
    }
}

pub(super) fn root_abi_must_be_complete(record: &TransitionRecord) -> bool {
    if let Some(rollback) = &record.rollback {
        let source = forward_ordinal(rollback.source);
        return source >= 9 || (source >= 8 && record.phase == Phase::RollbackComplete);
    }
    forward_phase_ordinal(record.phase) >= 9
}

pub(super) fn isolation_abi_must_be_complete(record: &TransitionRecord) -> bool {
    let phase = record
        .rollback
        .as_ref()
        .map(|rollback| forward_ordinal(rollback.source))
        .unwrap_or_else(|| forward_phase_ordinal(record.phase));
    (matches!(record.operation, Operation::NewState | Operation::ActiveReblit) && phase >= 5)
        || (record.options.run_system_triggers && phase >= 10)
}

fn active_reblit_reservation(record: &TransitionRecord) -> ActiveReblitReservation {
    let phase = record
        .rollback
        .as_ref()
        .map(|rollback| forward_ordinal(rollback.source))
        .unwrap_or_else(|| forward_phase_ordinal(record.phase));
    match phase {
        0..=3 => ActiveReblitReservation::Absent,
        4 => ActiveReblitReservation::Optional,
        _ => ActiveReblitReservation::Required,
    }
}

fn forward_ordinal(phase: ForwardPhase) -> u8 {
    match phase {
        ForwardPhase::Preparing => 0,
        ForwardPhase::FreshStateAllocating => 1,
        ForwardPhase::FreshStateAllocated => 2,
        ForwardPhase::CandidatePrepareStarted => 3,
        ForwardPhase::CandidatePrepared => 4,
        ForwardPhase::TransactionTriggersStarted => 5,
        ForwardPhase::TransactionTriggersComplete => 6,
        ForwardPhase::UsrExchangeIntent => 7,
        ForwardPhase::UsrExchanged => 8,
        ForwardPhase::RootLinksComplete => 9,
        ForwardPhase::SystemTriggersStarted => 10,
        ForwardPhase::SystemTriggersComplete => 11,
        ForwardPhase::PreviousArchiveIntent => 12,
        ForwardPhase::PreviousArchived => 13,
        ForwardPhase::BootSyncStarted => 14,
        ForwardPhase::BootSyncComplete => 15,
        ForwardPhase::CommitDecided => 16,
        ForwardPhase::CommitCleanupComplete => 17,
        ForwardPhase::Complete => 18,
    }
}

fn forward_phase_ordinal(phase: Phase) -> u8 {
    match phase {
        Phase::Preparing => 0,
        Phase::FreshStateAllocating => 1,
        Phase::FreshStateAllocated => 2,
        Phase::CandidatePrepareStarted => 3,
        Phase::CandidatePrepared => 4,
        Phase::TransactionTriggersStarted => 5,
        Phase::TransactionTriggersComplete => 6,
        Phase::UsrExchangeIntent => 7,
        Phase::UsrExchanged => 8,
        Phase::RootLinksComplete => 9,
        Phase::SystemTriggersStarted => 10,
        Phase::SystemTriggersComplete => 11,
        Phase::PreviousArchiveIntent => 12,
        Phase::PreviousArchived => 13,
        Phase::BootSyncStarted => 14,
        Phase::BootSyncComplete => 15,
        Phase::CommitDecided => 16,
        Phase::CommitCleanupComplete => 17,
        Phase::Complete => 18,
        _ => 18,
    }
}
