//! Semantic reconciliation after one reverse `/usr` exchange attempt.
//!
//! The raw syscall report is deliberately never interpreted. Every pending
//! attempt first crosses a reverse-specific test seam and then performs one
//! fresh authenticated namespace capture. Only that fresh evidence can name
//! the attempt `Applied`, `NotApplied`, or `Ambiguous`.

use crate::{Installation, transition_identity::Error, transition_journal::TransitionRecord};

use super::PendingReverseExchangeReconciliation;
use crate::client::startup_reconciliation::activation_namespace::{
    capture::{
        CaptureError, DurableReverseExchangeNamespace, NamespaceSnapshot, ProjectedReverseNamespace,
        RetainedReverseExchangeParents, ReverseExchangeDurabilityError, capture_snapshot,
    },
    policy::UsrExchangeLayout,
};

/// Opaque capabilities retained only after an exact POST-to-PRE exchange.
///
/// The fields remain private to this functional module. Later durability work
/// may consume this type here without exposing a descriptor or snapshot getter.
#[must_use = "an applied reverse exchange still requires parent durability"]
#[allow(dead_code)] // retained behind the unwired reverse durability executor
pub(in crate::client::startup_reconciliation::activation_namespace) struct AppliedReverseExchangeReconciliation {
    parents: RetainedReverseExchangeParents,
    fresh_pre: NamespaceSnapshot,
    fresh_pre_projection: ProjectedReverseNamespace,
    raw_report: Result<(), Error>,
}

/// Opaque durable descendant of an applied reverse exchange. The raw syscall
/// report remains retained as diagnostic evidence and is never interpreted as
/// the semantic outcome.
#[must_use = "durable applied reverse-exchange evidence must be consumed by persistence"]
#[allow(dead_code)] // consumed by the later journal-persistence checkpoint
pub(in crate::client::startup_reconciliation::activation_namespace) struct DurableAppliedReverseExchangeReconciliation {
    _namespace: DurableReverseExchangeNamespace,
    _raw_report: Result<(), Error>,
}

impl AppliedReverseExchangeReconciliation {
    /// Consume applied reconciliation through the exact two-parent durability
    /// sequence. `raw_report` is only transferred to the opaque completion;
    /// it is not inspected before, during, or after the barriers.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn complete_parent_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<DurableAppliedReverseExchangeReconciliation, ReverseExchangeDurabilityError> {
        let Self {
            parents,
            fresh_pre,
            fresh_pre_projection,
            raw_report,
        } = self;
        let namespace = parents.complete_parent_durability(installation, record, fresh_pre, fresh_pre_projection)?;
        Ok(DurableAppliedReverseExchangeReconciliation {
            _namespace: namespace,
            _raw_report: raw_report,
        })
    }
}

/// Namespace-derived result of exactly one reverse exchange attempt.
///
/// Failure variants are intentionally fieldless: neither can be retried with
/// a retained descriptor, baseline, or pending effect capability.
#[must_use = "a reconciled reverse exchange outcome must be handled"]
#[allow(dead_code)] // consumed by the later rollback-reverse executor
pub(in crate::client::startup_reconciliation::activation_namespace) enum ReverseExchangeReconciliation {
    Applied(AppliedReverseExchangeReconciliation),
    NotApplied,
    Ambiguous,
}

enum ClassifiedFreshNamespace {
    Applied {
        snapshot: NamespaceSnapshot,
        projection: ProjectedReverseNamespace,
    },
    NotApplied,
    Ambiguous,
}

impl PendingReverseExchangeReconciliation {
    /// Consume the pending raw attempt into a fresh namespace classification.
    ///
    /// Capture is unconditional and precedes all classification. The raw
    /// report remains sealed inside `self` until after classification and is
    /// never inspected, matched, or used as semantic evidence.
    #[allow(dead_code)] // invoked by the later rollback-reverse executor
    pub(in crate::client::startup_reconciliation::activation_namespace) fn reconcile(
        self,
        installation: &Installation,
        record: &TransitionRecord,
        authenticated_post_baseline: NamespaceSnapshot,
        authenticated_post_projection: ProjectedReverseNamespace,
    ) -> ReverseExchangeReconciliation {
        run_before_reverse_exchange_reconciliation_capture();
        let fresh_capture = capture_snapshot(installation, record);
        let classification = classify_fresh_namespace(
            record,
            &authenticated_post_baseline,
            &authenticated_post_projection,
            fresh_capture,
        );

        match classification {
            ClassifiedFreshNamespace::Applied {
                snapshot: fresh_pre,
                projection: fresh_pre_projection,
            } => {
                let Self { parents, raw_report } = self;
                if parents.revalidate_value_identity(installation).is_err() {
                    return ReverseExchangeReconciliation::Ambiguous;
                }
                ReverseExchangeReconciliation::Applied(AppliedReverseExchangeReconciliation {
                    parents,
                    fresh_pre,
                    fresh_pre_projection,
                    raw_report,
                })
            }
            ClassifiedFreshNamespace::NotApplied => {
                let Self {
                    parents,
                    raw_report: _raw_report,
                } = self;
                if parents.revalidate_value_identity(installation).is_err() {
                    return ReverseExchangeReconciliation::Ambiguous;
                }
                ReverseExchangeReconciliation::NotApplied
            }
            ClassifiedFreshNamespace::Ambiguous => ReverseExchangeReconciliation::Ambiguous,
        }
    }
}

fn classify_fresh_namespace(
    record: &TransitionRecord,
    authenticated_post_baseline: &NamespaceSnapshot,
    authenticated_post_projection: &ProjectedReverseNamespace,
    fresh_capture: Result<NamespaceSnapshot, CaptureError>,
) -> ClassifiedFreshNamespace {
    let baseline_projection = match ProjectedReverseNamespace::capture(authenticated_post_baseline, record) {
        Ok(projection) => projection,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if baseline_projection.layout() != UsrExchangeLayout::Post || baseline_projection != *authenticated_post_projection
    {
        return ClassifiedFreshNamespace::Ambiguous;
    }

    let fresh = match fresh_capture {
        Ok(fresh) => fresh,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if fresh.fingerprint() == authenticated_post_baseline.fingerprint() {
        return ClassifiedFreshNamespace::NotApplied;
    }
    let fresh_projection = match ProjectedReverseNamespace::capture(&fresh, record) {
        Ok(projection) => projection,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if authenticated_post_projection
        .require_post_to_pre(&fresh_projection)
        .is_err()
    {
        return ClassifiedFreshNamespace::Ambiguous;
    }
    ClassifiedFreshNamespace::Applied {
        snapshot: fresh,
        projection: fresh_projection,
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_REVERSE_EXCHANGE_RECONCILIATION_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_reverse_exchange_reconciliation_capture(hook: impl FnOnce() + 'static) {
    BEFORE_REVERSE_EXCHANGE_RECONCILIATION_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_reverse_exchange_reconciliation_capture() {
    BEFORE_REVERSE_EXCHANGE_RECONCILIATION_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_reverse_exchange_reconciliation_capture() {}

#[cfg(test)]
mod tests;
