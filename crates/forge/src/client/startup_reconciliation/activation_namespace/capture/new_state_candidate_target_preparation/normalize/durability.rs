//! Ordered durability for one freshly normalized canonical NewState target.
//!
//! Semantic reconciliation transfers its fresh retained canonical snapshot
//! into this module instead of publishing success. The capability is consumed
//! by the exact target barrier, a complete public-name revalidation, the exact
//! quarantine-parent barrier, and one final matching canonical capture. No
//! descriptor or partial durability authority escapes a failure.

use std::{fs::File, io};

use crate::{Installation, transition_journal::TransitionRecord};

use super::super::{NewStateTargetNormalizeLayout, ProjectedNewStateTargetNormalizeNamespace};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    CaptureError, NamespaceSnapshot, NewStateCandidatePreserveCaptureError, TreeLocation, capture_snapshot,
};

/// Opaque fresh same-inode canonical evidence produced only by semantic
/// restrictive-residue normalization reconciliation.
#[must_use = "fresh canonical target evidence must complete ordered durability"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct FreshCanonicalNewStateTargetNormalizeNamespace
{
    snapshot: NamespaceSnapshot,
    projection: ProjectedNewStateTargetNormalizeNamespace,
}

impl FreshCanonicalNewStateTargetNormalizeNamespace {
    pub(super) fn new(snapshot: NamespaceSnapshot, projection: ProjectedNewStateTargetNormalizeNamespace) -> Self {
        Self { snapshot, projection }
    }

    /// Consume fresh canonical evidence through both exact descriptor barriers
    /// and a final matching canonical namespace capture.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn complete_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<(), NewStateTargetNormalizeDurabilityError> {
        run_before_target_sync();
        self.require_exact_canonical(installation, record)?;
        require_boundary(DurabilityBoundary::TargetSync)?;
        let target = self.retained_target(record)?;
        target
            .sync_all()
            .map_err(NewStateTargetNormalizeDurabilityError::TargetSync)?;
        record_target_synced(target);

        run_before_quarantine_parent_sync();
        self.require_exact_canonical(installation, record)?;
        require_boundary(DurabilityBoundary::QuarantineParentSync)?;
        self.snapshot
            .quarantine
            .sync_all()
            .map_err(NewStateTargetNormalizeDurabilityError::QuarantineParentSync)?;
        record_quarantine_parent_synced(&self.snapshot.quarantine);

        run_before_final_canonical_capture();
        self.require_exact_canonical(installation, record)?;
        require_boundary(DurabilityBoundary::FinalCanonicalCapture)?;
        let final_canonical = capture_snapshot(installation, record)?;
        final_canonical.revalidate_retained()?;
        if final_canonical.fingerprint() != self.snapshot.fingerprint() {
            return Err(NewStateTargetNormalizeDurabilityError::FinalCanonicalChanged);
        }
        let final_projection = ProjectedNewStateTargetNormalizeNamespace::capture(&final_canonical, record)?;
        if final_projection.layout() != NewStateTargetNormalizeLayout::EmptyPrivate
            || final_projection != self.projection
        {
            return Err(NewStateTargetNormalizeDurabilityError::FinalCanonicalChanged);
        }
        installation.revalidate_mutable_namespace()?;
        final_canonical.revalidate_retained()?;
        record_final_canonical_proven();
        Ok(())
    }

    fn require_exact_canonical(
        &self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<(), NewStateTargetNormalizeDurabilityError> {
        installation.revalidate_mutable_namespace()?;
        self.snapshot.revalidate_retained()?;
        if self.projection.layout() != NewStateTargetNormalizeLayout::EmptyPrivate
            || ProjectedNewStateTargetNormalizeNamespace::capture(&self.snapshot, record)? != self.projection
        {
            return Err(NewStateTargetNormalizeDurabilityError::CanonicalEvidenceChanged);
        }
        self.retained_target(record)?;
        installation.revalidate_mutable_namespace()?;
        self.snapshot.revalidate_retained()?;
        if ProjectedNewStateTargetNormalizeNamespace::capture(&self.snapshot, record)? != self.projection {
            return Err(NewStateTargetNormalizeDurabilityError::CanonicalEvidenceChanged);
        }
        Ok(())
    }

    fn retained_target<'snapshot>(
        &'snapshot self,
        record: &TransitionRecord,
    ) -> Result<&'snapshot File, NewStateTargetNormalizeDurabilityError> {
        let expected_name = record.quarantine_name.as_str().as_bytes();
        let mut targets = self.snapshot.quarantine_entries.iter().filter(|wrapper| {
            wrapper.fingerprint.role == TreeLocation::TransitionQuarantine && wrapper.fingerprint.name == expected_name
        });
        let target = targets
            .next()
            .ok_or(NewStateTargetNormalizeDurabilityError::CanonicalTargetChanged)?;
        if targets.next().is_some() {
            return Err(NewStateTargetNormalizeDurabilityError::CanonicalTargetChanged);
        }
        Ok(&target.directory)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurabilityBoundary {
    TargetSync,
    QuarantineParentSync,
    FinalCanonicalCapture,
}

fn require_boundary(boundary: DurabilityBoundary) -> Result<(), NewStateTargetNormalizeDurabilityError> {
    if boundary_is_faulted(boundary) {
        #[cfg(test)]
        return Err(NewStateTargetNormalizeDurabilityError::InjectedFault {
            point: fault_point(boundary),
        });
        #[cfg(not(test))]
        unreachable!("production NewState target-normalization durability boundaries cannot be faulted");
    }
    Ok(())
}

#[cfg(test)]
fn boundary_is_faulted(boundary: DurabilityBoundary) -> bool {
    NORMALIZE_DURABILITY_FAULT.with(|slot| {
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
pub(in crate::client::startup_reconciliation::activation_namespace) enum NewStateTargetNormalizeDurabilityError {
    #[error("capture or revalidate exact canonical NewState target durability evidence")]
    Capture(#[from] CaptureError),
    #[error("project exact canonical NewState target durability evidence")]
    Projection(#[from] NewStateCandidatePreserveCaptureError),
    #[error("revalidate the mutable installation namespace around NewState target durability")]
    Installation(#[from] crate::installation::Error),
    #[error("fresh canonical NewState target evidence changed before durability completed")]
    CanonicalEvidenceChanged,
    #[error("the exact named canonical NewState target descriptor changed")]
    CanonicalTargetChanged,
    #[error("sync the exact retained canonical NewState target")]
    TargetSync(#[source] io::Error),
    #[error("sync the exact retained NewState quarantine parent")]
    QuarantineParentSync(#[source] io::Error),
    #[error("the final fresh NewState target capture is not the exact canonical baseline")]
    FinalCanonicalChanged,
    #[cfg(test)]
    #[error("injected NewState target-normalization durability fault at {point:?}")]
    InjectedFault {
        point: NewStateTargetNormalizeDurabilityFaultPoint,
    },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum NewStateTargetNormalizeDurabilityFaultPoint {
    TargetSync,
    QuarantineParentSync,
    FinalCanonicalCapture,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum NewStateTargetNormalizeDurabilityEvent {
    TargetSynced { device: u64, inode: u64 },
    QuarantineParentSynced { device: u64, inode: u64 },
    FinalCanonicalProven,
}

#[cfg(test)]
fn fault_point(boundary: DurabilityBoundary) -> NewStateTargetNormalizeDurabilityFaultPoint {
    match boundary {
        DurabilityBoundary::TargetSync => NewStateTargetNormalizeDurabilityFaultPoint::TargetSync,
        DurabilityBoundary::QuarantineParentSync => NewStateTargetNormalizeDurabilityFaultPoint::QuarantineParentSync,
        DurabilityBoundary::FinalCanonicalCapture => NewStateTargetNormalizeDurabilityFaultPoint::FinalCanonicalCapture,
    }
}

#[cfg(test)]
std::thread_local! {
    static NORMALIZE_DURABILITY_FAULT:
        std::cell::Cell<Option<NewStateTargetNormalizeDurabilityFaultPoint>> = const { std::cell::Cell::new(None) };
    static NORMALIZE_DURABILITY_EVENTS:
        std::cell::RefCell<Vec<NewStateTargetNormalizeDurabilityEvent>> = const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_TARGET_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_QUARANTINE_PARENT_SYNC:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_FINAL_CANONICAL_CAPTURE:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_new_state_target_normalize_durability_fault(
    point: NewStateTargetNormalizeDurabilityFaultPoint,
) {
    NORMALIZE_DURABILITY_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(point)).is_none(),
            "target-normalization durability fault already armed"
        );
    });
}

#[cfg(test)]
pub(in crate::client) fn reset_new_state_target_normalize_durability_events() {
    NORMALIZE_DURABILITY_EVENTS.with(|events| events.borrow_mut().clear());
}

#[cfg(test)]
pub(in crate::client) fn take_new_state_target_normalize_durability_events()
-> Vec<NewStateTargetNormalizeDurabilityEvent> {
    NORMALIZE_DURABILITY_EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_target_normalize_target_sync(hook: impl FnOnce() + 'static) {
    BEFORE_TARGET_SYNC.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_target_normalize_quarantine_parent_sync(hook: impl FnOnce() + 'static) {
    BEFORE_QUARANTINE_PARENT_SYNC.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_target_normalize_final_canonical_capture(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_CANONICAL_CAPTURE.with(|slot| {
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
fn run_before_final_canonical_capture() {
    BEFORE_FINAL_CANONICAL_CAPTURE.with(run_hook);
}

#[cfg(not(test))]
fn run_before_final_canonical_capture() {}

#[cfg(test)]
fn run_hook(slot: &std::cell::RefCell<Option<Box<dyn FnOnce()>>>) {
    if let Some(hook) = slot.borrow_mut().take() {
        hook();
    }
}

#[cfg(test)]
fn record_target_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        NewStateTargetNormalizeDurabilityEvent::TargetSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_target_synced(_file: &File) {}

#[cfg(test)]
fn record_quarantine_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        NewStateTargetNormalizeDurabilityEvent::QuarantineParentSynced { device, inode }
    });
}

#[cfg(not(test))]
fn record_quarantine_parent_synced(_file: &File) {}

#[cfg(test)]
fn record_descriptor_event(file: &File, event: impl FnOnce(u64, u64) -> NewStateTargetNormalizeDurabilityEvent) {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file
        .metadata()
        .expect("inspect synced NewState target durability descriptor");
    NORMALIZE_DURABILITY_EVENTS.with(|events| {
        events.borrow_mut().push(event(metadata.dev(), metadata.ino()));
    });
}

fn record_final_canonical_proven() {
    #[cfg(test)]
    NORMALIZE_DURABILITY_EVENTS.with(|events| {
        events
            .borrow_mut()
            .push(NewStateTargetNormalizeDurabilityEvent::FinalCanonicalProven);
    });
}
