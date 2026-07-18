//! Ordered exact-PRE durability before one archived-candidate child move.

use std::{fs::File, io};

use crate::{Installation, linux_fs::renameat2_noreplace_once, transition_journal::TransitionRecord};

use super::{
    ArchivedCandidatePreserveCaptureError, ArchivedCandidatePreserveLayout,
    ProjectedArchivedCandidatePreserveNamespace, RetainedArchivedCandidatePreserveParents,
    effect::PendingArchivedCandidatePreserveMoveReconciliation,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    CaptureError, NamespaceSnapshot, capture_snapshot,
};

/// Opaque source/destination-durable capability for one final move attempt.
#[must_use = "target-durable archived candidate PRE must be consumed"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct TargetDurableArchivedCandidatePreservePre {
    parents: RetainedArchivedCandidatePreserveParents,
    final_pre: NamespaceSnapshot,
    final_pre_projection: ProjectedArchivedCandidatePreserveNamespace,
}

impl RetainedArchivedCandidatePreserveParents {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn complete_target_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
        authenticated_pre: NamespaceSnapshot,
        authenticated_pre_projection: ProjectedArchivedCandidatePreserveNamespace,
    ) -> Result<TargetDurableArchivedCandidatePreservePre, ArchivedCandidatePreserveTargetDurabilityError> {
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;

        run_before_candidate_sync();
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;
        require_boundary(DurabilityBoundary::CandidateSync)?;
        self.candidate.sync_retained_tree().map_err(|source| {
            ArchivedCandidatePreserveTargetDurabilityError::CandidateSync {
                path: self.candidate.display_path().to_owned(),
                source,
            }
        })?;
        record_candidate_synced(self.candidate.retained_directory());
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;

        run_before_staging_parent_sync();
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;
        require_boundary(DurabilityBoundary::StagingParentSync)?;
        self.staging.sync_all().map_err(|source| {
            ArchivedCandidatePreserveTargetDurabilityError::StagingParentSync {
                path: self.staging_path.clone(),
                source,
            }
        })?;
        record_staging_parent_synced(&self.staging);
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;

        run_before_target_parent_sync();
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;
        require_boundary(DurabilityBoundary::TargetParentSync)?;
        self.target.sync_all().map_err(
            |source| ArchivedCandidatePreserveTargetDurabilityError::TargetParentSync {
                path: self.target_path.clone(),
                source,
            },
        )?;
        record_target_parent_synced(&self.target);
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;

        run_before_roots_parent_sync();
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;
        require_boundary(DurabilityBoundary::RootsParentSync)?;
        self.roots.sync_all().map_err(
            |source| ArchivedCandidatePreserveTargetDurabilityError::RootsParentSync {
                path: self.roots_path.clone(),
                source,
            },
        )?;
        record_roots_parent_synced(&self.roots);
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
            return Err(ArchivedCandidatePreserveTargetDurabilityError::FinalNamespaceChanged);
        }
        let final_pre_projection = ProjectedArchivedCandidatePreserveNamespace::capture(&final_pre, record)?;
        if final_pre_projection.layout() != ArchivedCandidatePreserveLayout::StagedWithCanonicalSlot
            || final_pre_projection != authenticated_pre_projection
        {
            return Err(ArchivedCandidatePreserveTargetDurabilityError::FinalProjectionChanged);
        }
        require_exact_pre(installation, record, &self, &final_pre, &final_pre_projection)?;
        record_final_pre_proven();
        Ok(TargetDurableArchivedCandidatePreservePre {
            parents: self,
            final_pre,
            final_pre_projection,
        })
    }
}

impl TargetDurableArchivedCandidatePreservePre {
    /// Perform one final exact PRE revalidation and exactly one no-replace move.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn attempt_move_once(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<PendingArchivedCandidatePreserveMoveReconciliation, ArchivedCandidatePreserveTargetDurabilityError>
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
        Ok(PendingArchivedCandidatePreserveMoveReconciliation::new(
            parents,
            final_pre,
            final_pre_projection,
            raw_report,
        ))
    }
}

fn require_exact_pre(
    installation: &Installation,
    record: &TransitionRecord,
    parents: &RetainedArchivedCandidatePreserveParents,
    snapshot: &NamespaceSnapshot,
    projection: &ProjectedArchivedCandidatePreserveNamespace,
) -> Result<(), ArchivedCandidatePreserveTargetDurabilityError> {
    installation.revalidate_mutable_namespace()?;
    snapshot.revalidate_retained()?;
    if projection.layout() != ArchivedCandidatePreserveLayout::StagedWithCanonicalSlot
        || ProjectedArchivedCandidatePreserveNamespace::capture(snapshot, record)? != *projection
    {
        return Err(ArchivedCandidatePreserveTargetDurabilityError::PreEvidenceChanged);
    }
    parents.revalidate_value_identity(installation)?;
    snapshot.revalidate_retained()?;
    if ProjectedArchivedCandidatePreserveNamespace::capture(snapshot, record)? != *projection {
        return Err(ArchivedCandidatePreserveTargetDurabilityError::PreEvidenceChanged);
    }
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

/// Private raw boundary: diagnostic status never classifies the namespace.
fn attempt_raw_move_once(staging: &File, target: &File) -> io::Result<()> {
    #[cfg(test)]
    let injected = begin_move_attempt();
    #[cfg(not(test))]
    let _injected = begin_move_attempt();
    #[cfg(test)]
    let apply = !matches!(
        injected,
        Some(
            ArchivedCandidatePreserveMoveFault::ErrorWithoutApply
                | ArchivedCandidatePreserveMoveFault::SuccessWithoutApply
        )
    );
    #[cfg(not(test))]
    let apply = true;
    let kernel_result = apply.then(|| renameat2_noreplace_once(staging, c"usr", target, c"usr"));
    #[cfg(test)]
    let result = match (injected, kernel_result) {
        (Some(ArchivedCandidatePreserveMoveFault::ErrorWithoutApply), None) => {
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        }
        (Some(ArchivedCandidatePreserveMoveFault::SuccessWithoutApply), None) => Ok(()),
        (Some(ArchivedCandidatePreserveMoveFault::ErrorAfterApply), Some(Ok(()))) => {
            Err(io::Error::from_raw_os_error(nix::libc::EINTR))
        }
        (_, Some(result)) => result,
        _ => unreachable!("archived candidate move fault matrix is complete"),
    };
    #[cfg(not(test))]
    let result = kernel_result.expect("production always invokes the one-shot archived candidate move");
    result
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ArchivedCandidatePreserveMoveFault {
    ErrorWithoutApply,
    SuccessWithoutApply,
    ErrorAfterApply,
}

#[cfg(test)]
std::thread_local! {
    static MOVE_FAULT: std::cell::Cell<Option<ArchivedCandidatePreserveMoveFault>> =
        const { std::cell::Cell::new(None) };
    static MOVE_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(in crate::client) fn arm_archived_candidate_preserve_move_fault(fault: ArchivedCandidatePreserveMoveFault) {
    MOVE_FAULT.with(|slot| assert!(slot.replace(Some(fault)).is_none()));
}

#[cfg(test)]
pub(in crate::client) fn reset_archived_candidate_preserve_move_attempt_count() {
    MOVE_ATTEMPTS.with(|count| count.set(0));
    MOVE_FAULT.with(|slot| slot.set(None));
}

#[cfg(test)]
pub(in crate::client) fn archived_candidate_preserve_move_attempt_count() -> usize {
    MOVE_ATTEMPTS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn begin_move_attempt() -> Option<ArchivedCandidatePreserveMoveFault> {
    MOVE_ATTEMPTS.with(|count| count.set(count.get().saturating_add(1)));
    MOVE_FAULT.with(std::cell::Cell::take)
}

#[cfg(not(test))]
fn begin_move_attempt() -> Option<std::convert::Infallible> {
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurabilityBoundary {
    CandidateSync,
    StagingParentSync,
    TargetParentSync,
    RootsParentSync,
    FinalPreCapture,
}

fn require_boundary(boundary: DurabilityBoundary) -> Result<(), ArchivedCandidatePreserveTargetDurabilityError> {
    if boundary_is_faulted(boundary) {
        #[cfg(test)]
        return Err(ArchivedCandidatePreserveTargetDurabilityError::InjectedFault {
            point: fault_point(boundary),
        });
        #[cfg(not(test))]
        unreachable!("production archived PRE durability boundaries cannot be faulted");
    }
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ArchivedCandidatePreserveTargetDurabilityFaultPoint {
    CandidateSync,
    StagingParentSync,
    TargetParentSync,
    RootsParentSync,
    FinalPreCapture,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ArchivedCandidatePreserveTargetDurabilityEvent {
    CandidateSynced { device: u64, inode: u64 },
    StagingParentSynced { device: u64, inode: u64 },
    TargetParentSynced { device: u64, inode: u64 },
    RootsParentSynced { device: u64, inode: u64 },
    FinalPreProven,
}

#[cfg(test)]
fn fault_point(boundary: DurabilityBoundary) -> ArchivedCandidatePreserveTargetDurabilityFaultPoint {
    match boundary {
        DurabilityBoundary::CandidateSync => ArchivedCandidatePreserveTargetDurabilityFaultPoint::CandidateSync,
        DurabilityBoundary::StagingParentSync => ArchivedCandidatePreserveTargetDurabilityFaultPoint::StagingParentSync,
        DurabilityBoundary::TargetParentSync => ArchivedCandidatePreserveTargetDurabilityFaultPoint::TargetParentSync,
        DurabilityBoundary::RootsParentSync => ArchivedCandidatePreserveTargetDurabilityFaultPoint::RootsParentSync,
        DurabilityBoundary::FinalPreCapture => ArchivedCandidatePreserveTargetDurabilityFaultPoint::FinalPreCapture,
    }
}

#[cfg(test)]
std::thread_local! {
    static TARGET_DURABILITY_FAULT:
        std::cell::Cell<Option<ArchivedCandidatePreserveTargetDurabilityFaultPoint>> = const { std::cell::Cell::new(None) };
    static TARGET_DURABILITY_EVENTS:
        std::cell::RefCell<Vec<ArchivedCandidatePreserveTargetDurabilityEvent>> = const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_CANDIDATE_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_STAGING_PARENT_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_TARGET_PARENT_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_ROOTS_PARENT_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_FINAL_PRE_CAPTURE:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_PRE_MOVE_REVALIDATION:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_archived_candidate_preserve_target_durability_fault(
    point: ArchivedCandidatePreserveTargetDurabilityFaultPoint,
) {
    TARGET_DURABILITY_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(point)).is_none(),
            "archived PRE durability fault already armed"
        );
    });
}

#[cfg(test)]
pub(in crate::client) fn reset_archived_candidate_preserve_target_durability_events() {
    TARGET_DURABILITY_EVENTS.with(|events| events.borrow_mut().clear());
    TARGET_DURABILITY_FAULT.with(|slot| slot.set(None));
}

#[cfg(test)]
pub(in crate::client) fn take_archived_candidate_preserve_target_durability_events()
-> Vec<ArchivedCandidatePreserveTargetDurabilityEvent> {
    TARGET_DURABILITY_EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_pre_candidate_sync(hook: impl FnOnce() + 'static) {
    BEFORE_CANDIDATE_SYNC.with(|slot| arm_hook(slot, hook));
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_pre_staging_parent_sync(hook: impl FnOnce() + 'static) {
    BEFORE_STAGING_PARENT_SYNC.with(|slot| arm_hook(slot, hook));
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_pre_target_parent_sync(hook: impl FnOnce() + 'static) {
    BEFORE_TARGET_PARENT_SYNC.with(|slot| arm_hook(slot, hook));
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_pre_roots_parent_sync(hook: impl FnOnce() + 'static) {
    BEFORE_ROOTS_PARENT_SYNC.with(|slot| arm_hook(slot, hook));
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_pre_final_capture(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_PRE_CAPTURE.with(|slot| arm_hook(slot, hook));
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_pre_move_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_PRE_MOVE_REVALIDATION.with(|slot| arm_hook(slot, hook));
}

#[cfg(test)]
fn arm_hook(slot: &std::cell::RefCell<Option<Box<dyn FnOnce()>>>, hook: impl FnOnce() + 'static) {
    assert!(
        slot.borrow_mut().replace(Box::new(hook)).is_none(),
        "archived PRE durability hook already armed"
    );
}

#[cfg(test)]
fn run_hook(slot: &std::cell::RefCell<Option<Box<dyn FnOnce()>>>) {
    if let Some(hook) = slot.borrow_mut().take() {
        hook();
    }
}

#[cfg(test)]
fn run_before_candidate_sync() {
    BEFORE_CANDIDATE_SYNC.with(run_hook);
}

#[cfg(not(test))]
fn run_before_candidate_sync() {}

#[cfg(test)]
fn run_before_staging_parent_sync() {
    BEFORE_STAGING_PARENT_SYNC.with(run_hook);
}

#[cfg(not(test))]
fn run_before_staging_parent_sync() {}

#[cfg(test)]
fn run_before_target_parent_sync() {
    BEFORE_TARGET_PARENT_SYNC.with(run_hook);
}

#[cfg(not(test))]
fn run_before_target_parent_sync() {}

#[cfg(test)]
fn run_before_roots_parent_sync() {
    BEFORE_ROOTS_PARENT_SYNC.with(run_hook);
}

#[cfg(not(test))]
fn run_before_roots_parent_sync() {}

#[cfg(test)]
fn run_before_final_pre_capture() {
    BEFORE_FINAL_PRE_CAPTURE.with(run_hook);
}

#[cfg(not(test))]
fn run_before_final_pre_capture() {}

#[cfg(test)]
fn run_before_pre_move_revalidation() {
    BEFORE_PRE_MOVE_REVALIDATION.with(run_hook);
}

#[cfg(not(test))]
fn run_before_pre_move_revalidation() {}

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
fn boundary_is_faulted(_: DurabilityBoundary) -> bool {
    false
}

#[cfg(test)]
fn record_candidate_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ArchivedCandidatePreserveTargetDurabilityEvent::CandidateSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_candidate_synced(_: &File) {}

#[cfg(test)]
fn record_staging_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ArchivedCandidatePreserveTargetDurabilityEvent::StagingParentSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_staging_parent_synced(_: &File) {}

#[cfg(test)]
fn record_target_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ArchivedCandidatePreserveTargetDurabilityEvent::TargetParentSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_target_parent_synced(_: &File) {}

#[cfg(test)]
fn record_roots_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ArchivedCandidatePreserveTargetDurabilityEvent::RootsParentSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_roots_parent_synced(_: &File) {}

#[cfg(test)]
fn record_descriptor_event(
    file: &File,
    event: impl FnOnce(u64, u64) -> ArchivedCandidatePreserveTargetDurabilityEvent,
) {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata().expect("inspect archived PRE durability descriptor");
    TARGET_DURABILITY_EVENTS.with(|events| events.borrow_mut().push(event(metadata.dev(), metadata.ino())));
}

fn record_final_pre_proven() {
    #[cfg(test)]
    TARGET_DURABILITY_EVENTS.with(|events| {
        events
            .borrow_mut()
            .push(ArchivedCandidatePreserveTargetDurabilityEvent::FinalPreProven);
    });
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ArchivedCandidatePreserveTargetDurabilityError
{
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ArchivedCandidatePreserveCaptureError),
    #[error("revalidate mutable installation namespace around archived candidate PRE durability")]
    Installation(#[from] crate::installation::Error),
    #[error("authenticated archived candidate evidence is no longer exact PRE")]
    PreEvidenceChanged,
    #[error("sync archived candidate tree at `{}`", path.display())]
    CandidateSync {
        path: std::path::PathBuf,
        #[source]
        source: crate::tree_marker::TreeMarkerError,
    },
    #[error("sync retained staging wrapper at `{}`", path.display())]
    StagingParentSync {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync retained canonical candidate wrapper at `{}`", path.display())]
    TargetParentSync {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync retained `.cast/root` parent at `{}`", path.display())]
    RootsParentSync {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("final archived candidate PRE namespace changed")]
    FinalNamespaceChanged,
    #[error("final archived candidate PRE projection changed")]
    FinalProjectionChanged,
    #[cfg(test)]
    #[error("injected archived candidate PRE durability fault at {point:?}")]
    InjectedFault {
        point: ArchivedCandidatePreserveTargetDurabilityFaultPoint,
    },
}
