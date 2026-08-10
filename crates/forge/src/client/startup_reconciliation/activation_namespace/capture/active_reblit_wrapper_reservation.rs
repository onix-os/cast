//! Evidence and one-shot creation of the absent ActiveReblit reservation.
//!
//! An ActiveReblit rollback that crashed before its replacement wrapper was
//! reserved has no destination to preserve its candidate into. Creating that
//! exact wrapper restores the staged shape the exchange effect already proves,
//! so this module owns only the absent prefix and one child creation.

use std::{ffi::CString, io};

use crate::{
    linux_fs::mkdirat_once,
    transition_journal::{Operation, Phase, RuntimeEpoch, TransitionRecord},
};

use super::{
    InodeWitness, NamespaceFingerprint, NamespaceSnapshot, RootAbiFingerprint, TreeLocation, UsrFingerprint,
    WrapperFingerprint, capture_snapshot,
};

/// Stable quarantine-parent identity across one future child creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MutableQuarantineIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
}

impl From<InodeWitness> for MutableQuarantineIdentity {
    fn from(witness: InodeWitness) -> Self {
        Self {
            device: witness.device,
            inode: witness.inode,
            mode: witness.mode,
            owner: witness.owner,
            group: witness.group,
        }
    }
}

/// Namespace state that reserving the replacement wrapper may not change.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CommonNonReservationInvariant {
    root: InodeWitness,
    roots: InodeWitness,
    epoch: RuntimeEpoch,
    live: UsrFingerprint,
    root_abi: RootAbiFingerprint,
    isolation_abi: RootAbiFingerprint,
    root_wrappers: Vec<WrapperFingerprint>,
    other_quarantine_wrappers: Vec<WrapperFingerprint>,
    target_residue: bool,
}

impl CommonNonReservationInvariant {
    fn capture(fingerprint: &NamespaceFingerprint, state: i32) -> Self {
        Self {
            root: fingerprint.root,
            roots: fingerprint.roots,
            epoch: fingerprint.epoch.clone(),
            live: fingerprint.live.clone(),
            root_abi: fingerprint.root_abi.clone(),
            isolation_abi: fingerprint.isolation_abi.clone(),
            root_wrappers: fingerprint.roots_entries.clone(),
            other_quarantine_wrappers: fingerprint
                .quarantine_entries
                .iter()
                .filter(|wrapper| !is_reservation_for(wrapper, state))
                .cloned()
                .collect(),
            target_residue: fingerprint.new_state_target_residue.is_some(),
        }
    }
}

fn is_reservation_for(wrapper: &WrapperFingerprint, state: i32) -> bool {
    matches!(wrapper.role, TreeLocation::ActiveReblitWrapper { state: actual, .. } if actual == state)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ActiveReblitWrapperReservationLayout {
    Absent,
    EmptyPrivate,
}

/// Projection spanning exactly the two safe reservation prefixes.
///
/// A newly created wrapper has no PRE inode identity to compare, so the exact
/// name, the quarantine parent, and every non-reservation invariant carry the
/// proof instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation::activation_namespace) struct ProjectedActiveReblitWrapperReservationNamespace
{
    layout: ActiveReblitWrapperReservationLayout,
    wrapper_name: Vec<u8>,
    quarantine: MutableQuarantineIdentity,
    invariant: CommonNonReservationInvariant,
}

impl ProjectedActiveReblitWrapperReservationNamespace {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, ActiveReblitWrapperReservationCaptureError> {
        let state = require_active_reblit_candidate_preserve(record)?;
        let wrapper_name = reservation_name(record, state)?;
        let fingerprint = snapshot.fingerprint();
        let reservations = fingerprint
            .quarantine_entries
            .iter()
            .filter(|wrapper| is_reservation_for(wrapper, state))
            .collect::<Vec<_>>();
        let layout = match reservations.as_slice() {
            [] => ActiveReblitWrapperReservationLayout::Absent,
            [reserved]
                if reserved.name == wrapper_name.as_bytes()
                    && reserved.has_exact_private_permissions()
                    && reserved.entries.is_empty()
                    && reserved.usr.is_none()
                    && reserved.slot.is_none() =>
            {
                ActiveReblitWrapperReservationLayout::EmptyPrivate
            }
            _ => return Err(ActiveReblitWrapperReservationCaptureError::NotReservationLayout),
        };
        Ok(Self {
            layout,
            wrapper_name: wrapper_name.into_bytes(),
            quarantine: fingerprint.quarantine.into(),
            invariant: CommonNonReservationInvariant::capture(fingerprint, state),
        })
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn layout(
        &self,
    ) -> ActiveReblitWrapperReservationLayout {
        self.layout
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn require_absent_to_reserved(
        &self,
        after: &Self,
    ) -> Result<(), ActiveReblitWrapperReservationCaptureError> {
        if self.layout != ActiveReblitWrapperReservationLayout::Absent
            || after.layout != ActiveReblitWrapperReservationLayout::EmptyPrivate
        {
            return Err(ActiveReblitWrapperReservationCaptureError::NotAbsentToReserved {
                before: self.layout,
                after: after.layout,
            });
        }
        if self.wrapper_name != after.wrapper_name
            || self.quarantine != after.quarantine
            || self.invariant != after.invariant
        {
            return Err(ActiveReblitWrapperReservationCaptureError::InvariantChanged);
        }
        Ok(())
    }
}

/// Opaque absent-reservation evidence retaining its own PRE snapshot.
#[must_use = "ActiveReblit reservation evidence must be prepared and consumed"]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitWrapperReservationNamespaceEvidence {
    pub(in crate::client::startup_reconciliation::activation_namespace) baseline: NamespaceSnapshot,
    pub(in crate::client::startup_reconciliation::activation_namespace) projection:
        ProjectedActiveReblitWrapperReservationNamespace,
}

impl UsrRollbackActiveReblitWrapperReservationNamespaceEvidence {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        baseline: NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, ActiveReblitWrapperReservationCaptureError> {
        let projection = ProjectedActiveReblitWrapperReservationNamespace::capture(&baseline, record)?;
        Ok(Self { baseline, projection })
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn reservation_name(
        record: &TransitionRecord,
    ) -> Result<CString, ActiveReblitWrapperReservationCaptureError> {
        let state = require_active_reblit_candidate_preserve(record)?;
        CString::new(reservation_name(record, state)?)
            .map_err(|_| ActiveReblitWrapperReservationCaptureError::WrongReservationName)
    }
}

#[must_use = "an ActiveReblit reservation attempt requires fresh semantic reconciliation"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct PendingActiveReblitWrapperReservationReconciliation
{
    authenticated_pre: NamespaceSnapshot,
    authenticated_pre_projection: ProjectedActiveReblitWrapperReservationNamespace,
    raw_report: io::Result<()>,
}

/// Fieldless semantic result of exactly one reservation attempt.
#[must_use = "a consumed ActiveReblit reservation attempt must be handled"]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ActiveReblitWrapperReservationReconciliation {
    RestartRequired,
    NotApplied,
    Ambiguous,
}

impl NamespaceSnapshot {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn attempt_active_reblit_wrapper_reservation_once(
        self,
        wrapper_name: &CString,
        projection: ProjectedActiveReblitWrapperReservationNamespace,
    ) -> PendingActiveReblitWrapperReservationReconciliation {
        let raw_report = attempt_reserve_once(&self.quarantine, wrapper_name);
        PendingActiveReblitWrapperReservationReconciliation {
            authenticated_pre: self,
            authenticated_pre_projection: projection,
            raw_report,
        }
    }
}

impl PendingActiveReblitWrapperReservationReconciliation {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn reconcile(
        self,
        installation: &crate::Installation,
        record: &TransitionRecord,
    ) -> ActiveReblitWrapperReservationReconciliation {
        let Self {
            authenticated_pre,
            authenticated_pre_projection,
            raw_report: _raw_report,
        } = self;
        let baseline_projection =
            match ProjectedActiveReblitWrapperReservationNamespace::capture(&authenticated_pre, record) {
                Ok(projection)
                    if projection.layout() == ActiveReblitWrapperReservationLayout::Absent
                        && projection == authenticated_pre_projection =>
                {
                    projection
                }
                Ok(_) | Err(_) => return ActiveReblitWrapperReservationReconciliation::Ambiguous,
            };

        run_before_reconciliation_capture();
        let fresh = match capture_snapshot(installation, record) {
            Ok(fresh) => fresh,
            Err(_) => return ActiveReblitWrapperReservationReconciliation::Ambiguous,
        };
        if fresh.fingerprint() == authenticated_pre.fingerprint() {
            return ActiveReblitWrapperReservationReconciliation::NotApplied;
        }
        let fresh_projection = match ProjectedActiveReblitWrapperReservationNamespace::capture(&fresh, record) {
            Ok(projection) => projection,
            Err(_) => return ActiveReblitWrapperReservationReconciliation::Ambiguous,
        };
        if baseline_projection
            .require_absent_to_reserved(&fresh_projection)
            .is_err()
        {
            return ActiveReblitWrapperReservationReconciliation::Ambiguous;
        }
        ActiveReblitWrapperReservationReconciliation::RestartRequired
    }
}

fn require_active_reblit_candidate_preserve(
    record: &TransitionRecord,
) -> Result<i32, ActiveReblitWrapperReservationCaptureError> {
    if record.operation != Operation::ActiveReblit || record.phase != Phase::CandidatePreserveIntent {
        return Err(ActiveReblitWrapperReservationCaptureError::WrongRoute);
    }
    record
        .previous
        .id
        .ok_or(ActiveReblitWrapperReservationCaptureError::PreviousStateMissing)
}

/// The exact name the forward reservation would have allocated. Index zero is
/// the only free one: no wrapper for this state exists in the absent prefix,
/// and the state is part of the name.
fn reservation_name(
    record: &TransitionRecord,
    state: i32,
) -> Result<String, ActiveReblitWrapperReservationCaptureError> {
    let token = record.previous.tree_token.as_str();
    if token.is_empty() {
        return Err(ActiveReblitWrapperReservationCaptureError::WrongReservationName);
    }
    Ok(format!("replaced-active-reblit-wrapper-{state}-{token}-0"))
}

fn attempt_reserve_once(parent: &std::fs::File, wrapper_name: &CString) -> io::Result<()> {
    #[cfg(test)]
    let injected = begin_reservation_attempt();
    #[cfg(not(test))]
    let _injected = begin_reservation_attempt();
    run_before_reservation_attempt();
    #[cfg(test)]
    let apply = !matches!(
        injected,
        Some(
            ActiveReblitWrapperReservationFault::ErrorWithoutApply
                | ActiveReblitWrapperReservationFault::SuccessWithoutApply
        )
    );
    #[cfg(not(test))]
    let apply = true;

    let kernel_result = apply.then(|| mkdirat_once(parent, wrapper_name, 0o700));
    #[cfg(test)]
    let result = match (injected, kernel_result) {
        (Some(ActiveReblitWrapperReservationFault::ErrorWithoutApply), None) => {
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        }
        (Some(ActiveReblitWrapperReservationFault::SuccessWithoutApply), None) => Ok(()),
        (Some(ActiveReblitWrapperReservationFault::ErrorAfterApply), Some(Ok(()))) => {
            Err(io::Error::from_raw_os_error(nix::libc::EINTR))
        }
        (_, Some(result)) => result,
        _ => unreachable!("ActiveReblit reservation fault injection has a complete result matrix"),
    };
    #[cfg(not(test))]
    let result = kernel_result.expect("production always invokes one reservation attempt");
    result
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ActiveReblitWrapperReservationCaptureError {
    #[error("reservation evidence requires an ActiveReblit CandidatePreserveIntent record")]
    WrongRoute,
    #[error("the previous state ID required by the ActiveReblit reservation is absent")]
    PreviousStateMissing,
    #[error("the exact ActiveReblit reservation name is not representable")]
    WrongReservationName,
    #[error("the namespace is not an exact ActiveReblit reservation prefix")]
    NotReservationLayout,
    #[error("the ActiveReblit reservation moved from {before:?} to {after:?}")]
    NotAbsentToReserved {
        before: ActiveReblitWrapperReservationLayout,
        after: ActiveReblitWrapperReservationLayout,
    },
    #[error("a non-reservation invariant changed across the ActiveReblit reservation attempt")]
    InvariantChanged,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitWrapperReservationFault {
    ErrorWithoutApply,
    SuccessWithoutApply,
    ErrorAfterApply,
}

#[cfg(test)]
std::thread_local! {
    static RESERVATION_FAULT: std::cell::Cell<Option<ActiveReblitWrapperReservationFault>> =
        const { std::cell::Cell::new(None) };
    static RESERVATION_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static BEFORE_RESERVATION_ATTEMPT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_RECONCILIATION_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_active_reblit_wrapper_reservation_fault(fault: ActiveReblitWrapperReservationFault) {
    RESERVATION_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(fault)).is_none(),
            "ActiveReblit reservation fault already armed"
        );
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_wrapper_reservation_attempt(hook: impl FnOnce() + 'static) {
    BEFORE_RESERVATION_ATTEMPT.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_wrapper_reservation_reconciliation_capture(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_RECONCILIATION_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn reset_active_reblit_wrapper_reservation_attempt_count() {
    RESERVATION_ATTEMPTS.with(|count| count.set(0));
    RESERVATION_FAULT.with(|slot| slot.set(None));
    BEFORE_RESERVATION_ATTEMPT.with(|slot| slot.borrow_mut().take());
    BEFORE_RECONCILIATION_CAPTURE.with(|slot| slot.borrow_mut().take());
}

#[cfg(test)]
pub(in crate::client) fn active_reblit_wrapper_reservation_attempt_count() -> usize {
    RESERVATION_ATTEMPTS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn begin_reservation_attempt() -> Option<ActiveReblitWrapperReservationFault> {
    RESERVATION_ATTEMPTS.with(|count| count.set(count.get().saturating_add(1)));
    RESERVATION_FAULT.with(std::cell::Cell::take)
}

#[cfg(test)]
fn run_before_reservation_attempt() {
    BEFORE_RESERVATION_ATTEMPT.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
fn run_before_reconciliation_capture() {
    BEFORE_RECONCILIATION_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn begin_reservation_attempt() -> Option<std::convert::Infallible> {
    None
}

#[cfg(not(test))]
fn run_before_reservation_attempt() {}

#[cfg(not(test))]
fn run_before_reconciliation_capture() {}
