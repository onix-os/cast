//! Consuming durability suffix for reconciled reverse namespace effects.
//!
//! Applied and already-satisfied evidence remain disjoint until each has
//! completed the same retained-parent sync sequence and final exact PRE proof.

use crate::{Installation, transition_journal::TransitionRecord};

use super::{UsrRollbackReverseAlreadySatisfiedNamespace, UsrRollbackReverseAppliedNamespace};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    DurableAppliedReverseExchangeReconciliation, DurableReverseExchangeNamespace,
};

#[cfg(test)]
pub(in crate::client) use crate::client::startup_reconciliation::activation_namespace::capture::{
    ReverseExchangeDurabilityEvent as UsrRollbackReverseNamespaceDurabilityEvent,
    ReverseExchangeDurabilityFaultPoint as UsrRollbackReverseNamespaceDurabilityFaultPoint,
    arm_before_reverse_exchange_durable_revalidation_capture as arm_before_usr_rollback_reverse_durable_namespace_capture,
    arm_before_reverse_exchange_final_pre_capture as arm_before_usr_rollback_reverse_namespace_final_pre_capture,
    arm_before_reverse_exchange_installation_root_sync as arm_before_usr_rollback_reverse_namespace_installation_root_sync,
    arm_reverse_exchange_durability_fault as arm_usr_rollback_reverse_namespace_durability_fault,
    reset_reverse_exchange_durability_events as reset_usr_rollback_reverse_namespace_durability_events,
    take_reverse_exchange_durability_events as take_usr_rollback_reverse_namespace_durability_events,
};

/// Opaque namespace proof shared only after the Applied or AlreadySatisfied
/// path has independently completed both parent barriers and final PRE proof.
#[must_use = "durable rollback-reverse namespace evidence must be consumed by persistence"]
#[allow(dead_code)] // consumed by the later journal-persistence checkpoint
pub(in crate::client::startup_reconciliation) struct UsrRollbackReverseDurableNamespace {
    _origin: DurableReverseOrigin,
}

#[allow(dead_code)] // retained privately for diagnostic provenance
enum DurableReverseOrigin {
    Applied(DurableAppliedReverseExchangeReconciliation),
    AlreadySatisfied(DurableReverseExchangeNamespace),
}

impl UsrRollbackReverseDurableNamespace {
    /// Borrow and freshly revalidate the exact durable PRE proof for the later
    /// persistence boundary.
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<(), super::UsrRollbackReverseNamespaceError> {
        match &self._origin {
            DurableReverseOrigin::Applied(namespace) => namespace.revalidate(installation, record)?,
            DurableReverseOrigin::AlreadySatisfied(namespace) => namespace.revalidate(installation, record)?,
        }
        Ok(())
    }
}

impl UsrRollbackReverseAppliedNamespace {
    /// Consume applied reconciliation through staging-parent sync, retained
    /// installation-root sync, and the final exact fresh PRE proof.
    pub(in crate::client::startup_reconciliation) fn complete_parent_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackReverseDurableNamespace, super::UsrRollbackReverseNamespaceError> {
        let durable = self.reconciliation.complete_parent_durability(installation, record)?;
        Ok(UsrRollbackReverseDurableNamespace {
            _origin: DurableReverseOrigin::Applied(durable),
        })
    }
}

impl UsrRollbackReverseAlreadySatisfiedNamespace {
    /// Consume exact PRE no-op reconciliation through the identical two
    /// parent barriers and final exact fresh PRE proof.
    pub(in crate::client::startup_reconciliation) fn complete_parent_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackReverseDurableNamespace, super::UsrRollbackReverseNamespaceError> {
        let durable =
            self.parents
                .complete_parent_durability(installation, record, self.fresh_pre, self.fresh_pre_projection)?;
        Ok(UsrRollbackReverseDurableNamespace {
            _origin: DurableReverseOrigin::AlreadySatisfied(durable),
        })
    }
}

#[cfg(test)]
mod tests;
