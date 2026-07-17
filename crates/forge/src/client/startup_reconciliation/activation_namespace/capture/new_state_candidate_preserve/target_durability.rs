//! Ordered target durability before one NewState candidate-preservation move.
//!
//! Every exact move lease independently syncs the already-canonical target
//! and its retained quarantine parent after the candidate tree barrier. The
//! raw move parents remain sealed until both barriers, complete retained and
//! public-name revalidation, and one final fresh exact PRE capture succeed.

use std::{fs::File, io};

use crate::{Installation, linux_fs::renameat2_noreplace_once, transition_journal::TransitionRecord};

use super::{
    NewStateCandidatePreserveCaptureError, NewStateCandidatePreserveLayout,
    ProjectedNewStateCandidatePreserveNamespace, RetainedNewStateCandidatePreserveParents,
    effect::PendingNewStateCandidatePreserveMoveReconciliation,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    CaptureError, NamespaceSnapshot, capture_snapshot,
};

/// Opaque proof that the exact destination and quarantine parent are durable
/// and that one final fresh capture is still the authenticated move PRE.
///
/// The raw parent descriptors have no accessor. Only consuming this value can
/// cross the final pre-move revalidation and reach the one-shot rename.
#[must_use = "target-durable NewState candidate-preservation PRE must be consumed"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct TargetDurableNewStateCandidatePreservePre {
    parents: RetainedNewStateCandidatePreserveParents,
    final_pre: NamespaceSnapshot,
    final_pre_projection: ProjectedNewStateCandidatePreserveNamespace,
}

impl RetainedNewStateCandidatePreserveParents {
    /// Consume the raw move parents through the target and quarantine-parent
    /// barriers, then retain only one final fresh exact PRE capability.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn complete_target_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
        authenticated_pre: NamespaceSnapshot,
        authenticated_pre_projection: ProjectedNewStateCandidatePreserveNamespace,
    ) -> Result<TargetDurableNewStateCandidatePreservePre, NewStateCandidatePreserveTargetDurabilityError> {
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;

        run_before_target_sync();
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;
        require_boundary(DurabilityBoundary::TargetSync)?;
        self.target
            .sync_all()
            .map_err(|source| NewStateCandidatePreserveTargetDurabilityError::TargetSync {
                path: self.target_path.clone(),
                source,
            })?;
        record_target_synced(&self.target);
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;

        run_before_quarantine_parent_sync();
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;
        require_boundary(DurabilityBoundary::QuarantineParentSync)?;
        self.quarantine.sync_all().map_err(|source| {
            NewStateCandidatePreserveTargetDurabilityError::QuarantineParentSync {
                path: self.quarantine_path.clone(),
                source,
            }
        })?;
        record_quarantine_parent_synced(&self.quarantine);
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;

        run_before_final_pre_capture();
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;
        require_boundary(DurabilityBoundary::FinalPreCapture)?;
        let final_pre = capture_snapshot(installation, record)?;
        final_pre.revalidate_retained()?;
        if final_pre.fingerprint() != authenticated_pre.fingerprint() {
            return Err(NewStateCandidatePreserveTargetDurabilityError::FinalNamespaceChanged);
        }
        let final_pre_projection = ProjectedNewStateCandidatePreserveNamespace::capture(&final_pre, record)?;
        if final_pre_projection.layout() != NewStateCandidatePreserveLayout::StagedWithEmptyQuarantine
            || final_pre_projection != authenticated_pre_projection
        {
            return Err(NewStateCandidatePreserveTargetDurabilityError::FinalProjectionChanged);
        }
        require_exact_pre(installation, record, &self, &final_pre, &final_pre_projection)?;
        record_final_pre_proven();

        Ok(TargetDurableNewStateCandidatePreservePre {
            parents: self,
            final_pre,
            final_pre_projection,
        })
    }
}

impl TargetDurableNewStateCandidatePreservePre {
    /// Revalidate the complete retained and named PRE after the enclosing
    /// authority's final non-namespace evidence sandwich, then make exactly
    /// one no-replace move attempt.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn attempt_move_once(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<PendingNewStateCandidatePreserveMoveReconciliation, NewStateCandidatePreserveTargetDurabilityError>
    {
        run_before_pre_move_revalidation();
        require_exact_pre(
            installation,
            record,
            &self.parents,
            &self.final_pre,
            &self.final_pre_projection,
        )?;

        let Self {
            parents,
            final_pre,
            final_pre_projection,
        } = self;
        let raw_report = attempt_raw_move_once(&parents.staging, &parents.target);
        Ok(PendingNewStateCandidatePreserveMoveReconciliation::new(
            parents,
            final_pre,
            final_pre_projection,
            raw_report,
        ))
    }
}

/// Invoke the raw move only inside the target-durable typestate module.
///
/// This helper is deliberately private: sibling modules cannot bypass the
/// durable PRE constructor or its final pre-move revalidation.
fn attempt_raw_move_once(staging: &File, target: &File) -> io::Result<()> {
    #[cfg(test)]
    let injected = begin_move_attempt();
    #[cfg(not(test))]
    let _injected = begin_move_attempt();
    #[cfg(test)]
    let apply = !matches!(
        injected,
        Some(
            NewStateCandidatePreserveMoveFault::ErrorWithoutApply
                | NewStateCandidatePreserveMoveFault::SuccessWithoutApply
        )
    );
    #[cfg(not(test))]
    let apply = true;

    let kernel_result = apply.then(|| renameat2_noreplace_once(staging, c"usr", target, c"usr"));
    #[cfg(test)]
    let result = match (injected, kernel_result) {
        (Some(NewStateCandidatePreserveMoveFault::ErrorWithoutApply), None) => {
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        }
        (Some(NewStateCandidatePreserveMoveFault::SuccessWithoutApply), None) => Ok(()),
        (Some(NewStateCandidatePreserveMoveFault::ErrorAfterApply), Some(Ok(()))) => {
            Err(io::Error::from_raw_os_error(nix::libc::EINTR))
        }
        (_, Some(result)) => result,
        _ => unreachable!("NewState candidate-move fault injection has a complete result matrix"),
    };
    #[cfg(not(test))]
    let result = kernel_result.expect("production always invokes the one-shot candidate move");
    result
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum NewStateCandidatePreserveMoveFault {
    ErrorWithoutApply,
    SuccessWithoutApply,
    ErrorAfterApply,
}

#[cfg(test)]
std::thread_local! {
    static MOVE_FAULT: std::cell::Cell<Option<NewStateCandidatePreserveMoveFault>> = const { std::cell::Cell::new(None) };
    static MOVE_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(in crate::client) fn arm_new_state_candidate_preserve_move_fault(fault: NewStateCandidatePreserveMoveFault) {
    MOVE_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(fault)).is_none(),
            "candidate-move fault already armed"
        );
    });
}

#[cfg(test)]
pub(in crate::client) fn reset_new_state_candidate_preserve_move_attempt_count() {
    MOVE_ATTEMPTS.with(|count| count.set(0));
    MOVE_FAULT.with(|slot| slot.set(None));
}

#[cfg(test)]
pub(in crate::client) fn new_state_candidate_preserve_move_attempt_count() -> usize {
    MOVE_ATTEMPTS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn begin_move_attempt() -> Option<NewStateCandidatePreserveMoveFault> {
    MOVE_ATTEMPTS.with(|count| count.set(count.get().saturating_add(1)));
    MOVE_FAULT.with(std::cell::Cell::take)
}

#[cfg(not(test))]
fn begin_move_attempt() -> Option<std::convert::Infallible> {
    None
}

fn require_exact_pre(
    installation: &Installation,
    record: &TransitionRecord,
    parents: &RetainedNewStateCandidatePreserveParents,
    snapshot: &NamespaceSnapshot,
    projection: &ProjectedNewStateCandidatePreserveNamespace,
) -> Result<(), NewStateCandidatePreserveTargetDurabilityError> {
    installation.revalidate_mutable_namespace()?;
    snapshot.revalidate_retained()?;
    if projection.layout() != NewStateCandidatePreserveLayout::StagedWithEmptyQuarantine
        || ProjectedNewStateCandidatePreserveNamespace::capture(snapshot, record)? != *projection
    {
        return Err(NewStateCandidatePreserveTargetDurabilityError::PreEvidenceChanged);
    }
    parents.revalidate_value_identity(installation)?;
    snapshot.revalidate_retained()?;
    if projection.layout() != NewStateCandidatePreserveLayout::StagedWithEmptyQuarantine
        || ProjectedNewStateCandidatePreserveNamespace::capture(snapshot, record)? != *projection
    {
        return Err(NewStateCandidatePreserveTargetDurabilityError::PreEvidenceChanged);
    }
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurabilityBoundary {
    TargetSync,
    QuarantineParentSync,
    FinalPreCapture,
}

fn require_boundary(boundary: DurabilityBoundary) -> Result<(), NewStateCandidatePreserveTargetDurabilityError> {
    if boundary_is_faulted(boundary) {
        #[cfg(test)]
        return Err(NewStateCandidatePreserveTargetDurabilityError::InjectedFault {
            point: fault_point(boundary),
        });
        #[cfg(not(test))]
        unreachable!("production candidate-move target-durability boundaries cannot be faulted");
    }
    Ok(())
}

#[cfg(test)]
fn boundary_is_faulted(boundary: DurabilityBoundary) -> bool {
    TARGET_DURABILITY_FAULT.with(|slot| {
        if slot.get() == Some(fault_point(boundary)) {
            slot.set(None);
            true
        } else {
            false
        }
    })
}

#[cfg(not(test))]
fn boundary_is_faulted(_boundary: DurabilityBoundary) -> bool {
    false
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum NewStateCandidatePreserveTargetDurabilityError
{
    #[error("capture or revalidate exact NewState candidate-move target-durability evidence")]
    Capture(#[from] CaptureError),
    #[error("project exact NewState candidate-move target-durability evidence")]
    Projection(#[from] NewStateCandidatePreserveCaptureError),
    #[error("revalidate the retained mutable installation namespace around candidate-move target durability")]
    Installation(#[from] crate::installation::Error),
    #[error("authenticated candidate-move target-durability evidence is no longer exact PRE")]
    PreEvidenceChanged,
    #[error("sync retained NewState candidate-move target at `{}`", path.display())]
    TargetSync {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync retained NewState candidate-move quarantine parent at `{}`", path.display())]
    QuarantineParentSync {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the final fresh candidate-move target-durability namespace changed")]
    FinalNamespaceChanged,
    #[error("the final fresh candidate-move target-durability projection changed")]
    FinalProjectionChanged,
    #[cfg(test)]
    #[error("injected NewState candidate-move target-durability fault at {point:?}")]
    InjectedFault {
        point: NewStateCandidatePreserveTargetDurabilityFaultPoint,
    },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum NewStateCandidatePreserveTargetDurabilityFaultPoint {
    TargetSync,
    QuarantineParentSync,
    FinalPreCapture,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum NewStateCandidatePreserveTargetDurabilityEvent {
    TargetSynced { device: u64, inode: u64 },
    QuarantineParentSynced { device: u64, inode: u64 },
    FinalPreProven,
}

#[cfg(test)]
fn fault_point(boundary: DurabilityBoundary) -> NewStateCandidatePreserveTargetDurabilityFaultPoint {
    match boundary {
        DurabilityBoundary::TargetSync => NewStateCandidatePreserveTargetDurabilityFaultPoint::TargetSync,
        DurabilityBoundary::QuarantineParentSync => {
            NewStateCandidatePreserveTargetDurabilityFaultPoint::QuarantineParentSync
        }
        DurabilityBoundary::FinalPreCapture => NewStateCandidatePreserveTargetDurabilityFaultPoint::FinalPreCapture,
    }
}

#[cfg(test)]
std::thread_local! {
    static TARGET_DURABILITY_FAULT:
        std::cell::Cell<Option<NewStateCandidatePreserveTargetDurabilityFaultPoint>> = const { std::cell::Cell::new(None) };
    static TARGET_DURABILITY_EVENTS:
        std::cell::RefCell<Vec<NewStateCandidatePreserveTargetDurabilityEvent>> = const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_TARGET_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_QUARANTINE_PARENT_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_TARGET_DURABILITY_FINAL_PRE_CAPTURE:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_EFFECT_FINAL_PRE_CAPTURE:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_TARGET_DURABILITY_PRE_MOVE_REVALIDATION:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_new_state_candidate_preserve_target_durability_fault(
    point: NewStateCandidatePreserveTargetDurabilityFaultPoint,
) {
    TARGET_DURABILITY_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(point)).is_none(),
            "candidate-move target-durability fault already armed"
        );
    });
}

#[cfg(test)]
pub(in crate::client) fn reset_new_state_candidate_preserve_target_durability_events() {
    TARGET_DURABILITY_EVENTS.with(|events| events.borrow_mut().clear());
}

#[cfg(test)]
pub(in crate::client) fn take_new_state_candidate_preserve_target_durability_events()
-> Vec<NewStateCandidatePreserveTargetDurabilityEvent> {
    TARGET_DURABILITY_EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_candidate_preserve_target_sync(hook: impl FnOnce() + 'static) {
    BEFORE_TARGET_SYNC.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_candidate_preserve_quarantine_parent_sync(hook: impl FnOnce() + 'static) {
    BEFORE_QUARANTINE_PARENT_SYNC.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_candidate_preserve_target_durability_final_pre_capture(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_TARGET_DURABILITY_FINAL_PRE_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_EFFECT_FINAL_PRE_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_candidate_preserve_target_durability_pre_move_revalidation(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_TARGET_DURABILITY_PRE_MOVE_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_target_sync() {
    BEFORE_TARGET_SYNC.with(run_hook);
}

#[cfg(not(test))]
fn run_before_target_sync() {}

#[cfg(test)]
fn run_before_quarantine_parent_sync() {
    BEFORE_QUARANTINE_PARENT_SYNC.with(run_hook);
}

#[cfg(not(test))]
fn run_before_quarantine_parent_sync() {}

#[cfg(test)]
fn run_before_final_pre_capture() {
    BEFORE_EFFECT_FINAL_PRE_CAPTURE.with(run_hook);
    BEFORE_TARGET_DURABILITY_FINAL_PRE_CAPTURE.with(run_hook);
}

#[cfg(not(test))]
fn run_before_final_pre_capture() {}

#[cfg(test)]
fn run_before_pre_move_revalidation() {
    BEFORE_TARGET_DURABILITY_PRE_MOVE_REVALIDATION.with(run_hook);
}

#[cfg(not(test))]
fn run_before_pre_move_revalidation() {}

#[cfg(test)]
fn run_hook(slot: &std::cell::RefCell<Option<Box<dyn FnOnce()>>>) {
    if let Some(hook) = slot.borrow_mut().take() {
        hook();
    }
}

#[cfg(test)]
fn record_target_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        NewStateCandidatePreserveTargetDurabilityEvent::TargetSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_target_synced(_file: &File) {}

#[cfg(test)]
fn record_quarantine_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        NewStateCandidatePreserveTargetDurabilityEvent::QuarantineParentSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_quarantine_parent_synced(_file: &File) {}

#[cfg(test)]
fn record_descriptor_event(
    file: &File,
    event: impl FnOnce(u64, u64) -> NewStateCandidatePreserveTargetDurabilityEvent,
) {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file
        .metadata()
        .expect("inspect synced NewState candidate-move target-durability descriptor");
    TARGET_DURABILITY_EVENTS.with(|events| {
        events.borrow_mut().push(event(metadata.dev(), metadata.ino()));
    });
}

fn record_final_pre_proven() {
    #[cfg(test)]
    TARGET_DURABILITY_EVENTS.with(|events| {
        events
            .borrow_mut()
            .push(NewStateCandidatePreserveTargetDurabilityEvent::FinalPreProven);
    });
}
