//! Final absent-target PRE proof and one-attempt semantic reconciliation.
//!
//! Namespace preparation ends before the attempt so the enclosing authority
//! can repeat its binding-first journal, database, plan, and installation
//! checks. Every reconciled result is fieldless and forces a caller boundary.

use std::ffi::CString;

use crate::{Installation, transition_journal::TransitionRecord};

use super::{
    UsrRollbackCandidatePreserveNamespaceError, UsrRollbackCandidatePreserveTopology, require_matching_fingerprints,
    require_topology,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    NamespaceSnapshot, NewStateCandidatePreserveCaptureError, NewStateTargetCreateLayout,
    NewStateTargetCreateReconciliation, ProjectedNewStateTargetCreateNamespace,
    UsrRollbackNewStateTargetCreateNamespaceEvidence, capture_snapshot,
};

/// Fieldless namespace result of consuming one exact absent-target capability.
#[must_use = "a consumed NewState target-creation namespace result must be handled"]
pub(in crate::client::startup_reconciliation) enum UsrRollbackNewStateTargetCreateNamespaceReconciliation {
    RestartRequired,
    NotApplied,
    Ambiguous,
}

/// Opaque final PRE authority for exactly one target-creation attempt.
#[must_use = "prepared NewState target-creation namespace authority must be consumed"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackNewStateTargetCreatePreparedNamespace {
    baseline: NamespaceSnapshot,
    projection: ProjectedNewStateTargetCreateNamespace,
    target_name: CString,
}

impl UsrRollbackNewStateTargetCreateNamespaceEvidence {
    /// Prove one final exact absent-target PRE without performing an effect.
    pub(in crate::client::startup_reconciliation) fn prepare_target_creation(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackNewStateTargetCreatePreparedNamespace, UsrRollbackCandidatePreserveNamespaceError> {
        let Self { baseline, projection } = self;
        if projection.layout() != NewStateTargetCreateLayout::Absent {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }

        installation.revalidate_mutable_namespace()?;
        baseline.revalidate_retained()?;
        require_projection(record, &baseline, &projection)?;
        require_topology(record, &baseline, UsrRollbackCandidatePreserveTopology::NewStateStaged)?;

        run_before_final_pre_capture();
        let fresh = capture_snapshot(installation, record)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&baseline, &fresh)?;
        require_projection(record, &fresh, &projection)?;
        require_topology(record, &fresh, UsrRollbackCandidatePreserveTopology::NewStateStaged)?;
        installation.revalidate_mutable_namespace()?;

        let target_name = CString::new(record.quarantine_name.as_str())
            .map_err(|_| NewStateCandidatePreserveCaptureError::WrongTargetName)?;
        Ok(UsrRollbackNewStateTargetCreatePreparedNamespace {
            baseline: fresh,
            projection,
            target_name,
        })
    }
}

impl UsrRollbackNewStateTargetCreatePreparedNamespace {
    /// Consume exact final PRE authority through one attempt and fresh capture.
    pub(in crate::client::startup_reconciliation) fn reconcile_target_creation(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> UsrRollbackNewStateTargetCreateNamespaceReconciliation {
        let Self {
            baseline,
            projection,
            target_name,
        } = self;
        match baseline
            .attempt_new_state_target_create_once(&target_name, projection)
            .reconcile(installation, record)
        {
            NewStateTargetCreateReconciliation::RestartRequired => {
                UsrRollbackNewStateTargetCreateNamespaceReconciliation::RestartRequired
            }
            NewStateTargetCreateReconciliation::NotApplied => {
                UsrRollbackNewStateTargetCreateNamespaceReconciliation::NotApplied
            }
            NewStateTargetCreateReconciliation::Ambiguous => {
                UsrRollbackNewStateTargetCreateNamespaceReconciliation::Ambiguous
            }
        }
    }
}

fn require_projection(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: &ProjectedNewStateTargetCreateNamespace,
) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
    if ProjectedNewStateTargetCreateNamespace::capture(snapshot, record)? == *expected {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveNamespaceError::NamespaceChanged)
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_PRE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_new_state_target_create_final_pre_capture(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_FINAL_PRE_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_final_pre_capture() {
    BEFORE_FINAL_PRE_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_final_pre_capture() {}
