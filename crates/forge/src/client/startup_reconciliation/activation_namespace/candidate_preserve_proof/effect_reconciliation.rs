//! Target-durable PRE proof and one-shot reconciliation for NewState preservation.
//!
//! Candidate `syncfs` plus retained-tree fsync runs before the exact target
//! and quarantine-parent barriers. Only their final fresh PRE can reach the
//! one-shot move. Post-move durability remains intentionally deferred.

use crate::{Installation, transition_journal::TransitionRecord};

use super::{
    UsrRollbackCandidatePreserveNamespaceError, UsrRollbackCandidatePreserveTopology,
    UsrRollbackNewStateCandidatePreserveNamespaceEffectEvidence, require_topology,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    AppliedNewStateCandidatePreserveMoveReconciliation, NamespaceSnapshot, NewStateCandidatePreserveLayout,
    NewStateCandidatePreserveMoveReconciliation, ProjectedNewStateCandidatePreserveNamespace,
    TargetDurableNewStateCandidatePreservePre,
};

/// Opaque POST namespace authority retained after fresh reconciliation.
///
/// No post-move durability or persistence method exists at this checkpoint.
#[must_use = "an applied candidate-preservation move still requires post-move durability"]
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

/// Opaque namespace authority prepared by candidate sync, exact target and
/// quarantine-parent durability, and a final fresh PRE capture.
///
/// Keeping this value separate lets the enclosing authority repeat its exact
/// journal, database, plan, and installation checks after namespace
/// preparation and immediately before the rename attempt.
#[must_use = "prepared NewState candidate-preservation namespace authority must be consumed"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackNewStateCandidatePreservePreparedNamespace {
    durable_pre: TargetDurableNewStateCandidatePreservePre,
}

impl UsrRollbackNewStateCandidatePreserveNamespaceEffectEvidence {
    /// Sync the exact candidate, destination, and quarantine parent, then
    /// prove one final fresh PRE1 namespace.
    ///
    /// This deliberately stops before the one-shot move so the enclosing
    /// authority can repeat every non-namespace evidence check.
    pub(in crate::client::startup_reconciliation) fn prepare_move(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackNewStateCandidatePreservePreparedNamespace, UsrRollbackCandidatePreserveNamespaceError> {
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
        let durable_pre = parents.complete_target_durability(installation, record, baseline, projection)?;
        Ok(UsrRollbackNewStateCandidatePreservePreparedNamespace { durable_pre })
    }
}

impl UsrRollbackNewStateCandidatePreservePreparedNamespace {
    /// Consume target-durable PRE1 authority through final exact revalidation,
    /// at most one no-replace move, and fresh post-attempt classification.
    pub(in crate::client::startup_reconciliation) fn reconcile_move(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation,
        UsrRollbackCandidatePreserveNamespaceError,
    > {
        let Self { durable_pre } = self;
        let pending = durable_pre.attempt_move_once(installation, record)?;
        Ok(match pending.reconcile(installation, record) {
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
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_candidate_preserve_candidate_sync(hook: impl FnOnce() + 'static) {
    BEFORE_CANDIDATE_SYNC.with(|slot| {
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
