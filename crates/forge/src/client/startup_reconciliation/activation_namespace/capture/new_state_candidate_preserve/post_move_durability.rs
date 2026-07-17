//! Indivisible post-move durability for one preserved NewState candidate.
//!
//! Applied and already-preserved evidence converge here before any durability
//! work begins. The capability is consumed through the exact candidate tree,
//! staging wrapper, target wrapper, quarantine parent, and final fresh POST
//! proof. A failure at any boundary drops every retained descriptor.

use std::{fs::File, io};

use crate::{Installation, transition_journal::TransitionRecord, tree_marker::TreeMarkerError};

use super::{
    NewStateCandidatePreserveCaptureError, NewStateCandidatePreserveLayout,
    ProjectedNewStateCandidatePreserveNamespace, RetainedNewStateCandidatePreserveParents,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    CaptureError, NamespaceSnapshot, capture_snapshot,
};

/// Common exact-POST capability accepted from either a freshly applied move
/// or independently admitted already-preserved NewState evidence.
#[must_use = "post-move NewState candidate-preservation evidence must complete durability"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct PendingNewStateCandidatePreservePostMoveDurability
{
    parents: RetainedNewStateCandidatePreserveParents,
    authenticated_post: NamespaceSnapshot,
    authenticated_post_projection: ProjectedNewStateCandidatePreserveNamespace,
}

/// Opaque proof that every ordered post-move barrier and the final exact POST
/// capture completed as one consuming suffix.
#[must_use = "durable NewState candidate-preservation namespace evidence must remain sealed"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct DurableNewStateCandidatePreservePostMoveNamespace
{
    _parents: RetainedNewStateCandidatePreserveParents,
    _final_post: NamespaceSnapshot,
    _final_post_projection: ProjectedNewStateCandidatePreserveNamespace,
}

impl PendingNewStateCandidatePreservePostMoveDurability {
    pub(super) fn new(
        parents: RetainedNewStateCandidatePreserveParents,
        authenticated_post: NamespaceSnapshot,
        authenticated_post_projection: ProjectedNewStateCandidatePreserveNamespace,
    ) -> Self {
        Self {
            parents,
            authenticated_post,
            authenticated_post_projection,
        }
    }

    /// Construct the common suffix input only from exact preserved NewState
    /// admission. The staged constructor remains separately PRE-restricted.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture_preserved(
        snapshot: NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, NewStateCandidatePreservePostMoveDurabilityError> {
        let projection = ProjectedNewStateCandidatePreserveNamespace::capture(&snapshot, record)?;
        if projection.layout() != NewStateCandidatePreserveLayout::Preserved {
            return Err(NewStateCandidatePreservePostMoveDurabilityError::PostEvidenceChanged);
        }
        let parents = RetainedNewStateCandidatePreserveParents::capture_preserved(&snapshot, record)?;
        Ok(Self::new(parents, snapshot, projection))
    }

    /// Consume all retained POST capability through the fixed durability
    /// order. No intermediate result can escape an error.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn complete(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<DurableNewStateCandidatePreservePostMoveNamespace, NewStateCandidatePreservePostMoveDurabilityError>
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
            NewStateCandidatePreservePostMoveDurabilityError::CandidateSync {
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
            NewStateCandidatePreservePostMoveDurabilityError::StagingParentSync {
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
            NewStateCandidatePreservePostMoveDurabilityError::TargetParentSync {
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

        run_before_quarantine_parent_sync();
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;
        require_boundary(DurabilityBoundary::QuarantineParentSync)?;
        parents.quarantine.sync_all().map_err(|source| {
            NewStateCandidatePreservePostMoveDurabilityError::QuarantineParentSync {
                path: parents.quarantine_path.clone(),
                source,
            }
        })?;
        record_quarantine_parent_synced(&parents.quarantine);
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
            return Err(NewStateCandidatePreservePostMoveDurabilityError::FinalNamespaceChanged);
        }
        let final_post_projection = ProjectedNewStateCandidatePreserveNamespace::capture(&final_post, record)?;
        if final_post_projection.layout() != NewStateCandidatePreserveLayout::Preserved
            || final_post_projection != authenticated_post_projection
        {
            return Err(NewStateCandidatePreservePostMoveDurabilityError::FinalProjectionChanged);
        }
        require_exact_post(installation, record, &parents, &final_post, &final_post_projection)?;
        record_final_post_proven();

        Ok(DurableNewStateCandidatePreservePostMoveNamespace {
            _parents: parents,
            _final_post: final_post,
            _final_post_projection: final_post_projection,
        })
    }
}

fn require_exact_post(
    installation: &Installation,
    record: &TransitionRecord,
    parents: &RetainedNewStateCandidatePreserveParents,
    snapshot: &NamespaceSnapshot,
    projection: &ProjectedNewStateCandidatePreserveNamespace,
) -> Result<(), NewStateCandidatePreservePostMoveDurabilityError> {
    installation.revalidate_mutable_namespace()?;
    snapshot.revalidate_retained()?;
    if projection.layout() != NewStateCandidatePreserveLayout::Preserved
        || ProjectedNewStateCandidatePreserveNamespace::capture(snapshot, record)? != *projection
    {
        return Err(NewStateCandidatePreservePostMoveDurabilityError::PostEvidenceChanged);
    }
    parents.revalidate_value_identity(installation)?;
    snapshot.revalidate_retained()?;
    if projection.layout() != NewStateCandidatePreserveLayout::Preserved
        || ProjectedNewStateCandidatePreserveNamespace::capture(snapshot, record)? != *projection
    {
        return Err(NewStateCandidatePreservePostMoveDurabilityError::PostEvidenceChanged);
    }
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurabilityBoundary {
    CandidateSync,
    StagingParentSync,
    TargetParentSync,
    QuarantineParentSync,
    FinalPostCapture,
}

fn require_boundary(boundary: DurabilityBoundary) -> Result<(), NewStateCandidatePreservePostMoveDurabilityError> {
    if boundary_is_faulted(boundary) {
        #[cfg(test)]
        return Err(NewStateCandidatePreservePostMoveDurabilityError::InjectedFault {
            point: fault_point(boundary),
        });
        #[cfg(not(test))]
        unreachable!("production post-move candidate durability boundaries cannot be faulted");
    }
    Ok(())
}

#[cfg(test)]
fn boundary_is_faulted(boundary: DurabilityBoundary) -> bool {
    POST_MOVE_DURABILITY_FAULT.with(|slot| {
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
pub(in crate::client::startup_reconciliation::activation_namespace) enum NewStateCandidatePreservePostMoveDurabilityError
{
    #[error("capture or revalidate exact NewState candidate-preservation POST evidence")]
    Capture(#[from] CaptureError),
    #[error("project exact NewState candidate-preservation POST evidence")]
    Projection(#[from] NewStateCandidatePreserveCaptureError),
    #[error("revalidate the mutable installation namespace around post-move candidate durability")]
    Installation(#[from] crate::installation::Error),
    #[error("authenticated NewState candidate-preservation evidence is no longer exact POST")]
    PostEvidenceChanged,
    #[error("sync retained NewState candidate tree at `{}`", path.display())]
    CandidateSync {
        path: std::path::PathBuf,
        #[source]
        source: TreeMarkerError,
    },
    #[error("sync retained NewState staging wrapper at `{}`", path.display())]
    StagingParentSync {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync retained NewState target wrapper at `{}`", path.display())]
    TargetParentSync {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync retained NewState quarantine parent at `{}`", path.display())]
    QuarantineParentSync {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the final fresh post-move durability namespace changed")]
    FinalNamespaceChanged,
    #[error("the final fresh post-move durability projection changed")]
    FinalProjectionChanged,
    #[cfg(test)]
    #[error("injected NewState candidate-preservation post-move durability fault at {point:?}")]
    InjectedFault {
        point: NewStateCandidatePreservePostMoveDurabilityFaultPoint,
    },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum NewStateCandidatePreservePostMoveDurabilityFaultPoint {
    CandidateSync,
    StagingParentSync,
    TargetParentSync,
    QuarantineParentSync,
    FinalPostCapture,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum NewStateCandidatePreservePostMoveDurabilityEvent {
    CandidateSynced { device: u64, inode: u64 },
    StagingParentSynced { device: u64, inode: u64 },
    TargetParentSynced { device: u64, inode: u64 },
    QuarantineParentSynced { device: u64, inode: u64 },
    FinalPostProven,
}

#[cfg(test)]
fn fault_point(boundary: DurabilityBoundary) -> NewStateCandidatePreservePostMoveDurabilityFaultPoint {
    match boundary {
        DurabilityBoundary::CandidateSync => NewStateCandidatePreservePostMoveDurabilityFaultPoint::CandidateSync,
        DurabilityBoundary::StagingParentSync => {
            NewStateCandidatePreservePostMoveDurabilityFaultPoint::StagingParentSync
        }
        DurabilityBoundary::TargetParentSync => NewStateCandidatePreservePostMoveDurabilityFaultPoint::TargetParentSync,
        DurabilityBoundary::QuarantineParentSync => {
            NewStateCandidatePreservePostMoveDurabilityFaultPoint::QuarantineParentSync
        }
        DurabilityBoundary::FinalPostCapture => NewStateCandidatePreservePostMoveDurabilityFaultPoint::FinalPostCapture,
    }
}

#[cfg(test)]
std::thread_local! {
    static POST_MOVE_DURABILITY_FAULT:
        std::cell::Cell<Option<NewStateCandidatePreservePostMoveDurabilityFaultPoint>> = const { std::cell::Cell::new(None) };
    static POST_MOVE_DURABILITY_EVENTS:
        std::cell::RefCell<Vec<NewStateCandidatePreservePostMoveDurabilityEvent>> = const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_POST_MOVE_CANDIDATE_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_POST_MOVE_STAGING_PARENT_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_POST_MOVE_TARGET_PARENT_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_POST_MOVE_QUARANTINE_PARENT_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_POST_MOVE_FINAL_POST_CAPTURE:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_new_state_candidate_preserve_post_move_durability_fault(
    point: NewStateCandidatePreservePostMoveDurabilityFaultPoint,
) {
    POST_MOVE_DURABILITY_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(point)).is_none(),
            "post-move candidate durability fault already armed"
        );
    });
}

#[cfg(test)]
pub(in crate::client) fn reset_new_state_candidate_preserve_post_move_durability_events() {
    POST_MOVE_DURABILITY_EVENTS.with(|events| events.borrow_mut().clear());
    POST_MOVE_DURABILITY_FAULT.with(|slot| slot.set(None));
}

#[cfg(test)]
pub(in crate::client) fn take_new_state_candidate_preserve_post_move_durability_events()
-> Vec<NewStateCandidatePreservePostMoveDurabilityEvent> {
    POST_MOVE_DURABILITY_EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_candidate_preserve_post_move_candidate_sync(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_POST_MOVE_CANDIDATE_SYNC.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_candidate_preserve_post_move_staging_parent_sync(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_POST_MOVE_STAGING_PARENT_SYNC.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_candidate_preserve_post_move_target_parent_sync(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_POST_MOVE_TARGET_PARENT_SYNC.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_candidate_preserve_post_move_quarantine_parent_sync(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_POST_MOVE_QUARANTINE_PARENT_SYNC.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_candidate_preserve_post_move_final_post_capture(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_POST_MOVE_FINAL_POST_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_candidate_sync() {
    BEFORE_POST_MOVE_CANDIDATE_SYNC.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_candidate_sync() {}

#[cfg(test)]
fn run_before_staging_parent_sync() {
    BEFORE_POST_MOVE_STAGING_PARENT_SYNC.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_staging_parent_sync() {}

#[cfg(test)]
fn run_before_target_parent_sync() {
    BEFORE_POST_MOVE_TARGET_PARENT_SYNC.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_target_parent_sync() {}

#[cfg(test)]
fn run_before_quarantine_parent_sync() {
    BEFORE_POST_MOVE_QUARANTINE_PARENT_SYNC.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_quarantine_parent_sync() {}

#[cfg(test)]
fn run_before_final_post_capture() {
    BEFORE_POST_MOVE_FINAL_POST_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_final_post_capture() {}

#[cfg(test)]
fn record_candidate_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        NewStateCandidatePreservePostMoveDurabilityEvent::CandidateSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_candidate_synced(_file: &File) {}

#[cfg(test)]
fn record_staging_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        NewStateCandidatePreservePostMoveDurabilityEvent::StagingParentSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_staging_parent_synced(_file: &File) {}

#[cfg(test)]
fn record_target_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        NewStateCandidatePreservePostMoveDurabilityEvent::TargetParentSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_target_parent_synced(_file: &File) {}

#[cfg(test)]
fn record_quarantine_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        NewStateCandidatePreservePostMoveDurabilityEvent::QuarantineParentSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_quarantine_parent_synced(_file: &File) {}

#[cfg(test)]
fn record_descriptor_event(
    file: &File,
    event: impl FnOnce(u64, u64) -> NewStateCandidatePreservePostMoveDurabilityEvent,
) {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file
        .metadata()
        .expect("inspect synced candidate-preservation descriptor");
    POST_MOVE_DURABILITY_EVENTS.with(|events| {
        events.borrow_mut().push(event(metadata.dev(), metadata.ino()));
    });
}

fn record_final_post_proven() {
    #[cfg(test)]
    POST_MOVE_DURABILITY_EVENTS.with(|events| {
        events
            .borrow_mut()
            .push(NewStateCandidatePreservePostMoveDurabilityEvent::FinalPostProven);
    });
}
