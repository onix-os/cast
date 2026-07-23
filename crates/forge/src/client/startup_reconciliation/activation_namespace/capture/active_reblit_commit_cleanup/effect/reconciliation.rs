//! Fresh semantic classification after one cleanup exchange attempt.

use crate::{Installation, transition_journal::TransitionRecord};

use super::super::{
    ActiveReblitCommitCleanupLayout, ProjectedActiveReblitCommitCleanupNamespace,
    RetainedActiveReblitCommitCleanupParents,
};
use super::PendingActiveReblitCommitCleanupExchangeReconciliation;
use crate::client::startup_reconciliation::activation_namespace::capture::{
    CaptureError, NamespaceSnapshot, capture_snapshot,
};

/// Exact fresh Finish evidence produced only by a classified Apply exchange.
#[must_use = "applied ActiveReblit cleanup still requires durability"]
pub(in crate::client::startup_reconciliation) struct AppliedActiveReblitCommitCleanupExchange
{
    parents: RetainedActiveReblitCommitCleanupParents,
    fresh_finish: NamespaceSnapshot,
    fresh_finish_projection: ProjectedActiveReblitCommitCleanupNamespace,
    _raw_report: std::io::Result<()>,
}

impl AppliedActiveReblitCommitCleanupExchange {
    pub(in crate::client::startup_reconciliation) fn into_durability(
        self,
    ) -> super::super::PendingActiveReblitCommitCleanupDurability {
        let Self {
            parents,
            fresh_finish,
            fresh_finish_projection,
            _raw_report: _,
        } = self;
        super::super::PendingActiveReblitCommitCleanupDurability::new(
            parents,
            fresh_finish,
            fresh_finish_projection,
        )
    }
}

/// Fresh-evidence result of one consumed cleanup exchange capability.
#[must_use = "a reconciled ActiveReblit cleanup exchange must be handled"]
pub(in crate::client::startup_reconciliation) enum ActiveReblitCommitCleanupExchangeReconciliation
{
    Applied(AppliedActiveReblitCommitCleanupExchange),
    NotApplied,
    Ambiguous,
}

enum ClassifiedFreshNamespace {
    Applied {
        snapshot: NamespaceSnapshot,
        projection: ProjectedActiveReblitCommitCleanupNamespace,
    },
    NotApplied,
    Ambiguous,
}

impl PendingActiveReblitCommitCleanupExchangeReconciliation {
    pub(in crate::client::startup_reconciliation) fn reconcile(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> ActiveReblitCommitCleanupExchangeReconciliation {
        let Self {
            parents,
            authenticated_apply,
            authenticated_projection,
            raw_report,
        } = self;
        run_before_reconciliation_capture();
        let classification = classify_fresh_namespace(
            record,
            &authenticated_apply,
            &authenticated_projection,
            capture_snapshot(installation, record),
        );
        match classification {
            ClassifiedFreshNamespace::Applied { snapshot, projection } => {
                if parents
                    .revalidate_layout(installation, ActiveReblitCommitCleanupLayout::Finish)
                    .is_err()
                {
                    return ActiveReblitCommitCleanupExchangeReconciliation::Ambiguous;
                }
                ActiveReblitCommitCleanupExchangeReconciliation::Applied(
                    AppliedActiveReblitCommitCleanupExchange {
                        parents,
                        fresh_finish: snapshot,
                        fresh_finish_projection: projection,
                        _raw_report: raw_report,
                    },
                )
            }
            ClassifiedFreshNamespace::NotApplied => {
                if parents
                    .revalidate_layout(installation, ActiveReblitCommitCleanupLayout::Apply)
                    .is_err()
                {
                    return ActiveReblitCommitCleanupExchangeReconciliation::Ambiguous;
                }
                let _raw_report = raw_report;
                ActiveReblitCommitCleanupExchangeReconciliation::NotApplied
            }
            ClassifiedFreshNamespace::Ambiguous => {
                ActiveReblitCommitCleanupExchangeReconciliation::Ambiguous
            }
        }
    }
}

fn classify_fresh_namespace(
    record: &TransitionRecord,
    authenticated_apply: &NamespaceSnapshot,
    authenticated_projection: &ProjectedActiveReblitCommitCleanupNamespace,
    fresh_capture: Result<NamespaceSnapshot, CaptureError>,
) -> ClassifiedFreshNamespace {
    let baseline = match ProjectedActiveReblitCommitCleanupNamespace::capture(authenticated_apply, record) {
        Ok(projection) => projection,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if baseline != *authenticated_projection || baseline.layout != ActiveReblitCommitCleanupLayout::Apply {
        return ClassifiedFreshNamespace::Ambiguous;
    }
    let fresh = match fresh_capture {
        Ok(fresh) => fresh,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if fresh.fingerprint() == authenticated_apply.fingerprint() {
        return ClassifiedFreshNamespace::NotApplied;
    }
    let projection = match ProjectedActiveReblitCommitCleanupNamespace::capture(&fresh, record) {
        Ok(projection) => projection,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if authenticated_projection.require_apply_to_finish(&projection).is_err() {
        return ClassifiedFreshNamespace::Ambiguous;
    }
    ClassifiedFreshNamespace::Applied {
        snapshot: fresh,
        projection,
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_RECONCILIATION_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_commit_cleanup_reconciliation_capture(
    hook: impl FnOnce() + 'static,
) {
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
