//! Fixed descriptor-ordered durability suffix for completed cleanup.

use std::{fs::File, io, path::PathBuf};

#[cfg(test)]
use std::os::unix::fs::MetadataExt as _;

use crate::{Installation, transition_journal::TransitionRecord, tree_marker::TreeMarkerError};

use super::{
    ActiveReblitCommitCleanupEffectError, ActiveReblitCommitCleanupLayout,
    ProjectedActiveReblitCommitCleanupNamespace, RetainedActiveReblitCommitCleanupParents,
    os_name,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    CaptureError, NamespaceSnapshot, capture_snapshot,
};

/// Exact Finish capability accepted from either a classified Apply exchange
/// or independent Finish admission.
#[must_use = "ActiveReblit cleanup Finish evidence must complete durability"]
pub(in crate::client::startup_reconciliation) struct PendingActiveReblitCommitCleanupDurability
{
    parents: RetainedActiveReblitCommitCleanupParents,
    authenticated_finish: NamespaceSnapshot,
    authenticated_projection: ProjectedActiveReblitCommitCleanupNamespace,
}

/// Opaque proof that the fixed five-barrier suffix and fresh Finish proof
/// completed. No intermediate capability survives a failure.
#[must_use = "durable ActiveReblit cleanup namespace evidence must remain sealed"]
pub(in crate::client::startup_reconciliation) struct DurableActiveReblitCommitCleanupNamespace
{
    parents: RetainedActiveReblitCommitCleanupParents,
    final_finish: NamespaceSnapshot,
    final_projection: ProjectedActiveReblitCommitCleanupNamespace,
}

impl PendingActiveReblitCommitCleanupDurability {
    pub(super) fn new(
        parents: RetainedActiveReblitCommitCleanupParents,
        authenticated_finish: NamespaceSnapshot,
        authenticated_projection: ProjectedActiveReblitCommitCleanupNamespace,
    ) -> Self {
        Self {
            parents,
            authenticated_finish,
            authenticated_projection,
        }
    }

    /// Consume exactly the required suffix: previous tree, quarantined
    /// original wrapper, empty staging replacement, roots parent, quarantine
    /// parent, then a fresh exact Finish proof.
    pub(in crate::client::startup_reconciliation) fn complete(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<DurableActiveReblitCommitCleanupNamespace, ActiveReblitCommitCleanupDurabilityError> {
        let Self {
            parents,
            authenticated_finish,
            authenticated_projection,
        } = self;
        require_exact_finish(
            installation,
            record,
            &parents,
            &authenticated_finish,
            &authenticated_projection,
        )?;

        require_boundary(DurabilityBoundary::PreviousTreeSync)?;
        parents.previous.sync_retained_tree().map_err(|source| {
            ActiveReblitCommitCleanupDurabilityError::PreviousTreeSync {
                path: post_previous_path(&parents),
                source,
            }
        })?;
        record_event(
            parents.previous.retained_directory(),
            ActiveReblitCommitCleanupDurabilityEventKind::PreviousTree,
        );
        require_exact_finish(
            installation,
            record,
            &parents,
            &authenticated_finish,
            &authenticated_projection,
        )?;

        require_boundary(DurabilityBoundary::PreviousWrapperSync)?;
        parents.previous_wrapper.sync_all().map_err(|source| {
            ActiveReblitCommitCleanupDurabilityError::PreviousWrapperSync {
                path: post_previous_wrapper_path(&parents),
                source,
            }
        })?;
        record_event(
            &parents.previous_wrapper,
            ActiveReblitCommitCleanupDurabilityEventKind::PreviousWrapper,
        );
        require_exact_finish(
            installation,
            record,
            &parents,
            &authenticated_finish,
            &authenticated_projection,
        )?;

        require_boundary(DurabilityBoundary::ReplacementWrapperSync)?;
        parents.replacement_wrapper.sync_all().map_err(|source| {
            ActiveReblitCommitCleanupDurabilityError::ReplacementWrapperSync {
                path: parents.roots_path.join("staging"),
                source,
            }
        })?;
        record_event(
            &parents.replacement_wrapper,
            ActiveReblitCommitCleanupDurabilityEventKind::ReplacementWrapper,
        );
        require_exact_finish(
            installation,
            record,
            &parents,
            &authenticated_finish,
            &authenticated_projection,
        )?;

        require_boundary(DurabilityBoundary::RootsParentSync)?;
        parents.roots.sync_all().map_err(|source| {
            ActiveReblitCommitCleanupDurabilityError::RootsParentSync {
                path: parents.roots_path.clone(),
                source,
            }
        })?;
        record_event(&parents.roots, ActiveReblitCommitCleanupDurabilityEventKind::RootsParent);
        require_exact_finish(
            installation,
            record,
            &parents,
            &authenticated_finish,
            &authenticated_projection,
        )?;

        require_boundary(DurabilityBoundary::QuarantineParentSync)?;
        parents.quarantine.sync_all().map_err(|source| {
            ActiveReblitCommitCleanupDurabilityError::QuarantineParentSync {
                path: parents.quarantine_path.clone(),
                source,
            }
        })?;
        record_event(
            &parents.quarantine,
            ActiveReblitCommitCleanupDurabilityEventKind::QuarantineParent,
        );
        require_exact_finish(
            installation,
            record,
            &parents,
            &authenticated_finish,
            &authenticated_projection,
        )?;

        require_boundary(DurabilityBoundary::FinalFinishCapture)?;
        let final_finish = capture_snapshot(installation, record)?;
        final_finish.revalidate_retained()?;
        if final_finish.fingerprint() != authenticated_finish.fingerprint() {
            return Err(ActiveReblitCommitCleanupDurabilityError::FinalNamespaceChanged);
        }
        let final_projection = ProjectedActiveReblitCommitCleanupNamespace::capture(&final_finish, record)?;
        if final_projection.layout != ActiveReblitCommitCleanupLayout::Finish
            || final_projection != authenticated_projection
        {
            return Err(ActiveReblitCommitCleanupDurabilityError::FinalProjectionChanged);
        }
        require_exact_finish(
            installation,
            record,
            &parents,
            &final_finish,
            &final_projection,
        )?;
        record_final_finish();
        Ok(DurableActiveReblitCommitCleanupNamespace {
            parents,
            final_finish,
            final_projection,
        })
    }
}

impl DurableActiveReblitCommitCleanupNamespace {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<(), ActiveReblitCommitCleanupDurabilityError> {
        require_exact_finish(
            installation,
            record,
            &self.parents,
            &self.final_finish,
            &self.final_projection,
        )?;
        let fresh = capture_snapshot(installation, record)?;
        fresh.revalidate_retained()?;
        if fresh.fingerprint() != self.final_finish.fingerprint() {
            return Err(ActiveReblitCommitCleanupDurabilityError::FinalNamespaceChanged);
        }
        let projection = ProjectedActiveReblitCommitCleanupNamespace::capture(&fresh, record)?;
        if projection != self.final_projection {
            return Err(ActiveReblitCommitCleanupDurabilityError::FinalProjectionChanged);
        }
        require_exact_finish(installation, record, &self.parents, &fresh, &projection)
    }
}

impl super::RetainedActiveReblitCommitCleanupNamespace {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn into_finish_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<PendingActiveReblitCommitCleanupDurability, ActiveReblitCommitCleanupEffectError> {
        self.revalidate(record)?;
        let parents = RetainedActiveReblitCommitCleanupParents::capture(
            &self.snapshot,
            record,
            ActiveReblitCommitCleanupLayout::Finish,
        )?;
        installation.revalidate_mutable_namespace()?;
        self.snapshot.revalidate_retained()?;
        if self.projection.layout != ActiveReblitCommitCleanupLayout::Finish
            || ProjectedActiveReblitCommitCleanupNamespace::capture(&self.snapshot, record)?
                != self.projection
        {
            return Err(ActiveReblitCommitCleanupEffectError::FinishEvidenceChanged);
        }
        parents.revalidate_layout(installation, ActiveReblitCommitCleanupLayout::Finish)?;
        let fresh = capture_snapshot(installation, record)?;
        fresh.revalidate_retained()?;
        if fresh.fingerprint() != self.snapshot.fingerprint() {
            return Err(ActiveReblitCommitCleanupEffectError::FinalNamespaceChanged);
        }
        let projection = ProjectedActiveReblitCommitCleanupNamespace::capture(&fresh, record)?;
        if projection != self.projection {
            return Err(ActiveReblitCommitCleanupEffectError::FinalProjectionChanged);
        }
        parents.revalidate_layout(installation, ActiveReblitCommitCleanupLayout::Finish)?;
        Ok(PendingActiveReblitCommitCleanupDurability::new(
            parents,
            fresh,
            projection,
        ))
    }
}

fn require_exact_finish(
    installation: &Installation,
    record: &TransitionRecord,
    parents: &RetainedActiveReblitCommitCleanupParents,
    snapshot: &NamespaceSnapshot,
    projection: &ProjectedActiveReblitCommitCleanupNamespace,
) -> Result<(), ActiveReblitCommitCleanupDurabilityError> {
    installation.revalidate_mutable_namespace()?;
    snapshot.revalidate_retained()?;
    if projection.layout != ActiveReblitCommitCleanupLayout::Finish
        || ProjectedActiveReblitCommitCleanupNamespace::capture(snapshot, record)? != *projection
    {
        return Err(ActiveReblitCommitCleanupDurabilityError::FinishEvidenceChanged);
    }
    parents.revalidate_layout(installation, ActiveReblitCommitCleanupLayout::Finish)?;
    snapshot.revalidate_retained()?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn post_previous_wrapper_path(parents: &RetainedActiveReblitCommitCleanupParents) -> PathBuf {
    parents.quarantine_path.join(os_name(parents.target_name.as_bytes()))
}

fn post_previous_path(parents: &RetainedActiveReblitCommitCleanupParents) -> PathBuf {
    post_previous_wrapper_path(parents).join("usr")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurabilityBoundary {
    PreviousTreeSync,
    PreviousWrapperSync,
    ReplacementWrapperSync,
    RootsParentSync,
    QuarantineParentSync,
    FinalFinishCapture,
}

#[cfg(test)]
fn require_boundary(boundary: DurabilityBoundary) -> Result<(), ActiveReblitCommitCleanupDurabilityError> {
    DURABILITY_FAULT.with(|slot| {
        if slot.get() == Some(fault_point(boundary)) {
            slot.set(None);
            Err(ActiveReblitCommitCleanupDurabilityError::InjectedFault {
                point: fault_point(boundary),
            })
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
fn require_boundary(_boundary: DurabilityBoundary) -> Result<(), ActiveReblitCommitCleanupDurabilityError> {
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum ActiveReblitCommitCleanupDurabilityError {
    #[error(transparent)]
    Effect(#[from] ActiveReblitCommitCleanupEffectError),
    #[error(transparent)]
    Projection(#[from] super::ActiveReblitCommitCleanupCaptureError),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("revalidate installation around ActiveReblit cleanup durability")]
    Installation(#[from] crate::installation::Error),
    #[error("authenticated ActiveReblit cleanup evidence is no longer exact Finish")]
    FinishEvidenceChanged,
    #[error("sync corrupt previous tree at `{}`", path.display())]
    PreviousTreeSync {
        path: PathBuf,
        #[source]
        source: TreeMarkerError,
    },
    #[error("sync quarantined original wrapper at `{}`", path.display())]
    PreviousWrapperSync {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync empty staging replacement at `{}`", path.display())]
    ReplacementWrapperSync {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync retained roots parent at `{}`", path.display())]
    RootsParentSync {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync retained quarantine parent at `{}`", path.display())]
    QuarantineParentSync {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the final fresh ActiveReblit Finish namespace changed")]
    FinalNamespaceChanged,
    #[error("the final fresh ActiveReblit Finish projection changed")]
    FinalProjectionChanged,
    #[cfg(test)]
    #[error("injected ActiveReblit cleanup durability fault at {point:?}")]
    InjectedFault {
        point: ActiveReblitCommitCleanupDurabilityFaultPoint,
    },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitCommitCleanupDurabilityFaultPoint {
    PreviousTreeSync,
    PreviousWrapperSync,
    ReplacementWrapperSync,
    RootsParentSync,
    QuarantineParentSync,
    FinalFinishCapture,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitCommitCleanupDurabilityEvent {
    PreviousTreeSynced { device: u64, inode: u64 },
    PreviousWrapperSynced { device: u64, inode: u64 },
    ReplacementWrapperSynced { device: u64, inode: u64 },
    RootsParentSynced { device: u64, inode: u64 },
    QuarantineParentSynced { device: u64, inode: u64 },
    FinalFinishProven,
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum ActiveReblitCommitCleanupDurabilityEventKind {
    PreviousTree,
    PreviousWrapper,
    ReplacementWrapper,
    RootsParent,
    QuarantineParent,
}

#[cfg(not(test))]
enum ActiveReblitCommitCleanupDurabilityEventKind {
    PreviousTree,
    PreviousWrapper,
    ReplacementWrapper,
    RootsParent,
    QuarantineParent,
}

#[cfg(test)]
fn fault_point(boundary: DurabilityBoundary) -> ActiveReblitCommitCleanupDurabilityFaultPoint {
    match boundary {
        DurabilityBoundary::PreviousTreeSync => ActiveReblitCommitCleanupDurabilityFaultPoint::PreviousTreeSync,
        DurabilityBoundary::PreviousWrapperSync => {
            ActiveReblitCommitCleanupDurabilityFaultPoint::PreviousWrapperSync
        }
        DurabilityBoundary::ReplacementWrapperSync => {
            ActiveReblitCommitCleanupDurabilityFaultPoint::ReplacementWrapperSync
        }
        DurabilityBoundary::RootsParentSync => ActiveReblitCommitCleanupDurabilityFaultPoint::RootsParentSync,
        DurabilityBoundary::QuarantineParentSync => {
            ActiveReblitCommitCleanupDurabilityFaultPoint::QuarantineParentSync
        }
        DurabilityBoundary::FinalFinishCapture => {
            ActiveReblitCommitCleanupDurabilityFaultPoint::FinalFinishCapture
        }
    }
}

#[cfg(test)]
std::thread_local! {
    static DURABILITY_FAULT: std::cell::Cell<Option<ActiveReblitCommitCleanupDurabilityFaultPoint>> =
        const { std::cell::Cell::new(None) };
    static DURABILITY_EVENTS: std::cell::RefCell<Vec<ActiveReblitCommitCleanupDurabilityEvent>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(in crate::client) fn arm_active_reblit_commit_cleanup_durability_fault(
    point: ActiveReblitCommitCleanupDurabilityFaultPoint,
) {
    DURABILITY_FAULT.with(|slot| {
        assert!(slot.replace(Some(point)).is_none(), "cleanup durability fault already armed");
    });
}

#[cfg(test)]
pub(in crate::client) fn reset_active_reblit_commit_cleanup_durability_events() {
    DURABILITY_EVENTS.with(|events| events.borrow_mut().clear());
    DURABILITY_FAULT.with(|slot| slot.set(None));
}

#[cfg(test)]
pub(in crate::client) fn take_active_reblit_commit_cleanup_durability_events(
) -> Vec<ActiveReblitCommitCleanupDurabilityEvent> {
    DURABILITY_EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

#[cfg(test)]
fn record_event(file: &File, kind: ActiveReblitCommitCleanupDurabilityEventKind) {
    let metadata = file.metadata().expect("retained cleanup descriptor remains readable");
    let event = match kind {
        ActiveReblitCommitCleanupDurabilityEventKind::PreviousTree => {
            ActiveReblitCommitCleanupDurabilityEvent::PreviousTreeSynced {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }
        ActiveReblitCommitCleanupDurabilityEventKind::PreviousWrapper => {
            ActiveReblitCommitCleanupDurabilityEvent::PreviousWrapperSynced {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }
        ActiveReblitCommitCleanupDurabilityEventKind::ReplacementWrapper => {
            ActiveReblitCommitCleanupDurabilityEvent::ReplacementWrapperSynced {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }
        ActiveReblitCommitCleanupDurabilityEventKind::RootsParent => {
            ActiveReblitCommitCleanupDurabilityEvent::RootsParentSynced {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }
        ActiveReblitCommitCleanupDurabilityEventKind::QuarantineParent => {
            ActiveReblitCommitCleanupDurabilityEvent::QuarantineParentSynced {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }
    };
    DURABILITY_EVENTS.with(|events| events.borrow_mut().push(event));
}

#[cfg(not(test))]
fn record_event(_file: &File, _kind: ActiveReblitCommitCleanupDurabilityEventKind) {}

#[cfg(test)]
fn record_final_finish() {
    DURABILITY_EVENTS.with(|events| {
        events.borrow_mut().push(ActiveReblitCommitCleanupDurabilityEvent::FinalFinishProven);
    });
}

#[cfg(not(test))]
fn record_final_finish() {}
