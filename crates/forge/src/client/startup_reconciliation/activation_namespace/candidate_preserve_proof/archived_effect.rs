//! Sealed archived-candidate child-move namespace proof.

mod post_move_durability;

use crate::{Installation, transition_journal::TransitionRecord};

use super::{
    UsrRollbackCandidatePreserveNamespaceError, UsrRollbackCandidatePreserveNamespaceProof,
    UsrRollbackCandidatePreserveTopology, require_topology,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    AppliedArchivedCandidatePreserveMoveReconciliation, ArchivedCandidatePreserveCaptureError,
    ArchivedCandidatePreserveLayout, ArchivedCandidatePreserveMoveReconciliation, NamespaceSnapshot,
    PendingArchivedCandidatePreservePostMoveDurability, ProjectedArchivedCandidatePreserveNamespace,
    RetainedArchivedCandidatePreserveParents, TargetDurableArchivedCandidatePreservePre, capture_snapshot,
};

pub(in crate::client::startup_reconciliation) use post_move_durability::UsrRollbackArchivedCandidatePreserveDurableNamespace;

pub(in crate::client::startup_reconciliation) struct UsrRollbackArchivedCandidatePreserveNamespaceEffectEvidence {
    baseline: NamespaceSnapshot,
    projection: ProjectedArchivedCandidatePreserveNamespace,
    parents: RetainedArchivedCandidatePreserveParents,
    topology: UsrRollbackCandidatePreserveTopology,
}

#[must_use = "prepared archived candidate move must be reconciled"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackArchivedCandidatePreservePreparedNamespace {
    prepared: TargetDurableArchivedCandidatePreservePre,
}

#[must_use = "applied archived candidate preservation still requires POST durability"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackArchivedCandidatePreserveAppliedNamespace {
    pub(super) reconciliation: AppliedArchivedCandidatePreserveMoveReconciliation,
}

#[must_use = "preserved archived candidate evidence still requires POST durability"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackArchivedCandidatePreserveAlreadySatisfiedNamespace {
    pub(super) pending: PendingArchivedCandidatePreservePostMoveDurability,
}

#[must_use = "a consumed archived candidate move must be handled"]
pub(in crate::client::startup_reconciliation) enum UsrRollbackArchivedCandidatePreserveNamespaceApplyReconciliation {
    Applied(UsrRollbackArchivedCandidatePreserveAppliedNamespace),
    NotApplied,
    Ambiguous,
}

impl UsrRollbackCandidatePreserveNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn into_archived_apply_effect_evidence(
        self,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackArchivedCandidatePreserveNamespaceEffectEvidence, UsrRollbackCandidatePreserveNamespaceError>
    {
        let expected = UsrRollbackCandidatePreserveTopology::ArchivedStagedWithCanonicalSlot;
        if self.topology != expected {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }
        let projection =
            ProjectedArchivedCandidatePreserveNamespace::capture(&self.after, record).map_err(archived_effect_error)?;
        let parents =
            RetainedArchivedCandidatePreserveParents::capture(&self.after, record).map_err(archived_effect_error)?;
        Ok(UsrRollbackArchivedCandidatePreserveNamespaceEffectEvidence {
            baseline: self.after,
            projection,
            parents,
            topology: expected,
        })
    }

    pub(in crate::client::startup_reconciliation) fn into_archived_finish_effect_evidence(
        self,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackArchivedCandidatePreserveNamespaceEffectEvidence, UsrRollbackCandidatePreserveNamespaceError>
    {
        let expected = UsrRollbackCandidatePreserveTopology::ArchivedPreserved;
        if self.topology != expected {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }
        let projection =
            ProjectedArchivedCandidatePreserveNamespace::capture(&self.after, record).map_err(archived_effect_error)?;
        let parents = RetainedArchivedCandidatePreserveParents::capture_preserved(&self.after, record)
            .map_err(archived_effect_error)?;
        Ok(UsrRollbackArchivedCandidatePreserveNamespaceEffectEvidence {
            baseline: self.after,
            projection,
            parents,
            topology: expected,
        })
    }
}

impl UsrRollbackArchivedCandidatePreserveNamespaceEffectEvidence {
    pub(in crate::client::startup_reconciliation) fn prepare_move(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackArchivedCandidatePreservePreparedNamespace, UsrRollbackCandidatePreserveNamespaceError> {
        let Self {
            baseline,
            projection,
            parents,
            topology,
        } = self;
        if topology != UsrRollbackCandidatePreserveTopology::ArchivedStagedWithCanonicalSlot {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }
        baseline.revalidate_retained()?;
        require_topology(record, &baseline, topology)?;
        if ProjectedArchivedCandidatePreserveNamespace::capture(&baseline, record).map_err(archived_effect_error)?
            != projection
        {
            return Err(UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged);
        }
        let prepared = parents
            .complete_target_durability(installation, record, baseline, projection)
            .map_err(|source| UsrRollbackCandidatePreserveNamespaceError::ArchivedEffect(Box::new(source)))?;
        Ok(UsrRollbackArchivedCandidatePreservePreparedNamespace { prepared })
    }

    pub(in crate::client::startup_reconciliation) fn reconcile_finish(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackArchivedCandidatePreserveAlreadySatisfiedNamespace, UsrRollbackCandidatePreserveNamespaceError>
    {
        let Self {
            baseline,
            projection,
            parents,
            topology,
        } = self;
        if topology != UsrRollbackCandidatePreserveTopology::ArchivedPreserved {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }
        installation.revalidate_mutable_namespace()?;
        baseline.revalidate_retained()?;
        require_topology(record, &baseline, topology)?;
        if ProjectedArchivedCandidatePreserveNamespace::capture(&baseline, record).map_err(archived_effect_error)?
            != projection
        {
            return Err(UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged);
        }
        parents
            .revalidate_value_identity(installation)
            .map_err(archived_effect_error)?;
        let fresh = capture_snapshot(installation, record)?;
        fresh.revalidate_retained()?;
        if fresh.fingerprint() != baseline.fingerprint() {
            return Err(UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged);
        }
        let fresh_projection =
            ProjectedArchivedCandidatePreserveNamespace::capture(&fresh, record).map_err(archived_effect_error)?;
        if fresh_projection != projection || fresh_projection.layout() != ArchivedCandidatePreserveLayout::Preserved {
            return Err(UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged);
        }
        parents
            .revalidate_value_identity(installation)
            .map_err(archived_effect_error)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackArchivedCandidatePreserveAlreadySatisfiedNamespace {
            pending: PendingArchivedCandidatePreservePostMoveDurability::new(parents, fresh, fresh_projection),
        })
    }
}

impl UsrRollbackArchivedCandidatePreservePreparedNamespace {
    pub(in crate::client::startup_reconciliation) fn reconcile_move(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<
        UsrRollbackArchivedCandidatePreserveNamespaceApplyReconciliation,
        UsrRollbackCandidatePreserveNamespaceError,
    > {
        let pending = self
            .prepared
            .attempt_move_once(installation, record)
            .map_err(|source| UsrRollbackCandidatePreserveNamespaceError::ArchivedEffect(Box::new(source)))?;
        Ok(match pending.reconcile(installation, record) {
            ArchivedCandidatePreserveMoveReconciliation::Applied(reconciliation) => {
                UsrRollbackArchivedCandidatePreserveNamespaceApplyReconciliation::Applied(
                    UsrRollbackArchivedCandidatePreserveAppliedNamespace { reconciliation },
                )
            }
            ArchivedCandidatePreserveMoveReconciliation::NotApplied => {
                UsrRollbackArchivedCandidatePreserveNamespaceApplyReconciliation::NotApplied
            }
            ArchivedCandidatePreserveMoveReconciliation::Ambiguous => {
                UsrRollbackArchivedCandidatePreserveNamespaceApplyReconciliation::Ambiguous
            }
        })
    }
}

fn archived_effect_error(source: ArchivedCandidatePreserveCaptureError) -> UsrRollbackCandidatePreserveNamespaceError {
    UsrRollbackCandidatePreserveNamespaceError::ArchivedEffect(Box::new(source))
}
