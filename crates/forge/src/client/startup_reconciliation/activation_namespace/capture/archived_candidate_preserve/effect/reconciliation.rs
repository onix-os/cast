//! Fresh semantic classification after one archived-candidate child move.

use crate::{Installation, transition_journal::TransitionRecord};

use super::PendingArchivedCandidatePreserveMoveReconciliation;
use crate::client::startup_reconciliation::activation_namespace::capture::{
    ArchivedCandidatePreserveLayout, CaptureError, NamespaceSnapshot, ProjectedArchivedCandidatePreserveNamespace,
    RetainedArchivedCandidatePreserveParents, capture_snapshot,
};

#[must_use = "an applied archived candidate move still requires post-move durability"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct AppliedArchivedCandidatePreserveMoveReconciliation
{
    pub(super) _parents: RetainedArchivedCandidatePreserveParents,
    pub(super) _fresh_post: NamespaceSnapshot,
    pub(super) _fresh_post_projection: ProjectedArchivedCandidatePreserveNamespace,
    pub(super) _raw_report: std::io::Result<()>,
}

#[must_use = "a reconciled archived candidate move must be handled"]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ArchivedCandidatePreserveMoveReconciliation {
    Applied(AppliedArchivedCandidatePreserveMoveReconciliation),
    NotApplied,
    Ambiguous,
}

enum ClassifiedFreshNamespace {
    Applied {
        snapshot: NamespaceSnapshot,
        projection: ProjectedArchivedCandidatePreserveNamespace,
    },
    NotApplied {
        snapshot: NamespaceSnapshot,
        projection: ProjectedArchivedCandidatePreserveNamespace,
    },
    Ambiguous,
}

impl PendingArchivedCandidatePreserveMoveReconciliation {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn reconcile(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> ArchivedCandidatePreserveMoveReconciliation {
        let Self {
            parents,
            authenticated_pre,
            authenticated_pre_projection,
            raw_report,
        } = self;
        run_before_reconciliation_capture();
        let classification = classify_fresh_namespace(
            record,
            &authenticated_pre,
            &authenticated_pre_projection,
            capture_snapshot(installation, record),
        );
        run_before_reconciliation_closing();
        match classification {
            ClassifiedFreshNamespace::Applied { snapshot, projection } => {
                if close_fresh_classification(
                    installation,
                    record,
                    &parents,
                    &snapshot,
                    &projection,
                    ArchivedCandidatePreserveLayout::Preserved,
                )
                .is_err()
                {
                    return ArchivedCandidatePreserveMoveReconciliation::Ambiguous;
                }
                ArchivedCandidatePreserveMoveReconciliation::Applied(
                    AppliedArchivedCandidatePreserveMoveReconciliation {
                        _parents: parents,
                        _fresh_post: snapshot,
                        _fresh_post_projection: projection,
                        _raw_report: raw_report,
                    },
                )
            }
            ClassifiedFreshNamespace::NotApplied { snapshot, projection } => {
                if close_fresh_classification(
                    installation,
                    record,
                    &parents,
                    &snapshot,
                    &projection,
                    ArchivedCandidatePreserveLayout::StagedWithCanonicalSlot,
                )
                .is_err()
                {
                    return ArchivedCandidatePreserveMoveReconciliation::Ambiguous;
                }
                let _raw_report = raw_report;
                ArchivedCandidatePreserveMoveReconciliation::NotApplied
            }
            ClassifiedFreshNamespace::Ambiguous => ArchivedCandidatePreserveMoveReconciliation::Ambiguous,
        }
    }
}

fn classify_fresh_namespace(
    record: &TransitionRecord,
    authenticated_pre: &NamespaceSnapshot,
    authenticated_pre_projection: &ProjectedArchivedCandidatePreserveNamespace,
    fresh_capture: Result<NamespaceSnapshot, CaptureError>,
) -> ClassifiedFreshNamespace {
    let baseline_projection = match ProjectedArchivedCandidatePreserveNamespace::capture(authenticated_pre, record) {
        Ok(projection) => projection,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if baseline_projection.layout() != ArchivedCandidatePreserveLayout::StagedWithCanonicalSlot
        || baseline_projection != *authenticated_pre_projection
    {
        return ClassifiedFreshNamespace::Ambiguous;
    }
    let fresh = match fresh_capture {
        Ok(fresh) => fresh,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    let fresh_projection = match ProjectedArchivedCandidatePreserveNamespace::capture(&fresh, record) {
        Ok(projection) => projection,
        Err(_) => return ClassifiedFreshNamespace::Ambiguous,
    };
    if fresh.fingerprint() == authenticated_pre.fingerprint() {
        return if fresh_projection.layout() == ArchivedCandidatePreserveLayout::StagedWithCanonicalSlot
            && fresh_projection == *authenticated_pre_projection
        {
            ClassifiedFreshNamespace::NotApplied {
                snapshot: fresh,
                projection: fresh_projection,
            }
        } else {
            ClassifiedFreshNamespace::Ambiguous
        };
    }
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

fn close_fresh_classification(
    installation: &Installation,
    record: &TransitionRecord,
    parents: &RetainedArchivedCandidatePreserveParents,
    snapshot: &NamespaceSnapshot,
    projection: &ProjectedArchivedCandidatePreserveNamespace,
    expected_layout: ArchivedCandidatePreserveLayout,
) -> Result<(), ()> {
    parents.revalidate_value_identity(installation).map_err(|_| ())?;
    snapshot.revalidate_retained().map_err(|_| ())?;
    let closing_projection = ProjectedArchivedCandidatePreserveNamespace::capture(snapshot, record).map_err(|_| ())?;
    if closing_projection.layout() != expected_layout || closing_projection != *projection {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_RECONCILIATION_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_RECONCILIATION_CLOSING: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_move_reconciliation_capture(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_RECONCILIATION_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_move_reconciliation_closing(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_RECONCILIATION_CLOSING.with(|slot| {
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

#[cfg(test)]
fn run_before_reconciliation_closing() {
    BEFORE_RECONCILIATION_CLOSING.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_reconciliation_capture() {}

#[cfg(not(test))]
fn run_before_reconciliation_closing() {}
