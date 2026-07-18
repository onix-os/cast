//! Independent retained namespace proof for candidate preservation.
//!
//! Candidate preservation has three operation-specific topologies.  This
//! read-only proof admits only their exact staged, crash-prefix, or preserved
//! shapes, retains both sides of the admission sandwich, and requires a fresh
//! matching capture whenever an authority is revalidated. Exact NewState
//! target prefixes can be consumed only by their separate one-attempt proof
//! children. Exact move evidence must also cross fresh target and
//! quarantine-parent durability before it can reach rename.

mod active_reblit_effect;
mod effect_reconciliation;
mod target_creation;
mod target_normalization;

use crate::{
    Installation,
    transition_journal::{Operation, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    capture::{
        CaptureError, NamespaceSnapshot, NewStateCandidatePreserveCaptureError,
        NewStateCandidatePreservePostMoveDurabilityError, NewStateCandidatePreserveTargetDurabilityError,
        ProjectedNewStateCandidatePreserveNamespace, RetainedNewStateCandidatePreserveParents, TreeLocation,
        UsrRollbackNewStateTargetCreateNamespaceEvidence, UsrRollbackNewStateTargetNormalizeNamespaceEvidence,
        WrapperFingerprint, capture_snapshot,
    },
    policy::{NamespacePolicyConflict, assess_snapshot_layout},
};

pub(in crate::client::startup_reconciliation) use active_reblit_effect::{
    UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedNamespace,
    UsrRollbackActiveReblitCandidatePreserveAppliedNamespace, UsrRollbackActiveReblitCandidatePreserveDurableNamespace,
    UsrRollbackActiveReblitCandidatePreserveNamespaceApplyReconciliation,
    UsrRollbackActiveReblitCandidatePreserveNamespaceEffectEvidence,
};

#[cfg(test)]
pub(in crate::client) use super::capture::{
    NewStateCandidatePreservePostMoveDurabilityEvent, NewStateCandidatePreservePostMoveDurabilityFaultPoint,
    NewStateCandidatePreserveTargetDurabilityEvent, NewStateCandidatePreserveTargetDurabilityFaultPoint,
    NewStateTargetNormalizeDurabilityEvent, NewStateTargetNormalizeDurabilityFaultPoint,
    arm_before_new_state_candidate_preserve_durable_post_revalidation_capture,
    arm_before_new_state_candidate_preserve_post_move_candidate_sync,
    arm_before_new_state_candidate_preserve_post_move_final_post_capture,
    arm_before_new_state_candidate_preserve_post_move_quarantine_parent_sync,
    arm_before_new_state_candidate_preserve_post_move_staging_parent_sync,
    arm_before_new_state_candidate_preserve_post_move_target_parent_sync,
    arm_before_new_state_candidate_preserve_quarantine_parent_sync,
    arm_before_new_state_candidate_preserve_target_durability_final_pre_capture,
    arm_before_new_state_candidate_preserve_target_durability_pre_move_revalidation,
    arm_before_new_state_candidate_preserve_target_sync, arm_before_new_state_target_normalize_final_canonical_capture,
    arm_before_new_state_target_normalize_quarantine_parent_sync, arm_before_new_state_target_normalize_target_sync,
    arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture,
    arm_new_state_candidate_preserve_post_move_durability_fault,
    arm_new_state_candidate_preserve_target_durability_fault, arm_new_state_target_normalize_durability_fault,
    reset_new_state_candidate_preserve_post_move_durability_events,
    reset_new_state_candidate_preserve_target_durability_events, reset_new_state_target_normalize_durability_events,
    take_new_state_candidate_preserve_post_move_durability_events,
    take_new_state_candidate_preserve_target_durability_events, take_new_state_target_normalize_durability_events,
};
#[cfg(test)]
pub(in crate::client) use effect_reconciliation::arm_before_new_state_candidate_preserve_candidate_sync;
pub(in crate::client::startup_reconciliation) use effect_reconciliation::{
    UsrRollbackNewStateCandidatePreserveAlreadySatisfiedNamespace,
    UsrRollbackNewStateCandidatePreserveAppliedNamespace, UsrRollbackNewStateCandidatePreserveDurableNamespace,
    UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation,
};
pub(in crate::client::startup_reconciliation) use target_creation::UsrRollbackNewStateTargetCreateNamespaceReconciliation;
#[cfg(test)]
pub(in crate::client) use target_creation::arm_before_usr_rollback_new_state_target_create_final_pre_capture;
pub(in crate::client::startup_reconciliation) use target_normalization::UsrRollbackNewStateTargetNormalizeNamespaceReconciliation;
#[cfg(test)]
pub(in crate::client) use target_normalization::arm_before_usr_rollback_new_state_target_normalize_final_pre_capture;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackCandidatePreserveTopology {
    NewStateStaged,
    NewStateStagedWithTargetResidue,
    NewStateStagedWithEmptyQuarantine,
    NewStatePreserved,
    ArchivedStagedWithCanonicalSlot,
    ArchivedPreserved,
    ActiveReblitStaged { wrapper_index: usize },
    ActiveReblitPreserved { wrapper_index: usize },
}

impl UsrRollbackCandidatePreserveTopology {
    pub(in crate::client::startup_reconciliation) fn is_preserved(self) -> bool {
        matches!(
            self,
            Self::NewStatePreserved | Self::ArchivedPreserved | Self::ActiveReblitPreserved { .. }
        )
    }
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackCandidatePreserveNamespaceInspection {
    before: NamespaceSnapshot,
    topology: UsrRollbackCandidatePreserveTopology,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackCandidatePreserveNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    topology: UsrRollbackCandidatePreserveTopology,
}

/// Opaque exact PRE1 namespace transferred into the consuming move lease.
///
/// The retained descriptors and normalized projection deliberately have no
/// accessor. The private effect child can only consume them through the
/// pre-move candidate, target, and quarantine-parent barriers, final PRE
/// recapture, and one-shot move.
pub(in crate::client::startup_reconciliation) struct UsrRollbackNewStateCandidatePreserveNamespaceEffectEvidence {
    baseline: NamespaceSnapshot,
    projection: ProjectedNewStateCandidatePreserveNamespace,
    parents: RetainedNewStateCandidatePreserveParents,
}

impl UsrRollbackCandidatePreserveNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackCandidatePreserveNamespaceError> {
        require_exact_journal(journal, expected)?;
        let before = capture_snapshot(installation, expected)?;
        let topology = candidate_preserve_topology(expected, &before)?;
        Ok(Self { before, topology })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<UsrRollbackCandidatePreserveNamespaceProof, UsrRollbackCandidatePreserveNamespaceError> {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_topology(expected, &self.before, self.topology)?;
        require_topology(expected, &after, self.topology)?;
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackCandidatePreserveNamespaceProof {
            before: self.before,
            after,
            topology: self.topology,
        })
    }
}

impl UsrRollbackCandidatePreserveNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn topology(&self) -> UsrRollbackCandidatePreserveTopology {
        self.topology
    }

    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_topology(expected, &self.before, self.topology)?;
        require_topology(expected, &self.after, self.topology)?;
        require_exact_journal(journal, expected)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_topology(expected, &fresh, self.topology)?;

        require_exact_journal(journal, expected)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    /// Consume only exact NewState staged evidence whose target is absent.
    pub(in crate::client::startup_reconciliation) fn into_new_state_target_create_evidence(
        self,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackNewStateTargetCreateNamespaceEvidence, UsrRollbackCandidatePreserveNamespaceError> {
        if self.topology != UsrRollbackCandidatePreserveTopology::NewStateStaged {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }
        Ok(UsrRollbackNewStateTargetCreateNamespaceEvidence::capture(
            self.after, record,
        )?)
    }

    /// Consume only exact NewState staged evidence with one owned restrictive
    /// target residue. It cannot be mistaken for either creation or movement.
    pub(in crate::client::startup_reconciliation) fn into_new_state_target_normalize_evidence(
        self,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackNewStateTargetNormalizeNamespaceEvidence, UsrRollbackCandidatePreserveNamespaceError> {
        if self.topology != UsrRollbackCandidatePreserveTopology::NewStateStagedWithTargetResidue {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }
        Ok(UsrRollbackNewStateTargetNormalizeNamespaceEvidence::capture(
            self.after, record,
        )?)
    }

    /// Consume only the exact NewState staged-with-empty-quarantine prefix.
    /// Every other candidate-preservation topology remains unsupported by this
    /// move checkpoint and yields no move-effect evidence.
    pub(in crate::client::startup_reconciliation) fn into_new_state_move_effect_evidence(
        self,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackNewStateCandidatePreserveNamespaceEffectEvidence, UsrRollbackCandidatePreserveNamespaceError>
    {
        if self.topology != UsrRollbackCandidatePreserveTopology::NewStateStagedWithEmptyQuarantine {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }
        let projection = ProjectedNewStateCandidatePreserveNamespace::capture(&self.after, record)?;
        let parents = RetainedNewStateCandidatePreserveParents::capture(&self.after, record)?;
        Ok(UsrRollbackNewStateCandidatePreserveNamespaceEffectEvidence {
            baseline: self.after,
            projection,
            parents,
        })
    }
}

fn candidate_preserve_topology(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<UsrRollbackCandidatePreserveTopology, UsrRollbackCandidatePreserveNamespaceError> {
    if record.phase != Phase::CandidatePreserveIntent {
        return Err(UsrRollbackCandidatePreserveNamespaceError::WrongPhase);
    }
    candidate_preserve_topology_after_phase(record, snapshot)
}

/// Require the one exact NewState namespace which may route durable candidate
/// preservation into fresh-database invalidation.
///
/// This helper is deliberately separate from `candidate_preserve_topology`:
/// the existing candidate-preservation checkpoint remains restricted to
/// `CandidatePreserveIntent`, while this read-only route accepts only its
/// already-persisted `CandidatePreserved` successor.
pub(in crate::client::startup_reconciliation::activation_namespace) fn require_exact_new_state_candidate_preserved_topology(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
    if record.phase != Phase::CandidatePreserved {
        return Err(UsrRollbackCandidatePreserveNamespaceError::WrongCandidatePreservedPhase);
    }
    if record.operation != Operation::NewState {
        return Err(UsrRollbackCandidatePreserveNamespaceError::NewStateRequired);
    }
    if candidate_preserve_topology_after_phase(record, snapshot)?
        == UsrRollbackCandidatePreserveTopology::NewStatePreserved
    {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch)
    }
}

/// Require the exact whole-wrapper ActiveReblit preservation topology while
/// the journal is at its already-persisted `CandidatePreserved` checkpoint.
///
/// This helper is deliberately phase and operation specific. The completion
/// route must not widen candidate-preservation admission or borrow the
/// NewState fresh-database route. The exact derived wrapper index is returned
/// so callers retain it rather than substituting a default index.
pub(in crate::client::startup_reconciliation::activation_namespace) fn require_exact_active_reblit_candidate_preserved_topology(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<usize, UsrRollbackCandidatePreserveNamespaceError> {
    if record.phase != Phase::CandidatePreserved {
        return Err(UsrRollbackCandidatePreserveNamespaceError::WrongActiveReblitCompleteRoutePhase);
    }
    if record.operation != Operation::ActiveReblit {
        return Err(UsrRollbackCandidatePreserveNamespaceError::ActiveReblitRequired);
    }
    let UsrRollbackCandidatePreserveTopology::ActiveReblitPreserved { wrapper_index } =
        candidate_preserve_topology_after_phase(record, snapshot)?
    else {
        return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
    };
    Ok(wrapper_index)
}

/// Require the exact whole-wrapper ActiveReblit preservation topology at
/// terminal rollback.
///
/// This helper is deliberately phase and operation specific. Terminal
/// finalization must recapture `RollbackComplete` evidence rather than widen
/// or reuse the authority which routed `CandidatePreserved` into that
/// successor. The exact derived wrapper index is retained by the caller;
/// index zero is valid evidence and is never treated as an absent sentinel.
pub(in crate::client::startup_reconciliation::activation_namespace) fn require_exact_active_reblit_rollback_complete_topology(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<usize, UsrRollbackCandidatePreserveNamespaceError> {
    if record.phase != Phase::RollbackComplete {
        return Err(UsrRollbackCandidatePreserveNamespaceError::WrongActiveReblitFinalizationPhase);
    }
    if record.operation != Operation::ActiveReblit {
        return Err(UsrRollbackCandidatePreserveNamespaceError::ActiveReblitRequired);
    }
    let UsrRollbackCandidatePreserveTopology::ActiveReblitPreserved { wrapper_index } =
        candidate_preserve_topology_after_phase(record, snapshot)?
    else {
        return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
    };
    Ok(wrapper_index)
}

/// Require the exact preserved-candidate namespace while the journal is at
/// `FreshDbInvalidationIntent`.
///
/// This is deliberately separate from the earlier routing proof: accepting a
/// persisted invalidation intent must never make `CandidatePreserved`
/// admission applicable again.
pub(in crate::client::startup_reconciliation::activation_namespace) fn require_exact_new_state_fresh_db_invalidation_topology(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
    if record.phase != Phase::FreshDbInvalidationIntent {
        return Err(UsrRollbackCandidatePreserveNamespaceError::WrongFreshDbInvalidationPhase);
    }
    if record.operation != Operation::NewState {
        return Err(UsrRollbackCandidatePreserveNamespaceError::NewStateRequired);
    }
    if candidate_preserve_topology_after_phase(record, snapshot)?
        == UsrRollbackCandidatePreserveTopology::NewStatePreserved
    {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch)
    }
}

/// Require the exact preserved-candidate namespace after fresh-database
/// invalidation has been persisted.
///
/// This phase-specific helper deliberately does not widen the earlier
/// `FreshDbInvalidationIntent` proof. A completion-route capability can only
/// be captured from the already-persisted `FreshDbInvalidated` record.
pub(in crate::client::startup_reconciliation::activation_namespace) fn require_exact_new_state_fresh_db_invalidated_topology(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
    if record.phase != Phase::FreshDbInvalidated {
        return Err(UsrRollbackCandidatePreserveNamespaceError::WrongFreshDbInvalidatedPhase);
    }
    if record.operation != Operation::NewState {
        return Err(UsrRollbackCandidatePreserveNamespaceError::NewStateRequired);
    }
    if candidate_preserve_topology_after_phase(record, snapshot)?
        == UsrRollbackCandidatePreserveTopology::NewStatePreserved
    {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch)
    }
}

/// Require the exact preserved-candidate namespace at terminal rollback.
///
/// This helper is deliberately phase-specific. Finalization must recapture
/// terminal `RollbackComplete` evidence rather than widening or reusing the
/// authority which routed `FreshDbInvalidated` into that successor.
#[allow(dead_code)] // consumed only by the separately sealed finalization checkpoint
pub(in crate::client::startup_reconciliation::activation_namespace) fn require_exact_new_state_rollback_complete_topology(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
    if record.phase != Phase::RollbackComplete {
        return Err(UsrRollbackCandidatePreserveNamespaceError::WrongRollbackCompletePhase);
    }
    if record.operation != Operation::NewState {
        return Err(UsrRollbackCandidatePreserveNamespaceError::NewStateRequired);
    }
    if candidate_preserve_topology_after_phase(record, snapshot)?
        == UsrRollbackCandidatePreserveTopology::NewStatePreserved
    {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch)
    }
}

fn candidate_preserve_topology_after_phase(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<UsrRollbackCandidatePreserveTopology, UsrRollbackCandidatePreserveNamespaceError> {
    assess_snapshot_layout(record, snapshot)?;
    if record.operation != Operation::ActiveReblit
        && snapshot.wrappers().any(|wrapper| {
            matches!(
                wrapper.role,
                TreeLocation::ArchivedCandidateParking { .. } | TreeLocation::PreviousParking { .. }
            )
        })
    {
        return Err(UsrRollbackCandidatePreserveNamespaceError::UnexpectedParkingWrapper);
    }
    require_current_state_wrapper_scope(record, snapshot)?;

    let candidate = snapshot
        .trees()
        .find(|tree| tree.token == record.candidate.tree_token.as_str())
        .ok_or(UsrRollbackCandidatePreserveNamespaceError::CandidateMissing)?;
    let staging = one_wrapper(snapshot, |wrapper| wrapper.role == TreeLocation::Staging)?
        .ok_or(UsrRollbackCandidatePreserveNamespaceError::StagingMissing)?;
    let transition = one_wrapper(snapshot, |wrapper| wrapper.role == TreeLocation::TransitionQuarantine)?;
    let new_state_target_residue = snapshot.has_new_state_target_residue();

    match record.operation {
        Operation::NewState => new_state_topology(candidate, staging, transition, new_state_target_residue),
        Operation::ActivateArchived if !new_state_target_residue => {
            archived_topology(record, snapshot, candidate, staging, transition)
        }
        Operation::ActiveReblit if !new_state_target_residue => {
            active_reblit_topology(record, snapshot, candidate, staging, transition)
        }
        Operation::ActivateArchived | Operation::ActiveReblit => {
            Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch)
        }
    }
}

fn require_current_state_wrapper_scope(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
    for wrapper in snapshot.wrappers() {
        let TreeLocation::State(state) = &wrapper.role else {
            continue;
        };
        let state = *state;
        if record.previous.id == Some(state)
            || (record.candidate.id == Some(state) && record.operation != Operation::ActivateArchived)
        {
            return Err(UsrRollbackCandidatePreserveNamespaceError::UnexpectedCurrentStateWrapper { state });
        }
    }
    Ok(())
}

fn new_state_topology(
    candidate: &super::capture::UsrFingerprint,
    staging: &WrapperFingerprint,
    transition: Option<&WrapperFingerprint>,
    target_residue: bool,
) -> Result<UsrRollbackCandidatePreserveTopology, UsrRollbackCandidatePreserveNamespaceError> {
    if candidate.marker_links() != 1 {
        return Err(UsrRollbackCandidatePreserveNamespaceError::MarkerLinks {
            expected: 1,
            actual: candidate.marker_links(),
        });
    }
    if candidate.location == TreeLocation::Staging
        && wrapper_contains(staging, candidate)
        && staging.slot_identity().is_none()
    {
        return match (transition, target_residue) {
            (None, false) => Ok(UsrRollbackCandidatePreserveTopology::NewStateStaged),
            (None, true) => Ok(UsrRollbackCandidatePreserveTopology::NewStateStagedWithTargetResidue),
            (Some(wrapper), false) if wrapper_is_empty(wrapper) && wrapper.has_exact_private_permissions() => {
                Ok(UsrRollbackCandidatePreserveTopology::NewStateStagedWithEmptyQuarantine)
            }
            (Some(_), false) | (Some(_), true) => Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch),
        };
    }
    if candidate.location == TreeLocation::TransitionQuarantine
        && wrapper_is_empty(staging)
        && !target_residue
        && transition.is_some_and(|wrapper| {
            wrapper_contains(wrapper, candidate)
                && wrapper.slot_identity().is_none()
                && wrapper.has_exact_private_permissions()
        })
    {
        return Ok(UsrRollbackCandidatePreserveTopology::NewStatePreserved);
    }
    Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch)
}

fn archived_topology(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    candidate: &super::capture::UsrFingerprint,
    staging: &WrapperFingerprint,
    transition: Option<&WrapperFingerprint>,
) -> Result<UsrRollbackCandidatePreserveTopology, UsrRollbackCandidatePreserveNamespaceError> {
    if transition.is_some() {
        return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
    }
    if candidate.marker_links() != 2 {
        return Err(UsrRollbackCandidatePreserveNamespaceError::MarkerLinks {
            expected: 2,
            actual: candidate.marker_links(),
        });
    }
    let state = record
        .candidate
        .id
        .ok_or(UsrRollbackCandidatePreserveNamespaceError::CandidateStateMissing)?;
    let canonical = one_wrapper(snapshot, |wrapper| wrapper.role == TreeLocation::State(state))?
        .ok_or(UsrRollbackCandidatePreserveNamespaceError::CandidateWrapperMissing)?;
    let exact_slot = |wrapper: &WrapperFingerprint| {
        wrapper
            .slot_identity()
            .is_some_and(|(actual_state, token)| actual_state == state && token == record.candidate.tree_token.as_str())
    };

    if candidate.location == TreeLocation::Staging && wrapper_contains(staging, candidate) {
        if staging.slot_identity().is_none() && canonical.usr.is_none() && exact_slot(canonical) {
            return Ok(UsrRollbackCandidatePreserveTopology::ArchivedStagedWithCanonicalSlot);
        }
    }
    if candidate.location == TreeLocation::State(state)
        && wrapper_is_empty(staging)
        && wrapper_contains(canonical, candidate)
        && exact_slot(canonical)
    {
        return Ok(UsrRollbackCandidatePreserveTopology::ArchivedPreserved);
    }
    Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch)
}

fn active_reblit_topology(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    candidate: &super::capture::UsrFingerprint,
    staging: &WrapperFingerprint,
    transition: Option<&WrapperFingerprint>,
) -> Result<UsrRollbackCandidatePreserveTopology, UsrRollbackCandidatePreserveNamespaceError> {
    if transition.is_some() {
        return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
    }
    if candidate.marker_links() != 1 {
        return Err(UsrRollbackCandidatePreserveNamespaceError::MarkerLinks {
            expected: 1,
            actual: candidate.marker_links(),
        });
    }
    let state = record
        .previous
        .id
        .ok_or(UsrRollbackCandidatePreserveNamespaceError::PreviousStateMissing)?;
    let replacement = one_wrapper(
        snapshot,
        |wrapper| matches!(wrapper.role, TreeLocation::ActiveReblitWrapper { state: actual, .. } if actual == state),
    )?
    .ok_or(UsrRollbackCandidatePreserveNamespaceError::ActiveReblitWrapperMissing)?;
    let TreeLocation::ActiveReblitWrapper {
        index: wrapper_index, ..
    } = &replacement.role
    else {
        unreachable!("active-reblit wrapper was selected by role")
    };
    let wrapper_index = *wrapper_index;

    // Whole-wrapper preservation moves the reserved empty replacement to the
    // fixed staging name, so require its exact private mode at either prefix.
    if candidate.location == TreeLocation::Staging
        && wrapper_contains(staging, candidate)
        && staging.slot_identity().is_none()
        && wrapper_is_empty(replacement)
        && replacement.has_exact_private_permissions()
    {
        return Ok(UsrRollbackCandidatePreserveTopology::ActiveReblitStaged { wrapper_index });
    }
    if candidate.location
        == (TreeLocation::ActiveReblitWrapper {
            state,
            index: wrapper_index,
        })
        && wrapper_is_empty(staging)
        && staging.has_exact_private_permissions()
        && wrapper_contains(replacement, candidate)
        && replacement.slot_identity().is_none()
    {
        return Ok(UsrRollbackCandidatePreserveTopology::ActiveReblitPreserved { wrapper_index });
    }
    Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch)
}

fn one_wrapper(
    snapshot: &NamespaceSnapshot,
    predicate: impl Fn(&WrapperFingerprint) -> bool,
) -> Result<Option<&WrapperFingerprint>, UsrRollbackCandidatePreserveNamespaceError> {
    let mut matches = snapshot.wrappers().filter(|wrapper| predicate(wrapper));
    let first = matches.next();
    if matches.next().is_some() {
        Err(UsrRollbackCandidatePreserveNamespaceError::DuplicateWrapper)
    } else {
        Ok(first)
    }
}

fn wrapper_contains(wrapper: &WrapperFingerprint, tree: &super::capture::UsrFingerprint) -> bool {
    wrapper.usr.as_ref() == Some(tree)
}

fn wrapper_is_empty(wrapper: &WrapperFingerprint) -> bool {
    wrapper.usr.is_none() && wrapper.slot_identity().is_none()
}

fn require_topology(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: UsrRollbackCandidatePreserveTopology,
) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
    if candidate_preserve_topology(record, snapshot)? == expected {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveNamespaceError::TopologyChanged)
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackCandidatePreserveNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackCandidatePreserveNamespaceError {
    #[error("capture or revalidate the exact candidate-preservation namespace")]
    Capture(#[from] CaptureError),
    #[error("assess the exact candidate-preservation namespace against the journal phase")]
    Policy(#[from] NamespacePolicyConflict),
    #[error("capture or reconcile an exact NewState candidate-preservation namespace effect")]
    NewStateEffect(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("capture or reconcile an exact ActiveReblit whole-wrapper candidate-preservation effect")]
    ActiveReblitEffect(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("read the retained canonical transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical transition journal changed during candidate-preservation proof")]
    JournalChanged,
    #[error("candidate-preservation proof requires CandidatePreserveIntent")]
    WrongPhase,
    #[error("fresh-database invalidation routing requires CandidatePreserved")]
    WrongCandidatePreservedPhase,
    #[error("ActiveReblit rollback-completion routing requires CandidatePreserved")]
    WrongActiveReblitCompleteRoutePhase,
    #[error("ActiveReblit rollback finalization requires RollbackComplete")]
    WrongActiveReblitFinalizationPhase,
    #[error("fresh-database invalidation requires FreshDbInvalidationIntent")]
    WrongFreshDbInvalidationPhase,
    #[error("rollback-completion routing requires FreshDbInvalidated")]
    WrongFreshDbInvalidatedPhase,
    #[error("rollback finalization requires RollbackComplete")]
    #[allow(dead_code)] // consumed only by the separately sealed finalization checkpoint
    WrongRollbackCompletePhase,
    #[error("fresh-database invalidation routing requires a NewState transition")]
    NewStateRequired,
    #[error("whole-wrapper rollback-completion routing requires an ActiveReblit transition")]
    ActiveReblitRequired,
    #[error("the candidate tree is absent from the accepted namespace inventory")]
    CandidateMissing,
    #[error("the candidate state ID required by archived preservation is absent")]
    CandidateStateMissing,
    #[error("the previous state ID required by active-reblit preservation is absent")]
    PreviousStateMissing,
    #[error("the fixed staging wrapper is absent")]
    StagingMissing,
    #[error("the canonical archived candidate wrapper is absent")]
    CandidateWrapperMissing,
    #[error("the exact active-reblit replacement wrapper is absent")]
    ActiveReblitWrapperMissing,
    #[error("multiple wrappers claim one exact candidate-preservation role")]
    DuplicateWrapper,
    #[error("an unmodeled transition parking wrapper exists beside candidate-preservation evidence")]
    UnexpectedParkingWrapper,
    #[error("canonical state wrapper {state} aliases a current transition state outside its exact destination")]
    UnexpectedCurrentStateWrapper { state: i32 },
    #[error("the candidate marker has {actual} links; exact topology requires {expected}")]
    MarkerLinks { expected: u64, actual: u64 },
    #[error("the activation namespace is not an exact candidate-preservation topology")]
    TopologyMismatch,
    #[error("the candidate-preservation activation namespace changed during proof")]
    NamespaceChanged,
    #[error("the candidate-preservation topology changed during proof")]
    TopologyChanged,
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}

impl From<NewStateCandidatePreserveCaptureError> for UsrRollbackCandidatePreserveNamespaceError {
    fn from(source: NewStateCandidatePreserveCaptureError) -> Self {
        Self::NewStateEffect(Box::new(source))
    }
}

impl From<NewStateCandidatePreserveTargetDurabilityError> for UsrRollbackCandidatePreserveNamespaceError {
    fn from(source: NewStateCandidatePreserveTargetDurabilityError) -> Self {
        Self::NewStateEffect(Box::new(source))
    }
}

impl From<NewStateCandidatePreservePostMoveDurabilityError> for UsrRollbackCandidatePreserveNamespaceError {
    fn from(source: NewStateCandidatePreservePostMoveDurabilityError) -> Self {
        Self::NewStateEffect(Box::new(source))
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FRESH_NAMESPACE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_candidate_preserve_fresh_namespace_capture(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FRESH_NAMESPACE_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_fresh_namespace_capture() {
    BEFORE_FRESH_NAMESPACE_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_fresh_namespace_capture() {}
