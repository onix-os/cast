//! Descriptor-bound parent durability for an exact reverse `/usr` exchange.
//!
//! This layer consumes both retained parent descriptors. It syncs the restored
//! staging-side parent before the installation-root parent, then authenticates
//! one final fresh PRE snapshot. No descriptor, retry authority, or partial
//! completion capability escapes any failure.

use std::io;

use crate::{Installation, transition_journal::TransitionRecord};

use super::{ProjectedReverseNamespace, RetainedReverseExchangeParents, ReverseExchangeCaptureError};
use crate::client::startup_reconciliation::activation_namespace::{
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
    policy::UsrExchangeLayout,
};

/// Opaque proof that both exact reverse-exchange parents are durable and a
/// final fresh capture still matches the authenticated PRE baseline.
#[must_use = "durable reverse-exchange namespace evidence must be consumed by persistence"]
#[allow(dead_code)] // consumed by the later journal-persistence checkpoint
pub(in crate::client::startup_reconciliation::activation_namespace) struct DurableReverseExchangeNamespace {
    _parents: RetainedReverseExchangeParents,
    _final_pre: NamespaceSnapshot,
    _final_pre_projection: ProjectedReverseNamespace,
}

impl RetainedReverseExchangeParents {
    /// Consume exact PRE evidence through both parent barriers and one final
    /// recapture. Every error drops both retained descriptors with `self`.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn complete_parent_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
        authenticated_pre: NamespaceSnapshot,
        authenticated_pre_projection: ProjectedReverseNamespace,
    ) -> Result<DurableReverseExchangeNamespace, ReverseExchangeDurabilityError> {
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;

        require_boundary(DurabilityBoundary::StagingParentSync)?;
        self.sync_staging_parent()?;
        record_staging_parent_synced(&self.staging);
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;

        run_before_installation_root_sync();
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;
        require_boundary(DurabilityBoundary::InstallationRootSync)?;
        self.sync_installation_root()?;
        record_installation_root_synced(&self.root);
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_pre_projection,
        )?;

        require_boundary(DurabilityBoundary::FinalPreCapture)?;
        run_before_final_pre_capture();
        let final_pre = capture_snapshot(installation, record)?;
        final_pre.revalidate_retained()?;
        if final_pre.fingerprint() != authenticated_pre.fingerprint() {
            return Err(ReverseExchangeDurabilityError::FinalNamespaceChanged);
        }
        let final_pre_projection = ProjectedReverseNamespace::capture(&final_pre, record)?;
        if final_pre_projection != authenticated_pre_projection
            || final_pre_projection.layout() != UsrExchangeLayout::Pre
        {
            return Err(ReverseExchangeDurabilityError::FinalProjectionChanged);
        }
        require_exact_pre(installation, record, &self, &final_pre, &final_pre_projection)?;
        record_final_pre_proven();

        Ok(DurableReverseExchangeNamespace {
            _parents: self,
            _final_pre: final_pre,
            _final_pre_projection: final_pre_projection,
        })
    }

    fn sync_staging_parent(&self) -> Result<(), ReverseExchangeDurabilityError> {
        self.staging
            .sync_all()
            .map_err(|source| ReverseExchangeDurabilityError::StagingParentSync {
                path: self.staging_path.clone(),
                source,
            })
    }

    fn sync_installation_root(&self) -> Result<(), ReverseExchangeDurabilityError> {
        self.root
            .sync_all()
            .map_err(|source| ReverseExchangeDurabilityError::InstallationRootSync {
                path: self.root_path.clone(),
                source,
            })
    }
}

fn require_exact_pre(
    installation: &Installation,
    record: &TransitionRecord,
    parents: &RetainedReverseExchangeParents,
    snapshot: &NamespaceSnapshot,
    projection: &ProjectedReverseNamespace,
) -> Result<(), ReverseExchangeDurabilityError> {
    installation.revalidate_mutable_namespace()?;
    snapshot.revalidate_retained()?;
    if projection.layout() != UsrExchangeLayout::Pre
        || ProjectedReverseNamespace::capture(snapshot, record)? != *projection
    {
        return Err(ReverseExchangeDurabilityError::PreEvidenceChanged);
    }
    parents.revalidate_value_identity(installation)?;
    snapshot.revalidate_retained()?;
    if projection.layout() != UsrExchangeLayout::Pre
        || ProjectedReverseNamespace::capture(snapshot, record)? != *projection
    {
        return Err(ReverseExchangeDurabilityError::PreEvidenceChanged);
    }
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurabilityBoundary {
    StagingParentSync,
    InstallationRootSync,
    FinalPreCapture,
}

fn require_boundary(boundary: DurabilityBoundary) -> Result<(), ReverseExchangeDurabilityError> {
    if boundary_is_faulted(boundary) {
        #[cfg(test)]
        return Err(ReverseExchangeDurabilityError::InjectedFault {
            point: fault_point(boundary),
        });
        #[cfg(not(test))]
        unreachable!("production reverse parent-durability boundaries cannot be faulted");
    }
    Ok(())
}

#[cfg(test)]
fn boundary_is_faulted(boundary: DurabilityBoundary) -> bool {
    REVERSE_EXCHANGE_DURABILITY_FAULT.with(|slot| {
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
pub(in crate::client::startup_reconciliation::activation_namespace) enum ReverseExchangeDurabilityError {
    #[error("capture or revalidate exact reverse parent-durability evidence")]
    Capture(#[from] CaptureError),
    #[error("project exact reverse parent-durability evidence")]
    Projection(#[from] ReverseExchangeCaptureError),
    #[error("revalidate the retained mutable installation namespace around reverse parent durability")]
    Installation(#[from] crate::installation::Error),
    #[error("authenticated reverse parent-durability evidence is no longer exact PRE")]
    PreEvidenceChanged,
    #[error("sync retained staging parent during reverse `/usr` durability at `{}`", path.display())]
    StagingParentSync {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync retained installation root during reverse `/usr` durability at `{}`", path.display())]
    InstallationRootSync {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the final fresh reverse parent-durability namespace changed from its exact PRE baseline")]
    FinalNamespaceChanged,
    #[error("the final fresh reverse parent-durability projection changed from exact PRE")]
    FinalProjectionChanged,
    #[cfg(test)]
    #[error("injected reverse parent-durability fault at {point:?}")]
    InjectedFault { point: ReverseExchangeDurabilityFaultPoint },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ReverseExchangeDurabilityFaultPoint {
    StagingParentSync,
    InstallationRootSync,
    FinalPreCapture,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ReverseExchangeDurabilityEvent {
    StagingParentSynced { device: u64, inode: u64 },
    InstallationRootSynced { device: u64, inode: u64 },
    FinalPreProven,
}

#[cfg(test)]
fn fault_point(boundary: DurabilityBoundary) -> ReverseExchangeDurabilityFaultPoint {
    match boundary {
        DurabilityBoundary::StagingParentSync => ReverseExchangeDurabilityFaultPoint::StagingParentSync,
        DurabilityBoundary::InstallationRootSync => ReverseExchangeDurabilityFaultPoint::InstallationRootSync,
        DurabilityBoundary::FinalPreCapture => ReverseExchangeDurabilityFaultPoint::FinalPreCapture,
    }
}

#[cfg(test)]
std::thread_local! {
    static REVERSE_EXCHANGE_DURABILITY_FAULT:
        std::cell::Cell<Option<ReverseExchangeDurabilityFaultPoint>> = const { std::cell::Cell::new(None) };
    static REVERSE_EXCHANGE_DURABILITY_EVENTS:
        std::cell::RefCell<Vec<ReverseExchangeDurabilityEvent>> = const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_REVERSE_EXCHANGE_FINAL_PRE_CAPTURE:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_REVERSE_EXCHANGE_INSTALLATION_ROOT_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_reverse_exchange_durability_fault(point: ReverseExchangeDurabilityFaultPoint) {
    REVERSE_EXCHANGE_DURABILITY_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(point)).is_none(),
            "reverse durability fault already armed"
        );
    });
}

#[cfg(test)]
pub(in crate::client) fn reset_reverse_exchange_durability_events() {
    REVERSE_EXCHANGE_DURABILITY_EVENTS.with(|events| events.borrow_mut().clear());
}

#[cfg(test)]
pub(in crate::client) fn take_reverse_exchange_durability_events() -> Vec<ReverseExchangeDurabilityEvent> {
    REVERSE_EXCHANGE_DURABILITY_EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

#[cfg(test)]
pub(in crate::client) fn arm_before_reverse_exchange_final_pre_capture(hook: impl FnOnce() + 'static) {
    BEFORE_REVERSE_EXCHANGE_FINAL_PRE_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_reverse_exchange_installation_root_sync(hook: impl FnOnce() + 'static) {
    BEFORE_REVERSE_EXCHANGE_INSTALLATION_ROOT_SYNC.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_installation_root_sync() {
    BEFORE_REVERSE_EXCHANGE_INSTALLATION_ROOT_SYNC.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_installation_root_sync() {}

#[cfg(test)]
fn run_before_final_pre_capture() {
    BEFORE_REVERSE_EXCHANGE_FINAL_PRE_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_final_pre_capture() {}

#[cfg(test)]
fn record_staging_parent_synced(file: &std::fs::File) {
    record_descriptor_event(file, |device, inode| {
        ReverseExchangeDurabilityEvent::StagingParentSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_staging_parent_synced(_file: &std::fs::File) {}

#[cfg(test)]
fn record_installation_root_synced(file: &std::fs::File) {
    record_descriptor_event(file, |device, inode| {
        ReverseExchangeDurabilityEvent::InstallationRootSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_installation_root_synced(_file: &std::fs::File) {}

#[cfg(test)]
fn record_descriptor_event(file: &std::fs::File, event: impl FnOnce(u64, u64) -> ReverseExchangeDurabilityEvent) {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata().expect("inspect synced reverse parent descriptor");
    REVERSE_EXCHANGE_DURABILITY_EVENTS.with(|events| {
        events.borrow_mut().push(event(metadata.dev(), metadata.ino()));
    });
}

fn record_final_pre_proven() {
    #[cfg(test)]
    REVERSE_EXCHANGE_DURABILITY_EVENTS.with(|events| {
        events.borrow_mut().push(ReverseExchangeDurabilityEvent::FinalPreProven);
    });
}
