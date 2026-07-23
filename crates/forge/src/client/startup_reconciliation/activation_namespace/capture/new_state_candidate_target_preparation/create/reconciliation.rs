//! Fresh semantic classification after one NewState target-creation attempt.

use crate::{Installation, transition_journal::TransitionRecord};

use super::PendingNewStateTargetCreateReconciliation;
use crate::client::startup_reconciliation::activation_namespace::capture::{
    NewStateTargetCreateLayout, ProjectedNewStateTargetCreateNamespace, capture_snapshot,
};

/// Namespace-derived result of consuming exactly one creation capability.
///
/// Every variant is fieldless. Even a safely prepared target requires a new
/// startup entry, and no result can authorize an in-process retry or move.
#[must_use = "a reconciled NewState target-creation attempt must be handled"]
pub(in crate::client::startup_reconciliation::activation_namespace) enum NewStateTargetCreateReconciliation {
    RestartRequired,
    NotApplied,
    Ambiguous,
}

impl PendingNewStateTargetCreateReconciliation {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn reconcile(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> NewStateTargetCreateReconciliation {
        let Self {
            authenticated_pre,
            authenticated_pre_projection,
            raw_report: _raw_report,
        } = self;
        let baseline_projection = match ProjectedNewStateTargetCreateNamespace::capture(&authenticated_pre, record) {
            Ok(projection)
                if projection.layout() == NewStateTargetCreateLayout::Absent
                    && projection == authenticated_pre_projection =>
            {
                projection
            }
            Ok(_) | Err(_) => return NewStateTargetCreateReconciliation::Ambiguous,
        };

        run_before_reconciliation_capture();
        let fresh = match capture_snapshot(installation, record) {
            Ok(fresh) => fresh,
            Err(_) => return NewStateTargetCreateReconciliation::Ambiguous,
        };
        if fresh.fingerprint() == authenticated_pre.fingerprint() {
            return NewStateTargetCreateReconciliation::NotApplied;
        }
        let fresh_projection = match ProjectedNewStateTargetCreateNamespace::capture(&fresh, record) {
            Ok(projection) => projection,
            Err(_) => return NewStateTargetCreateReconciliation::Ambiguous,
        };
        if baseline_projection
            .require_absent_to_prepared(&fresh_projection)
            .is_err()
        {
            return NewStateTargetCreateReconciliation::Ambiguous;
        }
        NewStateTargetCreateReconciliation::RestartRequired
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_RECONCILIATION_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_target_create_reconciliation_capture(hook: impl FnOnce() + 'static) {
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
