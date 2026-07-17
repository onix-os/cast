//! Fresh semantic classification after one ActiveReblit wrapper exchange.

use crate::{Installation, transition_journal::TransitionRecord};

use super::super::{
    ActiveReblitCandidatePreserveLayout, ProjectedActiveReblitCandidatePreserveNamespace,
    RetainedActiveReblitCandidatePreserveParents,
};
use super::PendingActiveReblitCandidatePreserveExchangeReconciliation;
use crate::client::startup_reconciliation::activation_namespace::capture::{
    CaptureError, NamespaceSnapshot, capture_snapshot,
};

/// Opaque retained capabilities produced only by an exact PRE-to-POST exchange.
#[must_use = "an applied ActiveReblit wrapper exchange still requires durability"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct AppliedActiveReblitCandidatePreserveExchangeReconciliation
{
    pub(super) _parents: RetainedActiveReblitCandidatePreserveParents,
    pub(super) _fresh_post: NamespaceSnapshot,
    pub(super) _fresh_post_projection: ProjectedActiveReblitCandidatePreserveNamespace,
    pub(super) _raw_report: std::io::Result<()>,
}

impl AppliedActiveReblitCandidatePreserveExchangeReconciliation {
    /// Discard the uninterpreted syscall report and transfer only fresh,
    /// semantically classified POST evidence into the common suffix.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn into_post_exchange_durability(
        self,
    ) -> super::super::PendingActiveReblitCandidatePreservePostExchangeDurability {
        let Self {
            _parents: parents,
            _fresh_post: authenticated_post,
            _fresh_post_projection: authenticated_post_projection,
            _raw_report: _,
        } = self;
        super::super::PendingActiveReblitCandidatePreservePostExchangeDurability::new(
            parents,
            authenticated_post,
            authenticated_post_projection,
        )
    }
}

/// Fresh-evidence result of one consumed wrapper exchange capability.
#[must_use = "a reconciled ActiveReblit wrapper exchange must be handled"]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ActiveReblitCandidatePreserveExchangeReconciliation
{
    Applied(AppliedActiveReblitCandidatePreserveExchangeReconciliation),
    NotApplied,
    Ambiguous,
}

enum ClassifiedFreshNamespace {
    Applied {
        snapshot: NamespaceSnapshot,
        projection: ProjectedActiveReblitCandidatePreserveNamespace,
    },
    NotApplied,
    Ambiguous,
}

impl PendingActiveReblitCandidatePreserveExchangeReconciliation {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn reconcile(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> ActiveReblitCandidatePreserveExchangeReconciliation {
        let Self {
            parents,
            authenticated_pre,
            authenticated_projection,
            raw_report,
        } = self;
        run_before_reconciliation_capture();
        let classification = classify_fresh_namespace(
            record,
            &authenticated_pre,
            &authenticated_projection,
            capture_snapshot(installation, record),
        );
        match classification {
            ClassifiedFreshNamespace::Applied { snapshot, projection } => {
                if parents
                    .revalidate_layout(installation, ActiveReblitCandidatePreserveLayout::Preserved)
                    .is_err()
                {
                    return ActiveReblitCandidatePreserveExchangeReconciliation::Ambiguous;
                }
                ActiveReblitCandidatePreserveExchangeReconciliation::Applied(
                    AppliedActiveReblitCandidatePreserveExchangeReconciliation {
                        _parents: parents,
                        _fresh_post: snapshot,
                        _fresh_post_projection: projection,
                        _raw_report: raw_report,
                    },
                )
            }
            ClassifiedFreshNamespace::NotApplied => {
                if parents
                    .revalidate_layout(installation, ActiveReblitCandidatePreserveLayout::Staged)
                    .is_err()
                {
                    return ActiveReblitCandidatePreserveExchangeReconciliation::Ambiguous;
                }
                let _raw_report = raw_report;
                ActiveReblitCandidatePreserveExchangeReconciliation::NotApplied
            }
            ClassifiedFreshNamespace::Ambiguous => ActiveReblitCandidatePreserveExchangeReconciliation::Ambiguous,
        }
    }
}

fn classify_fresh_namespace(
    record: &TransitionRecord,
    authenticated_pre: &NamespaceSnapshot,
    authenticated_projection: &ProjectedActiveReblitCandidatePreserveNamespace,
    fresh_capture: Result<NamespaceSnapshot, CaptureError>,
) -> ClassifiedFreshNamespace {
    let baseline_projection = match ProjectedActiveReblitCandidatePreserveNamespace::capture(authenticated_pre, record)
    {
        Ok(projection) => projection,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if baseline_projection != *authenticated_projection
        || baseline_projection.layout != ActiveReblitCandidatePreserveLayout::Staged
    {
        return ClassifiedFreshNamespace::Ambiguous;
    }
    let fresh = match fresh_capture {
        Ok(fresh) => fresh,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if fresh.fingerprint() == authenticated_pre.fingerprint() {
        return ClassifiedFreshNamespace::NotApplied;
    }
    let fresh_projection = match ProjectedActiveReblitCandidatePreserveNamespace::capture(&fresh, record) {
        Ok(projection) => projection,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if authenticated_projection
        .require_staged_to_preserved(&fresh_projection)
        .is_err()
    {
        return ClassifiedFreshNamespace::Ambiguous;
    }
    ClassifiedFreshNamespace::Applied {
        snapshot: fresh,
        projection: fresh_projection,
    }
}

std::thread_local! {
    static BEFORE_RECONCILIATION_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

pub(in crate::client) fn arm_before_active_reblit_candidate_preserve_reconciliation_capture(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_RECONCILIATION_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

fn run_before_reconciliation_capture() {
    BEFORE_RECONCILIATION_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}
