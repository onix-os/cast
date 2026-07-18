//! Bridge both archived-candidate origins into one POST durability suffix.

use crate::{Installation, transition_journal::TransitionRecord};

use super::{
    UsrRollbackArchivedCandidatePreserveAlreadySatisfiedNamespace,
    UsrRollbackArchivedCandidatePreserveAppliedNamespace, UsrRollbackCandidatePreserveNamespaceError,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    ArchivedCandidatePreservePostMoveDurabilityError, DurableArchivedCandidatePreservePostMoveNamespace,
};

#[must_use = "durable archived candidate namespace evidence must remain sealed"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackArchivedCandidatePreserveDurableNamespace {
    namespace: DurableArchivedCandidatePreservePostMoveNamespace,
}

impl UsrRollbackArchivedCandidatePreserveAppliedNamespace {
    pub(in crate::client::startup_reconciliation) fn complete_post_move_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackArchivedCandidatePreserveDurableNamespace, UsrRollbackCandidatePreserveNamespaceError> {
        let namespace = self
            .reconciliation
            .into_post_move_durability()
            .complete(installation, record)
            .map_err(post_error)?;
        Ok(UsrRollbackArchivedCandidatePreserveDurableNamespace { namespace })
    }
}

impl UsrRollbackArchivedCandidatePreserveAlreadySatisfiedNamespace {
    pub(in crate::client::startup_reconciliation) fn complete_post_move_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackArchivedCandidatePreserveDurableNamespace, UsrRollbackCandidatePreserveNamespaceError> {
        let namespace = self.pending.complete(installation, record).map_err(post_error)?;
        Ok(UsrRollbackArchivedCandidatePreserveDurableNamespace { namespace })
    }
}

impl UsrRollbackArchivedCandidatePreserveDurableNamespace {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
        self.namespace.revalidate(installation, record).map_err(post_error)
    }
}

fn post_error(source: ArchivedCandidatePreservePostMoveDurabilityError) -> UsrRollbackCandidatePreserveNamespaceError {
    UsrRollbackCandidatePreserveNamespaceError::ArchivedEffect(Box::new(source))
}
