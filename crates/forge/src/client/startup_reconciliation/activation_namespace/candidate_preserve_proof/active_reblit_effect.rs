//! Sealed ActiveReblit whole-wrapper effect proof.
//!
//! Exact staged or preserved topology can be consumed without exposing the
//! retained wrapper index, fixed name, or any descriptor.

mod post_exchange_durability;

use crate::{Installation, transition_journal::TransitionRecord};

use super::{
    UsrRollbackCandidatePreserveNamespaceError, UsrRollbackCandidatePreserveNamespaceProof,
    UsrRollbackCandidatePreserveTopology, require_topology,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    ActiveReblitCandidatePreserveEffectError, ActiveReblitCandidatePreserveExchangeReconciliation,
    ActiveReblitCandidatePreserveLayout, AppliedActiveReblitCandidatePreserveExchangeReconciliation, NamespaceSnapshot,
    PendingActiveReblitCandidatePreservePostExchangeDurability, PreparedActiveReblitCandidatePreserveExchange,
    ProjectedActiveReblitCandidatePreserveNamespace, RetainedActiveReblitCandidatePreserveParents, capture_snapshot,
};

pub(in crate::client::startup_reconciliation) use post_exchange_durability::UsrRollbackActiveReblitCandidatePreserveDurableNamespace;

/// Opaque exact staged or preserved namespace evidence.
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitCandidatePreserveNamespaceEffectEvidence {
    baseline: NamespaceSnapshot,
    projection: ProjectedActiveReblitCandidatePreserveNamespace,
    parents: RetainedActiveReblitCandidatePreserveParents,
    topology: UsrRollbackCandidatePreserveTopology,
}

/// Opaque target-durable PRE prepared for the one-shot wrapper exchange.
#[must_use = "prepared ActiveReblit candidate preservation must be reconciled"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitCandidatePreservePreparedNamespace {
    prepared: PreparedActiveReblitCandidatePreserveExchange,
}

/// Opaque POST authority retained after exact fresh classification.
#[must_use = "applied ActiveReblit candidate preservation still requires post-exchange durability"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitCandidatePreserveAppliedNamespace {
    _reconciliation: AppliedActiveReblitCandidatePreserveExchangeReconciliation,
}

/// Opaque exact preserved evidence produced without an exchange attempt.
#[must_use = "already-preserved ActiveReblit evidence still requires post-exchange durability"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedNamespace {
    pending: PendingActiveReblitCandidatePreservePostExchangeDurability,
}

/// Semantic outcome of consuming the one-shot namespace capability.
#[must_use = "a consumed ActiveReblit wrapper exchange must be handled"]
pub(in crate::client::startup_reconciliation) enum UsrRollbackActiveReblitCandidatePreserveNamespaceApplyReconciliation
{
    Applied(UsrRollbackActiveReblitCandidatePreserveAppliedNamespace),
    NotApplied,
    Ambiguous,
}

impl UsrRollbackCandidatePreserveNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn into_active_reblit_apply_effect_evidence(
        self,
        record: &TransitionRecord,
        wrapper_index: usize,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveNamespaceEffectEvidence,
        UsrRollbackCandidatePreserveNamespaceError,
    > {
        let expected = UsrRollbackCandidatePreserveTopology::ActiveReblitStaged { wrapper_index };
        if self.topology != expected {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }
        let projection = ProjectedActiveReblitCandidatePreserveNamespace::capture(&self.after, record)
            .map_err(active_effect_error)?;
        let parents = RetainedActiveReblitCandidatePreserveParents::capture(
            &self.after,
            record,
            ActiveReblitCandidatePreserveLayout::Staged,
        )
        .map_err(active_effect_error)?;
        Ok(UsrRollbackActiveReblitCandidatePreserveNamespaceEffectEvidence {
            baseline: self.after,
            projection,
            parents,
            topology: expected,
        })
    }

    pub(in crate::client::startup_reconciliation) fn into_active_reblit_finish_effect_evidence(
        self,
        record: &TransitionRecord,
        wrapper_index: usize,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveNamespaceEffectEvidence,
        UsrRollbackCandidatePreserveNamespaceError,
    > {
        let expected = UsrRollbackCandidatePreserveTopology::ActiveReblitPreserved { wrapper_index };
        if self.topology != expected {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }
        let projection = ProjectedActiveReblitCandidatePreserveNamespace::capture(&self.after, record)
            .map_err(active_effect_error)?;
        let parents = RetainedActiveReblitCandidatePreserveParents::capture(
            &self.after,
            record,
            ActiveReblitCandidatePreserveLayout::Preserved,
        )
        .map_err(active_effect_error)?;
        Ok(UsrRollbackActiveReblitCandidatePreserveNamespaceEffectEvidence {
            baseline: self.after,
            projection,
            parents,
            topology: expected,
        })
    }
}

impl UsrRollbackActiveReblitCandidatePreserveNamespaceEffectEvidence {
    /// Complete the exact PRE durability prefix and return a one-shot lease.
    pub(in crate::client::startup_reconciliation) fn prepare_exchange(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackActiveReblitCandidatePreservePreparedNamespace, UsrRollbackCandidatePreserveNamespaceError>
    {
        let Self {
            baseline,
            projection,
            parents,
            topology,
        } = self;
        let UsrRollbackCandidatePreserveTopology::ActiveReblitStaged { .. } = topology else {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        };
        baseline.revalidate_retained()?;
        require_topology(record, &baseline, topology)?;
        if ProjectedActiveReblitCandidatePreserveNamespace::capture(&baseline, record).map_err(active_effect_error)?
            != projection
        {
            return Err(UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged);
        }
        let prepared = parents
            .prepare_exchange(installation, record, baseline, projection)
            .map_err(active_effect_error)?;
        Ok(UsrRollbackActiveReblitCandidatePreservePreparedNamespace { prepared })
    }

    /// Consume exact preserved evidence without issuing an exchange.
    pub(in crate::client::startup_reconciliation) fn reconcile_finish(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedNamespace,
        UsrRollbackCandidatePreserveNamespaceError,
    > {
        let Self {
            baseline,
            projection,
            parents,
            topology,
        } = self;
        let UsrRollbackCandidatePreserveTopology::ActiveReblitPreserved { .. } = topology else {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        };
        installation.revalidate_mutable_namespace()?;
        baseline.revalidate_retained()?;
        require_topology(record, &baseline, topology)?;
        if ProjectedActiveReblitCandidatePreserveNamespace::capture(&baseline, record).map_err(active_effect_error)?
            != projection
        {
            return Err(UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged);
        }
        parents
            .revalidate_layout(installation, ActiveReblitCandidatePreserveLayout::Preserved)
            .map_err(active_effect_error)?;
        let fresh = capture_snapshot(installation, record)?;
        fresh.revalidate_retained()?;
        if fresh.fingerprint() != baseline.fingerprint() {
            return Err(UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged);
        }
        let fresh_projection =
            ProjectedActiveReblitCandidatePreserveNamespace::capture(&fresh, record).map_err(active_effect_error)?;
        if fresh_projection != projection {
            return Err(UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged);
        }
        parents
            .revalidate_layout(installation, ActiveReblitCandidatePreserveLayout::Preserved)
            .map_err(active_effect_error)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedNamespace {
            pending: PendingActiveReblitCandidatePreservePostExchangeDurability::new(parents, fresh, fresh_projection),
        })
    }
}

impl UsrRollbackActiveReblitCandidatePreservePreparedNamespace {
    pub(in crate::client::startup_reconciliation) fn reconcile_exchange(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveNamespaceApplyReconciliation,
        UsrRollbackCandidatePreserveNamespaceError,
    > {
        let pending = self
            .prepared
            .attempt_exchange_once(installation, record)
            .map_err(active_effect_error)?;
        Ok(match pending.reconcile(installation, record) {
            ActiveReblitCandidatePreserveExchangeReconciliation::Applied(reconciliation) => {
                UsrRollbackActiveReblitCandidatePreserveNamespaceApplyReconciliation::Applied(
                    UsrRollbackActiveReblitCandidatePreserveAppliedNamespace {
                        _reconciliation: reconciliation,
                    },
                )
            }
            ActiveReblitCandidatePreserveExchangeReconciliation::NotApplied => {
                UsrRollbackActiveReblitCandidatePreserveNamespaceApplyReconciliation::NotApplied
            }
            ActiveReblitCandidatePreserveExchangeReconciliation::Ambiguous => {
                UsrRollbackActiveReblitCandidatePreserveNamespaceApplyReconciliation::Ambiguous
            }
        })
    }
}

fn active_effect_error(source: ActiveReblitCandidatePreserveEffectError) -> UsrRollbackCandidatePreserveNamespaceError {
    UsrRollbackCandidatePreserveNamespaceError::ActiveReblitEffect(Box::new(source))
}
