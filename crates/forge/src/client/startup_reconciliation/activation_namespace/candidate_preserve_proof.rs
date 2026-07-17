//! Independent retained namespace proof for candidate preservation.
//!
//! Candidate preservation has three operation-specific topologies.  This
//! read-only proof admits only their exact staged, crash-prefix, or preserved
//! shapes, retains both sides of the admission sandwich, and requires a fresh
//! matching capture whenever an authority is revalidated.

mod effect_reconciliation;

use crate::{
    Installation,
    transition_journal::{Operation, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    capture::{
        CaptureError, NamespaceSnapshot, NewStateCandidatePreserveCaptureError,
        ProjectedNewStateCandidatePreserveNamespace, RetainedNewStateCandidatePreserveParents, TreeLocation,
        WrapperFingerprint, capture_snapshot,
    },
    policy::{NamespacePolicyConflict, assess_snapshot_layout},
};

pub(in crate::client::startup_reconciliation) use effect_reconciliation::{
    UsrRollbackNewStateCandidatePreserveAppliedNamespace,
    UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation,
};
#[cfg(test)]
pub(in crate::client) use effect_reconciliation::{
    arm_before_new_state_candidate_preserve_candidate_sync,
    arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackCandidatePreserveTopology {
    NewStateStaged,
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
/// pre-move candidate sync, final PRE recapture, and one-shot move.
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

    /// Consume only the exact NewState staged-with-empty-quarantine prefix.
    /// Every other candidate-preservation topology remains unsupported by this
    /// first mutation checkpoint and yields no effect evidence.
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

    match record.operation {
        Operation::NewState => new_state_topology(candidate, staging, transition),
        Operation::ActivateArchived => archived_topology(record, snapshot, candidate, staging, transition),
        Operation::ActiveReblit => active_reblit_topology(record, snapshot, candidate, staging, transition),
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
        return match transition {
            None => Ok(UsrRollbackCandidatePreserveTopology::NewStateStaged),
            Some(wrapper) if wrapper_is_empty(wrapper) => {
                Ok(UsrRollbackCandidatePreserveTopology::NewStateStagedWithEmptyQuarantine)
            }
            Some(_) => Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch),
        };
    }
    if candidate.location == TreeLocation::TransitionQuarantine
        && wrapper_is_empty(staging)
        && transition.is_some_and(|wrapper| wrapper_contains(wrapper, candidate) && wrapper.slot_identity().is_none())
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

    if candidate.location == TreeLocation::Staging
        && wrapper_contains(staging, candidate)
        && staging.slot_identity().is_none()
        && wrapper_is_empty(replacement)
    {
        return Ok(UsrRollbackCandidatePreserveTopology::ActiveReblitStaged { wrapper_index });
    }
    if candidate.location
        == (TreeLocation::ActiveReblitWrapper {
            state,
            index: wrapper_index,
        })
        && wrapper_is_empty(staging)
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
    #[error("capture or reconcile the exact NewState candidate-preservation move namespace")]
    NewStateMove(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("read the retained canonical transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical transition journal changed during candidate-preservation proof")]
    JournalChanged,
    #[error("candidate-preservation proof requires CandidatePreserveIntent")]
    WrongPhase,
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
        Self::NewStateMove(Box::new(source))
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
