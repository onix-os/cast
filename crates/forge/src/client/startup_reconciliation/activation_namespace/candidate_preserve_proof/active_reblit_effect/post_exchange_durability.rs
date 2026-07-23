//! Namespace bridge into the shared ActiveReblit POST durability suffix.

use crate::{Installation, transition_journal::TransitionRecord};

use super::{
    UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedNamespace,
    UsrRollbackActiveReblitCandidatePreserveAppliedNamespace, UsrRollbackCandidatePreserveNamespaceError,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    ActiveReblitCandidatePreservePostExchangeDurabilityError, DurableActiveReblitCandidatePreservePostExchangeNamespace,
};

/// Common namespace proof after either origin completed every POST barrier.
#[must_use = "durable ActiveReblit candidate-preservation namespace evidence must remain sealed"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitCandidatePreserveDurableNamespace {
    namespace: DurableActiveReblitCandidatePreservePostExchangeNamespace,
}

impl UsrRollbackActiveReblitCandidatePreserveAppliedNamespace {
    /// Consume freshly applied POST evidence through the common suffix.
    pub(in crate::client::startup_reconciliation) fn complete_post_exchange_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackActiveReblitCandidatePreserveDurableNamespace, UsrRollbackCandidatePreserveNamespaceError>
    {
        let pending = self._reconciliation.into_post_exchange_durability();
        let namespace = pending
            .complete(installation, record)
            .map_err(post_exchange_durability_error)?;
        Ok(UsrRollbackActiveReblitCandidatePreserveDurableNamespace { namespace })
    }
}

impl UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedNamespace {
    /// Consume independently admitted POST evidence through the same suffix.
    pub(in crate::client::startup_reconciliation) fn complete_post_exchange_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackActiveReblitCandidatePreserveDurableNamespace, UsrRollbackCandidatePreserveNamespaceError>
    {
        let namespace = self
            .pending
            .complete(installation, record)
            .map_err(post_exchange_durability_error)?;
        Ok(UsrRollbackActiveReblitCandidatePreserveDurableNamespace { namespace })
    }
}

impl UsrRollbackActiveReblitCandidatePreserveDurableNamespace {
    /// Revalidate the sealed final POST without repeating any sync.
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
        self.namespace
            .revalidate(installation, record)
            .map_err(post_exchange_durability_error)
    }
}

fn post_exchange_durability_error(
    source: ActiveReblitCandidatePreservePostExchangeDurabilityError,
) -> UsrRollbackCandidatePreserveNamespaceError {
    UsrRollbackCandidatePreserveNamespaceError::ActiveReblitEffect(Box::new(source))
}
