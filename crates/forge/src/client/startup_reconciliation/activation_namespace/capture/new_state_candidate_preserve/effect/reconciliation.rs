//! Fresh semantic classification after one NewState candidate move attempt.

use crate::{Installation, transition_journal::TransitionRecord};

use super::PendingNewStateCandidatePreserveMoveReconciliation;
use crate::client::startup_reconciliation::activation_namespace::capture::{
    CaptureError, NamespaceSnapshot, NewStateCandidatePreserveLayout, ProjectedNewStateCandidatePreserveNamespace,
    RetainedNewStateCandidatePreserveParents, capture_snapshot,
};

/// Opaque retained capabilities produced only by an exact PRE-to-POST move.
#[must_use = "an applied NewState candidate move still requires durability"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct AppliedNewStateCandidatePreserveMoveReconciliation
{
    pub(super) _parents: RetainedNewStateCandidatePreserveParents,
    pub(super) _fresh_post: NamespaceSnapshot,
    pub(super) _fresh_post_projection: ProjectedNewStateCandidatePreserveNamespace,
    pub(super) _raw_report: std::io::Result<()>,
}

/// Namespace-derived result of exactly one no-replace candidate move.
///
/// Failure variants carry no baseline, descriptor, or retry capability.
#[must_use = "a reconciled NewState candidate move must be handled"]
pub(in crate::client::startup_reconciliation::activation_namespace) enum NewStateCandidatePreserveMoveReconciliation {
    Applied(AppliedNewStateCandidatePreserveMoveReconciliation),
    NotApplied,
    Ambiguous,
}

enum ClassifiedFreshNamespace {
    Applied {
        snapshot: NamespaceSnapshot,
        projection: ProjectedNewStateCandidatePreserveNamespace,
    },
    NotApplied,
    Ambiguous,
}

impl PendingNewStateCandidatePreserveMoveReconciliation {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn reconcile(
        self,
        installation: &Installation,
        record: &TransitionRecord,
        authenticated_pre: NamespaceSnapshot,
        authenticated_pre_projection: ProjectedNewStateCandidatePreserveNamespace,
    ) -> NewStateCandidatePreserveMoveReconciliation {
        run_before_reconciliation_capture();
        let fresh_capture = capture_snapshot(installation, record);
        let classification =
            classify_fresh_namespace(record, &authenticated_pre, &authenticated_pre_projection, fresh_capture);

        match classification {
            ClassifiedFreshNamespace::Applied { snapshot, projection } => {
                let Self { parents, raw_report } = self;
                if parents.revalidate_value_identity(installation).is_err() {
                    return NewStateCandidatePreserveMoveReconciliation::Ambiguous;
                }
                NewStateCandidatePreserveMoveReconciliation::Applied(
                    AppliedNewStateCandidatePreserveMoveReconciliation {
                        _parents: parents,
                        _fresh_post: snapshot,
                        _fresh_post_projection: projection,
                        _raw_report: raw_report,
                    },
                )
            }
            ClassifiedFreshNamespace::NotApplied => {
                let Self {
                    parents,
                    raw_report: _raw_report,
                } = self;
                if parents.revalidate_value_identity(installation).is_err() {
                    return NewStateCandidatePreserveMoveReconciliation::Ambiguous;
                }
                NewStateCandidatePreserveMoveReconciliation::NotApplied
            }
            ClassifiedFreshNamespace::Ambiguous => NewStateCandidatePreserveMoveReconciliation::Ambiguous,
        }
    }
}

fn classify_fresh_namespace(
    record: &TransitionRecord,
    authenticated_pre: &NamespaceSnapshot,
    authenticated_pre_projection: &ProjectedNewStateCandidatePreserveNamespace,
    fresh_capture: Result<NamespaceSnapshot, CaptureError>,
) -> ClassifiedFreshNamespace {
    let baseline_projection = match ProjectedNewStateCandidatePreserveNamespace::capture(authenticated_pre, record) {
        Ok(projection) => projection,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if baseline_projection.layout() != NewStateCandidatePreserveLayout::StagedWithEmptyQuarantine
        || baseline_projection != *authenticated_pre_projection
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
    let fresh_projection = match ProjectedNewStateCandidatePreserveNamespace::capture(&fresh, record) {
        Ok(projection) => projection,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if authenticated_pre_projection
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

#[cfg(test)]
std::thread_local! {
    static BEFORE_RECONCILIATION_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_candidate_preserve_move_reconciliation_capture(
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
