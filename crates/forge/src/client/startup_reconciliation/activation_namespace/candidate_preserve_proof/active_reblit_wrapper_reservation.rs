//! Final absent-reservation PRE proof and one-attempt reconciliation.
//!
//! Preparation ends before the attempt so the enclosing authority can repeat
//! its binding-first journal, database, plan, and installation checks. The
//! reconciled result is fieldless and forces a caller boundary.

use std::ffi::CString;

use crate::{Installation, transition_journal::TransitionRecord};

use super::{
    UsrRollbackCandidatePreserveNamespaceError, UsrRollbackCandidatePreserveTopology, require_matching_fingerprints,
    require_topology,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    ActiveReblitWrapperReservationLayout, ActiveReblitWrapperReservationReconciliation, NamespaceSnapshot,
    ProjectedActiveReblitWrapperReservationNamespace, UsrRollbackActiveReblitWrapperReservationNamespaceEvidence,
    capture_snapshot,
};

/// Fieldless namespace result of consuming one exact reservation capability.
#[must_use = "a consumed ActiveReblit reservation namespace result must be handled"]
pub(in crate::client::startup_reconciliation) enum UsrRollbackActiveReblitWrapperReservationNamespaceReconciliation {
    RestartRequired,
    NotApplied,
    Ambiguous,
}

/// Opaque final PRE authority for exactly one reservation attempt.
#[must_use = "prepared ActiveReblit reservation namespace authority must be consumed"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitWrapperReservationPreparedNamespace {
    baseline: NamespaceSnapshot,
    projection: ProjectedActiveReblitWrapperReservationNamespace,
    wrapper_name: CString,
}

impl UsrRollbackActiveReblitWrapperReservationNamespaceEvidence {
    /// Prove one final exact absent-reservation PRE without performing an effect.
    pub(in crate::client::startup_reconciliation) fn prepare_wrapper_reservation(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackActiveReblitWrapperReservationPreparedNamespace, UsrRollbackCandidatePreserveNamespaceError>
    {
        let Self { baseline, projection } = self;
        if projection.layout() != ActiveReblitWrapperReservationLayout::Absent {
            return Err(UsrRollbackCandidatePreserveNamespaceError::TopologyMismatch);
        }

        installation.revalidate_mutable_namespace()?;
        baseline.revalidate_retained()?;
        require_projection(record, &baseline, &projection)?;
        require_topology(
            record,
            &baseline,
            UsrRollbackCandidatePreserveTopology::ActiveReblitStagedWithoutReservation,
        )?;

        run_before_final_pre_capture();
        let fresh = capture_snapshot(installation, record)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&baseline, &fresh)?;
        require_projection(record, &fresh, &projection)?;
        require_topology(
            record,
            &fresh,
            UsrRollbackCandidatePreserveTopology::ActiveReblitStagedWithoutReservation,
        )?;
        installation.revalidate_mutable_namespace()?;

        let wrapper_name = UsrRollbackActiveReblitWrapperReservationNamespaceEvidence::reservation_name(record)?;
        Ok(UsrRollbackActiveReblitWrapperReservationPreparedNamespace {
            baseline: fresh,
            projection,
            wrapper_name,
        })
    }
}

impl UsrRollbackActiveReblitWrapperReservationPreparedNamespace {
    /// Consume exact final PRE authority through one attempt and fresh capture.
    pub(in crate::client::startup_reconciliation) fn reconcile_wrapper_reservation(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> UsrRollbackActiveReblitWrapperReservationNamespaceReconciliation {
        let Self {
            baseline,
            projection,
            wrapper_name,
        } = self;
        match baseline
            .attempt_active_reblit_wrapper_reservation_once(&wrapper_name, projection)
            .reconcile(installation, record)
        {
            ActiveReblitWrapperReservationReconciliation::RestartRequired => {
                UsrRollbackActiveReblitWrapperReservationNamespaceReconciliation::RestartRequired
            }
            ActiveReblitWrapperReservationReconciliation::NotApplied => {
                UsrRollbackActiveReblitWrapperReservationNamespaceReconciliation::NotApplied
            }
            ActiveReblitWrapperReservationReconciliation::Ambiguous => {
                UsrRollbackActiveReblitWrapperReservationNamespaceReconciliation::Ambiguous
            }
        }
    }
}

fn require_projection(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: &ProjectedActiveReblitWrapperReservationNamespace,
) -> Result<(), UsrRollbackCandidatePreserveNamespaceError> {
    if ProjectedActiveReblitWrapperReservationNamespace::capture(snapshot, record)? == *expected {
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
pub(in crate::client) fn arm_before_usr_rollback_active_reblit_wrapper_reservation_final_pre_capture(
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
