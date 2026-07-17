//! One-attempt mode normalization of an exact retained NewState target.
//!
//! The raw chmod report is diagnostic only. The consumed PRE namespace and
//! its retained restrictive-residue descriptor remain sealed behind a pending
//! value until fresh full namespace capture classifies the semantic result.

mod reconciliation;

use std::io;

use crate::linux_fs::chmod_path_descriptor_once;

use super::{NamespaceSnapshot, ProjectedNewStateTargetNormalizeNamespace};

pub(in crate::client::startup_reconciliation::activation_namespace) use reconciliation::NewStateTargetNormalizeReconciliation;
#[cfg(test)]
pub(in crate::client) use reconciliation::arm_before_new_state_target_normalize_reconciliation_capture;

const PRIVATE_TARGET_MODE: u32 = 0o700;

#[must_use = "a NewState target-normalization attempt requires fresh semantic reconciliation"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct PendingNewStateTargetNormalizeReconciliation
{
    pub(super) authenticated_pre: NamespaceSnapshot,
    pub(super) authenticated_pre_projection: ProjectedNewStateTargetNormalizeNamespace,
    pub(super) raw_report: io::Result<()>,
}

impl NamespaceSnapshot {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn attempt_new_state_target_normalize_once(
        self,
        projection: ProjectedNewStateTargetNormalizeNamespace,
    ) -> PendingNewStateTargetNormalizeReconciliation {
        let retained_target = &self
            .new_state_target_residue
            .as_ref()
            .expect("prepared target-normalization namespace retains the exact restrictive residue")
            .directory;
        let raw_report = attempt_normalize_once(retained_target);
        PendingNewStateTargetNormalizeReconciliation {
            authenticated_pre: self,
            authenticated_pre_projection: projection,
            raw_report,
        }
    }
}

fn attempt_normalize_once(target: &std::fs::File) -> io::Result<()> {
    #[cfg(test)]
    let injected = begin_normalize_attempt();
    #[cfg(not(test))]
    let _injected = begin_normalize_attempt();
    run_before_normalize_attempt();
    #[cfg(test)]
    let apply = !matches!(
        injected,
        Some(NewStateTargetNormalizeFault::ErrorWithoutApply | NewStateTargetNormalizeFault::SuccessWithoutApply)
    );
    #[cfg(not(test))]
    let apply = true;

    let kernel_result = apply.then(|| chmod_path_descriptor_once(target, PRIVATE_TARGET_MODE));
    #[cfg(test)]
    let result = match (injected, kernel_result) {
        (Some(NewStateTargetNormalizeFault::ErrorWithoutApply), None) => {
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        }
        (Some(NewStateTargetNormalizeFault::SuccessWithoutApply), None) => Ok(()),
        (Some(NewStateTargetNormalizeFault::ErrorAfterApply), Some(Ok(()))) => {
            Err(io::Error::from_raw_os_error(nix::libc::EINTR))
        }
        (_, Some(result)) => result,
        _ => unreachable!("NewState target-normalization fault injection has a complete result matrix"),
    };
    #[cfg(not(test))]
    let result = kernel_result.expect("production always invokes one target-normalization attempt");
    result
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum NewStateTargetNormalizeFault {
    ErrorWithoutApply,
    SuccessWithoutApply,
    ErrorAfterApply,
}

#[cfg(test)]
std::thread_local! {
    static NORMALIZE_FAULT: std::cell::Cell<Option<NewStateTargetNormalizeFault>> = const { std::cell::Cell::new(None) };
    static NORMALIZE_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static BEFORE_NORMALIZE_ATTEMPT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_new_state_target_normalize_fault(fault: NewStateTargetNormalizeFault) {
    NORMALIZE_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(fault)).is_none(),
            "target-normalization fault already armed"
        );
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_target_normalize_attempt(hook: impl FnOnce() + 'static) {
    BEFORE_NORMALIZE_ATTEMPT.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn reset_new_state_target_normalize_attempt_count() {
    NORMALIZE_ATTEMPTS.with(|count| count.set(0));
    NORMALIZE_FAULT.with(|slot| slot.set(None));
    BEFORE_NORMALIZE_ATTEMPT.with(|slot| slot.borrow_mut().take());
}

#[cfg(test)]
pub(in crate::client) fn new_state_target_normalize_attempt_count() -> usize {
    NORMALIZE_ATTEMPTS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn begin_normalize_attempt() -> Option<NewStateTargetNormalizeFault> {
    NORMALIZE_ATTEMPTS.with(|count| count.set(count.get().saturating_add(1)));
    NORMALIZE_FAULT.with(std::cell::Cell::take)
}

#[cfg(test)]
fn run_before_normalize_attempt() {
    BEFORE_NORMALIZE_ATTEMPT.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn begin_normalize_attempt() -> Option<std::convert::Infallible> {
    None
}

#[cfg(not(test))]
fn run_before_normalize_attempt() {}
