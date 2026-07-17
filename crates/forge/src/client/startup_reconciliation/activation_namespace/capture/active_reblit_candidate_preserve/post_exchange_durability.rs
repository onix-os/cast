//! Indivisible post-exchange durability for one preserved ActiveReblit wrapper.
//!
//! Applied and independently admitted preserved evidence converge here. The
//! retained descriptors are consumed through one fixed suffix; an error drops
//! every remaining capability and exposes no partial result.

use std::{fs::File, io, path::PathBuf};

use crate::{Installation, transition_journal::TransitionRecord, tree_marker::TreeMarkerError};

use super::{
    ActiveReblitCandidatePreserveEffectError, ActiveReblitCandidatePreserveLayout,
    ProjectedActiveReblitCandidatePreserveNamespace, RetainedActiveReblitCandidatePreserveParents, os_name,
};
use crate::client::startup_reconciliation::activation_namespace::capture::{
    CaptureError, NamespaceSnapshot, capture_snapshot,
};

/// Common exact POST capability accepted from either exchange reconciliation
/// or an independent Finish admission.
#[must_use = "ActiveReblit post-exchange evidence must complete durability"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct PendingActiveReblitCandidatePreservePostExchangeDurability
{
    parents: RetainedActiveReblitCandidatePreserveParents,
    authenticated_post: NamespaceSnapshot,
    authenticated_post_projection: ProjectedActiveReblitCandidatePreserveNamespace,
}

/// Opaque proof that all five descriptor barriers and the final fresh POST
/// proof completed through one consuming suffix.
#[must_use = "durable ActiveReblit namespace evidence must remain sealed"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct DurableActiveReblitCandidatePreservePostExchangeNamespace
{
    parents: RetainedActiveReblitCandidatePreserveParents,
    final_post: NamespaceSnapshot,
    final_post_projection: ProjectedActiveReblitCandidatePreserveNamespace,
}

impl PendingActiveReblitCandidatePreservePostExchangeDurability {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn new(
        parents: RetainedActiveReblitCandidatePreserveParents,
        authenticated_post: NamespaceSnapshot,
        authenticated_post_projection: ProjectedActiveReblitCandidatePreserveNamespace,
    ) -> Self {
        Self {
            parents,
            authenticated_post,
            authenticated_post_projection,
        }
    }

    /// Consume the entire fixed suffix. No intermediate capability survives a
    /// failed revalidation, injected fault, sync, or final proof.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn complete(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<
        DurableActiveReblitCandidatePreservePostExchangeNamespace,
        ActiveReblitCandidatePreservePostExchangeDurabilityError,
    > {
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
            ActiveReblitCandidatePreservePostExchangeDurabilityError::CandidateSync {
                path: post_candidate_path(&parents),
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

        run_before_candidate_wrapper_sync();
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;
        require_boundary(DurabilityBoundary::CandidateWrapperSync)?;
        parents.candidate_wrapper.sync_all().map_err(|source| {
            ActiveReblitCandidatePreservePostExchangeDurabilityError::CandidateWrapperSync {
                path: post_candidate_wrapper_path(&parents),
                source,
            }
        })?;
        record_candidate_wrapper_synced(&parents.candidate_wrapper);
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;

        run_before_reservation_wrapper_sync();
        require_exact_post(
            installation,
            record,
            &parents,
            &authenticated_post,
            &authenticated_post_projection,
        )?;
        require_boundary(DurabilityBoundary::ReservationWrapperSync)?;
        parents.reservation_wrapper.sync_all().map_err(|source| {
            ActiveReblitCandidatePreservePostExchangeDurabilityError::ReservationWrapperSync {
                path: parents.roots_path.join("staging"),
                source,
            }
        })?;
        record_reservation_wrapper_synced(&parents.reservation_wrapper);
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
            ActiveReblitCandidatePreservePostExchangeDurabilityError::RootsParentSync {
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
            ActiveReblitCandidatePreservePostExchangeDurabilityError::QuarantineParentSync {
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
            return Err(ActiveReblitCandidatePreservePostExchangeDurabilityError::FinalNamespaceChanged);
        }
        let final_post_projection = ProjectedActiveReblitCandidatePreserveNamespace::capture(&final_post, record)?;
        if final_post_projection.layout != ActiveReblitCandidatePreserveLayout::Preserved
            || final_post_projection != authenticated_post_projection
        {
            return Err(ActiveReblitCandidatePreservePostExchangeDurabilityError::FinalProjectionChanged);
        }
        require_exact_post(installation, record, &parents, &final_post, &final_post_projection)?;
        record_final_post_proven();

        Ok(DurableActiveReblitCandidatePreservePostExchangeNamespace {
            parents,
            final_post,
            final_post_projection,
        })
    }
}

impl DurableActiveReblitCandidatePreservePostExchangeNamespace {
    /// Revalidate the sealed durable POST through a fresh capture without
    /// repeating a durability barrier.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn revalidate(
        &self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<(), ActiveReblitCandidatePreservePostExchangeDurabilityError> {
        require_exact_post(
            installation,
            record,
            &self.parents,
            &self.final_post,
            &self.final_post_projection,
        )?;
        let fresh_post = capture_snapshot(installation, record)?;
        fresh_post.revalidate_retained()?;
        if fresh_post.fingerprint() != self.final_post.fingerprint() {
            return Err(ActiveReblitCandidatePreservePostExchangeDurabilityError::FinalNamespaceChanged);
        }
        let fresh_projection = ProjectedActiveReblitCandidatePreserveNamespace::capture(&fresh_post, record)?;
        if fresh_projection != self.final_post_projection {
            return Err(ActiveReblitCandidatePreservePostExchangeDurabilityError::FinalProjectionChanged);
        }
        require_exact_post(installation, record, &self.parents, &fresh_post, &fresh_projection)
    }
}

fn require_exact_post(
    installation: &Installation,
    record: &TransitionRecord,
    parents: &RetainedActiveReblitCandidatePreserveParents,
    snapshot: &NamespaceSnapshot,
    projection: &ProjectedActiveReblitCandidatePreserveNamespace,
) -> Result<(), ActiveReblitCandidatePreservePostExchangeDurabilityError> {
    installation.revalidate_mutable_namespace()?;
    snapshot.revalidate_retained()?;
    if projection.layout != ActiveReblitCandidatePreserveLayout::Preserved
        || ProjectedActiveReblitCandidatePreserveNamespace::capture(snapshot, record)? != *projection
    {
        return Err(ActiveReblitCandidatePreservePostExchangeDurabilityError::PostEvidenceChanged);
    }
    parents.revalidate_layout(installation, ActiveReblitCandidatePreserveLayout::Preserved)?;
    snapshot.revalidate_retained()?;
    if ProjectedActiveReblitCandidatePreserveNamespace::capture(snapshot, record)? != *projection {
        return Err(ActiveReblitCandidatePreservePostExchangeDurabilityError::PostEvidenceChanged);
    }
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn post_candidate_wrapper_path(parents: &RetainedActiveReblitCandidatePreserveParents) -> PathBuf {
    parents.quarantine_path.join(os_name(parents.target_name.as_bytes()))
}

fn post_candidate_path(parents: &RetainedActiveReblitCandidatePreserveParents) -> PathBuf {
    post_candidate_wrapper_path(parents).join("usr")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurabilityBoundary {
    CandidateSync,
    CandidateWrapperSync,
    ReservationWrapperSync,
    RootsParentSync,
    QuarantineParentSync,
    FinalPostCapture,
}

fn require_boundary(
    boundary: DurabilityBoundary,
) -> Result<(), ActiveReblitCandidatePreservePostExchangeDurabilityError> {
    POST_EXCHANGE_DURABILITY_FAULT.with(|slot| {
        if slot.get() == Some(fault_point(boundary)) {
            slot.set(None);
            Err(
                ActiveReblitCandidatePreservePostExchangeDurabilityError::InjectedFault {
                    point: fault_point(boundary),
                },
            )
        } else {
            Ok(())
        }
    })
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ActiveReblitCandidatePreservePostExchangeDurabilityError
{
    #[error(transparent)]
    Effect(#[from] ActiveReblitCandidatePreserveEffectError),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("revalidate the installation around ActiveReblit post-exchange durability")]
    Installation(#[from] crate::installation::Error),
    #[error("authenticated ActiveReblit evidence is no longer exact POST")]
    PostEvidenceChanged,
    #[error("sync retained ActiveReblit candidate tree at `{}`", path.display())]
    CandidateSync {
        path: PathBuf,
        #[source]
        source: TreeMarkerError,
    },
    #[error("sync retained ActiveReblit candidate wrapper at `{}`", path.display())]
    CandidateWrapperSync {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync retained ActiveReblit reservation wrapper at `{}`", path.display())]
    ReservationWrapperSync {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync retained `.cast/root` at `{}`", path.display())]
    RootsParentSync {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync retained `.cast/quarantine` at `{}`", path.display())]
    QuarantineParentSync {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the final fresh ActiveReblit POST namespace changed")]
    FinalNamespaceChanged,
    #[error("the final fresh ActiveReblit POST projection changed")]
    FinalProjectionChanged,
    #[error("injected ActiveReblit post-exchange durability fault at {point:?}")]
    InjectedFault {
        point: ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint {
    CandidateSync,
    CandidateWrapperSync,
    ReservationWrapperSync,
    RootsParentSync,
    QuarantineParentSync,
    FinalPostCapture,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitCandidatePreservePostExchangeDurabilityEvent {
    CandidateSynced { device: u64, inode: u64 },
    CandidateWrapperSynced { device: u64, inode: u64 },
    ReservationWrapperSynced { device: u64, inode: u64 },
    RootsParentSynced { device: u64, inode: u64 },
    QuarantineParentSynced { device: u64, inode: u64 },
    FinalPostProven,
}

fn fault_point(boundary: DurabilityBoundary) -> ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint {
    match boundary {
        DurabilityBoundary::CandidateSync => {
            ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::CandidateSync
        }
        DurabilityBoundary::CandidateWrapperSync => {
            ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::CandidateWrapperSync
        }
        DurabilityBoundary::ReservationWrapperSync => {
            ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::ReservationWrapperSync
        }
        DurabilityBoundary::RootsParentSync => {
            ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::RootsParentSync
        }
        DurabilityBoundary::QuarantineParentSync => {
            ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::QuarantineParentSync
        }
        DurabilityBoundary::FinalPostCapture => {
            ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::FinalPostCapture
        }
    }
}

std::thread_local! {
    static POST_EXCHANGE_DURABILITY_FAULT:
        std::cell::Cell<Option<ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint>> = const { std::cell::Cell::new(None) };
    static POST_EXCHANGE_DURABILITY_EVENTS:
        std::cell::RefCell<Vec<ActiveReblitCandidatePreservePostExchangeDurabilityEvent>> = const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_CANDIDATE_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_CANDIDATE_WRAPPER_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_RESERVATION_WRAPPER_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_ROOTS_PARENT_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_QUARANTINE_PARENT_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_FINAL_POST_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

pub(in crate::client) fn arm_active_reblit_candidate_preserve_post_exchange_durability_fault(
    point: ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint,
) {
    POST_EXCHANGE_DURABILITY_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(point)).is_none(),
            "ActiveReblit durability fault already armed"
        );
    });
}

pub(in crate::client) fn reset_active_reblit_candidate_preserve_post_exchange_durability_events() {
    POST_EXCHANGE_DURABILITY_EVENTS.with(|events| events.borrow_mut().clear());
    POST_EXCHANGE_DURABILITY_FAULT.with(|slot| slot.set(None));
}

pub(in crate::client) fn take_active_reblit_candidate_preserve_post_exchange_durability_events()
-> Vec<ActiveReblitCandidatePreservePostExchangeDurabilityEvent> {
    POST_EXCHANGE_DURABILITY_EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

macro_rules! define_hook {
    ($arm:ident, $run:ident, $slot:ident) => {
        pub(in crate::client) fn $arm(hook: impl FnOnce() + 'static) {
            $slot.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
        }

        fn $run() {
            $slot.with(|slot| {
                if let Some(hook) = slot.borrow_mut().take() {
                    hook();
                }
            });
        }
    };
}

define_hook!(
    arm_before_active_reblit_candidate_preserve_post_exchange_candidate_sync,
    run_before_candidate_sync,
    BEFORE_CANDIDATE_SYNC
);
define_hook!(
    arm_before_active_reblit_candidate_preserve_post_exchange_candidate_wrapper_sync,
    run_before_candidate_wrapper_sync,
    BEFORE_CANDIDATE_WRAPPER_SYNC
);
define_hook!(
    arm_before_active_reblit_candidate_preserve_post_exchange_reservation_wrapper_sync,
    run_before_reservation_wrapper_sync,
    BEFORE_RESERVATION_WRAPPER_SYNC
);
define_hook!(
    arm_before_active_reblit_candidate_preserve_post_exchange_roots_parent_sync,
    run_before_roots_parent_sync,
    BEFORE_ROOTS_PARENT_SYNC
);
define_hook!(
    arm_before_active_reblit_candidate_preserve_post_exchange_quarantine_parent_sync,
    run_before_quarantine_parent_sync,
    BEFORE_QUARANTINE_PARENT_SYNC
);
define_hook!(
    arm_before_active_reblit_candidate_preserve_post_exchange_final_post_capture,
    run_before_final_post_capture,
    BEFORE_FINAL_POST_CAPTURE
);

fn record_candidate_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::CandidateSynced { device, inode }
    });
}

fn record_candidate_wrapper_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::CandidateWrapperSynced { device, inode }
    });
}

fn record_reservation_wrapper_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::ReservationWrapperSynced { device, inode }
    });
}

fn record_roots_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::RootsParentSynced { device, inode }
    });
}

fn record_quarantine_parent_synced(file: &File) {
    record_descriptor_event(file, |device, inode| {
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::QuarantineParentSynced { device, inode }
    });
}

fn record_descriptor_event(
    file: &File,
    event: impl FnOnce(u64, u64) -> ActiveReblitCandidatePreservePostExchangeDurabilityEvent,
) {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata().expect("inspect synced ActiveReblit descriptor");
    POST_EXCHANGE_DURABILITY_EVENTS.with(|events| {
        events.borrow_mut().push(event(metadata.dev(), metadata.ino()));
    });
}

fn record_final_post_proven() {
    POST_EXCHANGE_DURABILITY_EVENTS.with(|events| {
        events
            .borrow_mut()
            .push(ActiveReblitCandidatePreservePostExchangeDurabilityEvent::FinalPostProven);
    });
}
