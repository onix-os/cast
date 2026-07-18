//! Indivisible POST durability for one preserved archived candidate.

use std::{fs::File, io};

use crate::{Installation, transition_journal::TransitionRecord};

use super::{
    ArchivedCandidatePreserveCaptureError, ArchivedCandidatePreserveLayout,
    ProjectedArchivedCandidatePreserveNamespace, RetainedArchivedCandidatePreserveParents,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    CaptureError, NamespaceSnapshot, capture_snapshot,
};

/// Shared POST input from either applied reconciliation or preserved capture.
#[must_use = "archived candidate POST evidence must complete durability"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct PendingArchivedCandidatePreservePostMoveDurability
{
    parents: RetainedArchivedCandidatePreserveParents,
    authenticated_post: NamespaceSnapshot,
    authenticated_post_projection: ProjectedArchivedCandidatePreserveNamespace,
}

/// Opaque final proof that every archived-candidate POST barrier completed.
#[must_use = "durable archived candidate namespace evidence must remain sealed"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct DurableArchivedCandidatePreservePostMoveNamespace
{
    parents: RetainedArchivedCandidatePreserveParents,
    final_post: NamespaceSnapshot,
    final_post_projection: ProjectedArchivedCandidatePreserveNamespace,
}

impl PendingArchivedCandidatePreservePostMoveDurability {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn new(
        parents: RetainedArchivedCandidatePreserveParents,
        authenticated_post: NamespaceSnapshot,
        authenticated_post_projection: ProjectedArchivedCandidatePreserveNamespace,
    ) -> Self {
        Self {
            parents,
            authenticated_post,
            authenticated_post_projection,
        }
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn complete(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<DurableArchivedCandidatePreservePostMoveNamespace, ArchivedCandidatePreservePostMoveDurabilityError>
    {
        let Self {
            parents,
            authenticated_post,
            authenticated_post_projection,
        } = self;
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;

        run_before_candidate_sync();
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;
        require_boundary(DurabilityBoundary::CandidateSync)?;
        parents.candidate.sync_retained_tree().map_err(|source| {
            ArchivedCandidatePreservePostMoveDurabilityError::CandidateSync {
                path: parents.candidate.display_path().to_owned(),
                source,
            }
        })?;
        record_candidate_synced(parents.candidate.retained_directory());
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;

        run_before_staging_parent_sync();
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;
        require_boundary(DurabilityBoundary::StagingParentSync)?;
        parents.staging.sync_all().map_err(|source| {
            ArchivedCandidatePreservePostMoveDurabilityError::StagingParentSync {
                path: parents.staging_path.clone(),
                source,
            }
        })?;
        record_staging_parent_synced(&parents.staging);
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;

        run_before_target_parent_sync();
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;
        require_boundary(DurabilityBoundary::TargetParentSync)?;
        parents.target.sync_all().map_err(|source| {
            ArchivedCandidatePreservePostMoveDurabilityError::TargetParentSync {
                path: parents.target_path.clone(),
                source,
            }
        })?;
        record_target_parent_synced(&parents.target);
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;

        run_before_roots_parent_sync();
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;
        require_boundary(DurabilityBoundary::RootsParentSync)?;
        parents.roots.sync_all().map_err(|source| {
            ArchivedCandidatePreservePostMoveDurabilityError::RootsParentSync {
                path: parents.roots_path.clone(),
                source,
            }
        })?;
        record_roots_parent_synced(&parents.roots);
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;

        run_before_final_post_capture();
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;
        require_boundary(DurabilityBoundary::FinalPostCapture)?;
        let final_post = capture_snapshot(installation, record)?;
        final_post.revalidate_retained()?;
        if final_post.fingerprint() != authenticated_post.fingerprint() {
            return Err(ArchivedCandidatePreservePostMoveDurabilityError::FinalNamespaceChanged);
        }
        let final_post_projection = ProjectedArchivedCandidatePreserveNamespace::capture(&final_post, record)?;
        if final_post_projection.layout() != ArchivedCandidatePreserveLayout::Preserved
            || final_post_projection != authenticated_post_projection
        {
            return Err(ArchivedCandidatePreservePostMoveDurabilityError::FinalProjectionChanged);
        }
        require_exact_post(installation, record, &parents, &final_post, &final_post_projection)?;
        record_final_post_proven();
        Ok(DurableArchivedCandidatePreservePostMoveNamespace {
            parents,
            final_post,
            final_post_projection,
        })
    }
}

impl DurableArchivedCandidatePreservePostMoveNamespace {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn revalidate(
        &self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<(), ArchivedCandidatePreservePostMoveDurabilityError> {
        require_exact_post(
            installation,
            record,
            &self.parents,
            &self.final_post,
            &self.final_post_projection,
        )?;
        run_before_durable_post_revalidation_capture();
        let fresh = capture_snapshot(installation, record)?;
        fresh.revalidate_retained()?;
        if fresh.fingerprint() != self.final_post.fingerprint() {
            return Err(ArchivedCandidatePreservePostMoveDurabilityError::FinalNamespaceChanged);
        }
        let projection = ProjectedArchivedCandidatePreserveNamespace::capture(&fresh, record)?;
        if projection != self.final_post_projection || projection.layout() != ArchivedCandidatePreserveLayout::Preserved
        {
            return Err(ArchivedCandidatePreservePostMoveDurabilityError::FinalProjectionChanged);
        }
        require_exact_post(installation, record, &self.parents, &fresh, &projection)
    }
}

fn require_exact_post(
    installation: &Installation,
    record: &TransitionRecord,
    parents: &RetainedArchivedCandidatePreserveParents,
    snapshot: &NamespaceSnapshot,
    projection: &ProjectedArchivedCandidatePreserveNamespace,
) -> Result<(), ArchivedCandidatePreservePostMoveDurabilityError> {
    installation.revalidate_mutable_namespace()?;
    snapshot.revalidate_retained()?;
    if projection.layout() != ArchivedCandidatePreserveLayout::Preserved
        || ProjectedArchivedCandidatePreserveNamespace::capture(snapshot, record)? != *projection
    {
        return Err(ArchivedCandidatePreservePostMoveDurabilityError::PostEvidenceChanged);
    }
    parents.revalidate_value_identity(installation)?;
    snapshot.revalidate_retained()?;
    if ProjectedArchivedCandidatePreserveNamespace::capture(snapshot, record)? != *projection {
        return Err(ArchivedCandidatePreservePostMoveDurabilityError::PostEvidenceChanged);
    }
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurabilityBoundary {
    CandidateSync,
    StagingParentSync,
    TargetParentSync,
    RootsParentSync,
    FinalPostCapture,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ArchivedCandidatePreservePostMoveDurabilityFaultPoint {
    CandidateSync,
    StagingParentSync,
    TargetParentSync,
    RootsParentSync,
    FinalPostCapture,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ArchivedCandidatePreservePostMoveDurabilityEvent {
    CandidateSynced { device: u64, inode: u64 },
    StagingParentSynced { device: u64, inode: u64 },
    TargetParentSynced { device: u64, inode: u64 },
    RootsParentSynced { device: u64, inode: u64 },
    FinalPostProven,
}

fn require_boundary(boundary: DurabilityBoundary) -> Result<(), ArchivedCandidatePreservePostMoveDurabilityError> {
    if boundary_is_faulted(boundary) {
        #[cfg(test)]
        return Err(ArchivedCandidatePreservePostMoveDurabilityError::InjectedFault {
            point: fault_point(boundary),
        });
        #[cfg(not(test))]
        unreachable!("production archived POST durability boundaries cannot be faulted");
    }
    Ok(())
}

#[cfg(test)]
std::thread_local! {
    static POST_FAULT: std::cell::Cell<Option<ArchivedCandidatePreservePostMoveDurabilityFaultPoint>> =
        const { std::cell::Cell::new(None) };
    static POST_EVENTS: std::cell::RefCell<Vec<ArchivedCandidatePreservePostMoveDurabilityEvent>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_CANDIDATE_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_STAGING_PARENT_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_TARGET_PARENT_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_ROOTS_PARENT_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_FINAL_POST_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_DURABLE_POST_REVALIDATION_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_archived_candidate_preserve_post_move_durability_fault(
    point: ArchivedCandidatePreservePostMoveDurabilityFaultPoint,
) {
    POST_FAULT.with(|slot| assert!(slot.replace(Some(point)).is_none()));
}

#[cfg(test)]
pub(in crate::client) fn reset_archived_candidate_preserve_post_move_durability_events() {
    POST_EVENTS.with(|events| events.borrow_mut().clear());
    POST_FAULT.with(|slot| slot.set(None));
}

#[cfg(test)]
pub(in crate::client) fn take_archived_candidate_preserve_post_move_durability_events()
-> Vec<ArchivedCandidatePreservePostMoveDurabilityEvent> {
    POST_EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_post_candidate_sync(hook: impl FnOnce() + 'static) {
    BEFORE_CANDIDATE_SYNC.with(|slot| arm_hook(slot, hook));
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_post_staging_parent_sync(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_STAGING_PARENT_SYNC.with(|slot| arm_hook(slot, hook));
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_post_target_parent_sync(hook: impl FnOnce() + 'static) {
    BEFORE_TARGET_PARENT_SYNC.with(|slot| arm_hook(slot, hook));
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_post_roots_parent_sync(hook: impl FnOnce() + 'static) {
    BEFORE_ROOTS_PARENT_SYNC.with(|slot| arm_hook(slot, hook));
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_post_final_capture(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_POST_CAPTURE.with(|slot| arm_hook(slot, hook));
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_durable_post_revalidation_capture(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_DURABLE_POST_REVALIDATION_CAPTURE.with(|slot| arm_hook(slot, hook));
}

#[cfg(test)]
fn arm_hook(slot: &std::cell::RefCell<Option<Box<dyn FnOnce()>>>, hook: impl FnOnce() + 'static) {
    assert!(
        slot.borrow_mut().replace(Box::new(hook)).is_none(),
        "archived POST durability hook already armed"
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
fn run_before_final_post_capture() {
    BEFORE_FINAL_POST_CAPTURE.with(run_hook);
}

#[cfg(not(test))]
fn run_before_final_post_capture() {}

#[cfg(test)]
fn run_before_durable_post_revalidation_capture() {
    BEFORE_DURABLE_POST_REVALIDATION_CAPTURE.with(run_hook);
}

#[cfg(not(test))]
fn run_before_durable_post_revalidation_capture() {}

#[cfg(test)]
fn record_candidate_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ArchivedCandidatePreservePostMoveDurabilityEvent::CandidateSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_candidate_synced(_: &File) {}

#[cfg(test)]
fn record_staging_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ArchivedCandidatePreservePostMoveDurabilityEvent::StagingParentSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_staging_parent_synced(_: &File) {}

#[cfg(test)]
fn record_target_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ArchivedCandidatePreservePostMoveDurabilityEvent::TargetParentSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_target_parent_synced(_: &File) {}

#[cfg(test)]
fn record_roots_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ArchivedCandidatePreservePostMoveDurabilityEvent::RootsParentSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_roots_parent_synced(_: &File) {}

#[cfg(test)]
fn record_descriptor_event(
    file: &File,
    event: impl FnOnce(u64, u64) -> ArchivedCandidatePreservePostMoveDurabilityEvent,
) {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata().expect("inspect archived POST durability descriptor");
    POST_EVENTS.with(|events| events.borrow_mut().push(event(metadata.dev(), metadata.ino())));
}

fn record_final_post_proven() {
    #[cfg(test)]
    POST_EVENTS.with(|events| {
        events
            .borrow_mut()
            .push(ArchivedCandidatePreservePostMoveDurabilityEvent::FinalPostProven);
    });
}

#[cfg(test)]
fn boundary_is_faulted(boundary: DurabilityBoundary) -> bool {
    POST_FAULT.with(|slot| {
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
fn fault_point(boundary: DurabilityBoundary) -> ArchivedCandidatePreservePostMoveDurabilityFaultPoint {
    match boundary {
        DurabilityBoundary::CandidateSync => ArchivedCandidatePreservePostMoveDurabilityFaultPoint::CandidateSync,
        DurabilityBoundary::StagingParentSync => {
            ArchivedCandidatePreservePostMoveDurabilityFaultPoint::StagingParentSync
        }
        DurabilityBoundary::TargetParentSync => ArchivedCandidatePreservePostMoveDurabilityFaultPoint::TargetParentSync,
        DurabilityBoundary::RootsParentSync => ArchivedCandidatePreservePostMoveDurabilityFaultPoint::RootsParentSync,
        DurabilityBoundary::FinalPostCapture => ArchivedCandidatePreservePostMoveDurabilityFaultPoint::FinalPostCapture,
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ArchivedCandidatePreservePostMoveDurabilityError
{
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ArchivedCandidatePreserveCaptureError),
    #[error("revalidate mutable installation namespace around archived candidate POST durability")]
    Installation(#[from] crate::installation::Error),
    #[error("authenticated archived candidate evidence is no longer exact POST")]
    PostEvidenceChanged,
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
    #[error("sync retained `.cast/root` at `{}`", path.display())]
    RootsParentSync {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("final archived candidate POST namespace changed")]
    FinalNamespaceChanged,
    #[error("final archived candidate POST projection changed")]
    FinalProjectionChanged,
    #[cfg(test)]
    #[error("injected archived candidate POST durability fault at {point:?}")]
    InjectedFault {
        point: ArchivedCandidatePreservePostMoveDurabilityFaultPoint,
    },
}
