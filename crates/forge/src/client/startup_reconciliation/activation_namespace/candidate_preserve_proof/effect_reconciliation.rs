//! Final PRE proof and one-shot reconciliation for NewState preservation.
//!
//! Candidate `syncfs` plus retained-tree fsync runs before the last exact PRE
//! recapture. This is a pre-move safety barrier only: the post-move candidate
//! resync and changed-parent durability suffix are intentionally deferred.

use crate::{Installation, transition_journal::TransitionRecord};

use super::{
    UsrRollbackCandidatePreserveNamespaceError, UsrRollbackCandidatePreserveTopology,
    UsrRollbackNewStateCandidatePreserveNamespaceEffectEvidence, require_matching_fingerprints, require_topology,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    AppliedNewStateCandidatePreserveMoveReconciliation, NamespaceSnapshot, NewStateCandidatePreserveLayout,
    NewStateCandidatePreserveMoveReconciliation, ProjectedNewStateCandidatePreserveNamespace,
    RetainedNewStateCandidatePreserveParents, capture_snapshot,
};

/// Opaque POST namespace authority retained after fresh reconciliation.
///
/// No durability or persistence method exists at this checkpoint.
#[must_use = "an applied candidate-preservation move still requires durability"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackNewStateCandidatePreserveAppliedNamespace {
    _reconciliation: AppliedNewStateCandidatePreserveMoveReconciliation,
}

/// Semantic result of consuming one exact PRE1 move capability.
///
/// Failure variants contain no retained evidence and cannot authorize a retry.
#[must_use = "a consumed NewState candidate-preservation move must be handled"]
pub(in crate::client::startup_reconciliation) enum UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation {
    Applied(UsrRollbackNewStateCandidatePreserveAppliedNamespace),
    NotApplied,
    Ambiguous,
}

/// Opaque namespace authority prepared by candidate sync plus a fresh PRE1
/// capture, but not yet consumed into the one-shot move.
///
/// Keeping this value separate lets the enclosing authority repeat its exact
/// journal, database, plan, and installation checks after namespace
/// preparation and immediately before the rename attempt.
#[must_use = "prepared NewState candidate-preservation namespace authority must be consumed"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackNewStateCandidatePreservePreparedNamespace {
    baseline: NamespaceSnapshot,
    projection: ProjectedNewStateCandidatePreserveNamespace,
    parents: RetainedNewStateCandidatePreserveParents,
}

impl UsrRollbackNewStateCandidatePreserveNamespaceEffectEvidence {
    /// Sync the exact candidate and prove one final fresh PRE1 namespace.
    ///
    /// This deliberately stops before the one-shot move so the enclosing
    /// authority can repeat every non-namespace evidence check.
    pub(in crate::client::startup_reconciliation) fn prepare_move(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackNewStateCandidatePreservePreparedNamespace, UsrRollbackCandidatePreserveNamespaceError> {
        let FinalNewStateCandidatePreservePre {
            baseline,
            projection,
            parents,
        } = self.final_exact_pre(installation, record)?;
        Ok(UsrRollbackNewStateCandidatePreservePreparedNamespace {
            baseline,
            projection,
            parents,
        })
    }

    fn final_exact_pre(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<FinalNewStateCandidatePreservePre, UsrRollbackCandidatePreserveNamespaceError> {
        let Self {
            baseline,
            projection,
            parents,
        } = self;
        if projection.layout() != NewStateCandidatePreserveLayout::StagedWithEmptyQuarantine {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }

        installation.revalidate_mutable_namespace()?;
        baseline.revalidate_retained()?;
        require_projection(record, &baseline, &projection)?;
        require_topology(
            record,
            &baseline,
            UsrRollbackCandidatePreserveTopology::NewStateStagedWithEmptyQuarantine,
        )?;
        parents.revalidate_value_identity(installation)?;

        run_before_candidate_sync();
        parents.sync_retained_candidate_for_move()?;

        run_before_final_pre_capture();
        let fresh = capture_snapshot(installation, record)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&baseline, &fresh)?;
        require_projection(record, &fresh, &projection)?;
        require_topology(
            record,
            &fresh,
            UsrRollbackCandidatePreserveTopology::NewStateStagedWithEmptyQuarantine,
        )?;
        parents.revalidate_value_identity(installation)?;
        installation.revalidate_mutable_namespace()?;

        Ok(FinalNewStateCandidatePreservePre {
            baseline: fresh,
            projection,
            parents,
        })
    }
}

struct FinalNewStateCandidatePreservePre {
    baseline: NamespaceSnapshot,
    projection: ProjectedNewStateCandidatePreserveNamespace,
    parents: RetainedNewStateCandidatePreserveParents,
}

impl UsrRollbackNewStateCandidatePreservePreparedNamespace {
    /// Consume prepared PRE1 authority through at most one no-replace move and
    /// classify only the fresh post-attempt namespace.
    pub(in crate::client::startup_reconciliation) fn reconcile_move(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation,
        UsrRollbackCandidatePreserveNamespaceError,
    > {
        let Self {
            baseline,
            projection,
            parents,
        } = self;
        let pending = parents.attempt_move_once();
        Ok(match pending.reconcile(installation, record, baseline, projection) {
            NewStateCandidatePreserveMoveReconciliation::Applied(reconciliation) => {
                UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation::Applied(
                    UsrRollbackNewStateCandidatePreserveAppliedNamespace {
                        _reconciliation: reconciliation,
                    },
                )
            }
            NewStateCandidatePreserveMoveReconciliation::NotApplied => {
                UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation::NotApplied
            }
            NewStateCandidatePreserveMoveReconciliation::Ambiguous => {
                UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation::Ambiguous
            }
        })
    }
}

fn require_projection(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: &ProjectedNewStateCandidatePreserveNamespace,
) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
    if ProjectedNewStateCandidatePreserveNamespace::capture(snapshot, record)? == *expected {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged)
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_CANDIDATE_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_FINAL_PRE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_candidate_preserve_candidate_sync(hook: impl FnOnce() + 'static) {
    BEFORE_CANDIDATE_SYNC.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_PRE_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_candidate_sync() {
    BEFORE_CANDIDATE_SYNC.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_candidate_sync() {}

#[cfg(test)]
fn run_before_final_pre_capture() {
    BEFORE_FINAL_PRE_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_final_pre_capture() {}
