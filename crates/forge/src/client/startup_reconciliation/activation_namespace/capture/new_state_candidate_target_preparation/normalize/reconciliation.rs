//! Fresh semantic classification after one NewState target normalization.

use crate::{Installation, transition_journal::TransitionRecord};

use super::{FreshCanonicalNewStateTargetNormalizeNamespace, PendingNewStateTargetNormalizeReconciliation};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    NewStateTargetNormalizeLayout, ProjectedNewStateTargetNormalizeNamespace, capture_snapshot,
};

/// Namespace-derived result of consuming exactly one normalization capability.
///
/// The canonical branch privately retains its fresh same-inode namespace until
/// ordered target and quarantine-parent durability consumes it. The externally
/// observable failure branches remain fieldless and authorize no retry or move.
#[must_use = "a reconciled NewState target-normalization attempt must be handled"]
pub(in crate::client::startup_reconciliation::activation_namespace) enum NewStateTargetNormalizeReconciliation {
    Canonical(FreshCanonicalNewStateTargetNormalizeNamespace),
    NotApplied,
    Ambiguous,
}

impl PendingNewStateTargetNormalizeReconciliation {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn reconcile(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> NewStateTargetNormalizeReconciliation {
        let Self {
            authenticated_pre,
            authenticated_pre_projection,
            raw_report: _raw_report,
        } = self;
        let baseline_projection = match ProjectedNewStateTargetNormalizeNamespace::capture(&authenticated_pre, record) {
            Ok(projection)
                if projection.layout() == NewStateTargetNormalizeLayout::RestrictiveResidue
                    && projection == authenticated_pre_projection =>
            {
                projection
            }
            Ok(_) | Err(_) => return NewStateTargetNormalizeReconciliation::Ambiguous,
        };

        run_before_reconciliation_capture();
        let fresh = match capture_snapshot(installation, record) {
            Ok(fresh) => fresh,
            Err(_) => return NewStateTargetNormalizeReconciliation::Ambiguous,
        };
        if fresh.fingerprint() == authenticated_pre.fingerprint() {
            return NewStateTargetNormalizeReconciliation::NotApplied;
        }
        let fresh_projection = match ProjectedNewStateTargetNormalizeNamespace::capture(&fresh, record) {
            Ok(projection) => projection,
            Err(_) => return NewStateTargetNormalizeReconciliation::Ambiguous,
        };
        if baseline_projection
            .require_residue_to_empty_private(&fresh_projection)
            .is_err()
        {
            return NewStateTargetNormalizeReconciliation::Ambiguous;
        }
        NewStateTargetNormalizeReconciliation::Canonical(FreshCanonicalNewStateTargetNormalizeNamespace::new(
            fresh,
            fresh_projection,
        ))
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_RECONCILIATION_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_target_normalize_reconciliation_capture(hook: impl FnOnce() + 'static) {
    BEFORE_RECONCILIATION_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_reconciliation_capture() {
    BEFORE_RECONCILIATION_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_reconciliation_capture() {}
