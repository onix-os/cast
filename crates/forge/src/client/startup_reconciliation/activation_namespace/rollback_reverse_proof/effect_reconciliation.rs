//! Final namespace proof and semantic reconciliation for one reverse effect.
//!
//! This child owns the private fields of the read-only reverse proof while
//! keeping retained descriptors opaque. Applying consumes the POST evidence
//! into one raw attempt and classifies only a fresh capture. Finishing consumes
//! exact PRE evidence without making an exchange attempt.

mod durability;

use crate::{Installation, transition_journal::TransitionRecord};

use super::{
    UsrRollbackReverseNamespaceEffectEvidence, UsrRollbackReverseNamespaceError, require_layout,
    require_matching_fingerprints, require_projection,
};
use crate::client::startup_reconciliation::activation_namespace::{
    capture::{
        AppliedReverseExchangeReconciliation, NamespaceSnapshot, ProjectedReverseNamespace,
        RetainedReverseExchangeParents, ReverseExchangeReconciliation, capture_snapshot,
    },
    policy::UsrExchangeLayout,
};

pub(in crate::client::startup_reconciliation) use durability::UsrRollbackReverseDurableNamespace;
#[cfg(test)]
pub(in crate::client) use durability::{
    UsrRollbackReverseNamespaceDurabilityEvent, UsrRollbackReverseNamespaceDurabilityFaultPoint,
    arm_before_usr_rollback_reverse_durable_namespace_capture,
    arm_before_usr_rollback_reverse_namespace_final_pre_capture,
    arm_before_usr_rollback_reverse_namespace_installation_root_sync,
    arm_usr_rollback_reverse_namespace_durability_fault, reset_usr_rollback_reverse_namespace_durability_events,
    take_usr_rollback_reverse_namespace_durability_events,
};

/// Opaque POST-to-PRE namespace authority retained after fresh reconciliation.
#[must_use = "an applied reverse exchange still requires parent durability"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackReverseAppliedNamespace {
    reconciliation: AppliedReverseExchangeReconciliation,
}

/// Opaque exact-PRE namespace authority produced without an exchange attempt.
#[must_use = "an already-satisfied reverse exchange still requires parent durability"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackReverseAlreadySatisfiedNamespace {
    parents: RetainedReverseExchangeParents,
    fresh_pre: NamespaceSnapshot,
    fresh_pre_projection: ProjectedReverseNamespace,
}

/// Semantic result of consuming one exact POST namespace effect capability.
///
/// Failure variants deliberately contain no retained evidence and therefore
/// cannot authorize a retry.
#[must_use = "a consumed reverse exchange must be handled"]
pub(in crate::client::startup_reconciliation) enum UsrRollbackReverseNamespaceApplyReconciliation {
    Applied(UsrRollbackReverseAppliedNamespace),
    NotApplied,
    Ambiguous,
}

struct FinalReverseNamespace {
    baseline: NamespaceSnapshot,
    projection: ProjectedReverseNamespace,
    parents: RetainedReverseExchangeParents,
}

impl UsrRollbackReverseNamespaceEffectEvidence {
    /// Consume exact POST evidence into at most one exchange attempt.
    pub(in crate::client::startup_reconciliation) fn reconcile_apply(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackReverseNamespaceApplyReconciliation, UsrRollbackReverseNamespaceError> {
        let FinalReverseNamespace {
            baseline,
            projection,
            parents,
        } = self.final_exact_namespace(installation, record, UsrExchangeLayout::Post)?;
        let pending = parents.attempt_usr_exchange_once();
        Ok(match pending.reconcile(installation, record, baseline, projection) {
            ReverseExchangeReconciliation::Applied(reconciliation) => {
                UsrRollbackReverseNamespaceApplyReconciliation::Applied(UsrRollbackReverseAppliedNamespace {
                    reconciliation,
                })
            }
            ReverseExchangeReconciliation::NotApplied => UsrRollbackReverseNamespaceApplyReconciliation::NotApplied,
            ReverseExchangeReconciliation::Ambiguous => UsrRollbackReverseNamespaceApplyReconciliation::Ambiguous,
        })
    }

    /// Consume exact PRE evidence without issuing an exchange attempt.
    pub(in crate::client::startup_reconciliation) fn reconcile_finish(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackReverseAlreadySatisfiedNamespace, UsrRollbackReverseNamespaceError> {
        let FinalReverseNamespace {
            baseline,
            projection,
            parents,
        } = self.final_exact_namespace(installation, record, UsrExchangeLayout::Pre)?;
        Ok(UsrRollbackReverseAlreadySatisfiedNamespace {
            parents,
            fresh_pre: baseline,
            fresh_pre_projection: projection,
        })
    }

    /// Perform the last full, exact recapture before either by-value path may
    /// leave this read-only proof layer. The returned parent capability can be
    /// consumed only by the caller and is never exposed through an accessor.
    fn final_exact_namespace(
        self,
        installation: &Installation,
        record: &TransitionRecord,
        expected_layout: UsrExchangeLayout,
    ) -> Result<FinalReverseNamespace, UsrRollbackReverseNamespaceError> {
        let Self {
            baseline,
            projection,
            parents,
            layout,
        } = self;
        if layout != expected_layout || projection.layout() != expected_layout {
            return Err(UsrRollbackReverseNamespaceError::LayoutChanged);
        }

        installation.revalidate_mutable_namespace()?;
        baseline.revalidate_retained()?;
        require_projection(record, &baseline, &projection)?;
        require_layout(record, &baseline, expected_layout)?;
        parents.revalidate_value_identity(installation)?;

        run_before_reverse_effect_final_namespace_capture();
        let fresh = capture_snapshot(installation, record)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&baseline, &fresh)?;
        require_projection(record, &fresh, &projection)?;
        require_layout(record, &fresh, expected_layout)?;
        parents.revalidate_value_identity(installation)?;
        installation.revalidate_mutable_namespace()?;

        Ok(FinalReverseNamespace {
            baseline: fresh,
            projection,
            parents,
        })
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_REVERSE_EFFECT_FINAL_NAMESPACE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_reverse_effect_final_namespace_capture(hook: impl FnOnce() + 'static) {
    BEFORE_REVERSE_EFFECT_FINAL_NAMESPACE_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_reverse_effect_final_namespace_capture() {
    BEFORE_REVERSE_EFFECT_FINAL_NAMESPACE_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_reverse_effect_final_namespace_capture() {}
