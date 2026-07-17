//! One-attempt creation of an exact NewState quarantine target.
//!
//! The raw operation report is diagnostic only. The consumed PRE namespace
//! and its retained quarantine parent remain sealed behind a pending value
//! until a fresh full capture classifies the semantic result.

mod reconciliation;

use std::{ffi::CString, io};

use crate::linux_fs::mkdirat_once;

use super::{NamespaceSnapshot, ProjectedNewStateTargetCreateNamespace};

pub(in crate::client::startup_reconciliation::activation_namespace) use reconciliation::NewStateTargetCreateReconciliation;
#[cfg(test)]
pub(in crate::client) use reconciliation::arm_before_new_state_target_create_reconciliation_capture;

#[must_use = "a NewState target-creation attempt requires fresh semantic reconciliation"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct PendingNewStateTargetCreateReconciliation {
    pub(super) authenticated_pre: NamespaceSnapshot,
    pub(super) authenticated_pre_projection: ProjectedNewStateTargetCreateNamespace,
    pub(super) raw_report: io::Result<()>,
}

impl NamespaceSnapshot {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn attempt_new_state_target_create_once(
        self,
        target_name: &CString,
        projection: ProjectedNewStateTargetCreateNamespace,
    ) -> PendingNewStateTargetCreateReconciliation {
        let raw_report = attempt_create_once(&self.quarantine, target_name);
        PendingNewStateTargetCreateReconciliation {
            authenticated_pre: self,
            authenticated_pre_projection: projection,
            raw_report,
        }
    }
}

fn attempt_create_once(parent: &std::fs::File, target_name: &CString) -> io::Result<()> {
    #[cfg(test)]
    let injected = begin_create_attempt();
    #[cfg(not(test))]
    let _injected = begin_create_attempt();
    run_before_create_attempt();
    #[cfg(test)]
    let apply = !matches!(
        injected,
        Some(NewStateTargetCreateFault::ErrorWithoutApply | NewStateTargetCreateFault::SuccessWithoutApply)
    );
    #[cfg(not(test))]
    let apply = true;

    let kernel_result = apply.then(|| mkdirat_once(parent, target_name, 0o700));
    #[cfg(test)]
    let result = match (injected, kernel_result) {
        (Some(NewStateTargetCreateFault::ErrorWithoutApply), None) => Err(io::Error::from_raw_os_error(nix::libc::EIO)),
        (Some(NewStateTargetCreateFault::SuccessWithoutApply), None) => Ok(()),
        (Some(NewStateTargetCreateFault::ErrorAfterApply), Some(Ok(()))) => {
            Err(io::Error::from_raw_os_error(nix::libc::EINTR))
        }
        (_, Some(result)) => result,
        _ => unreachable!("NewState target-creation fault injection has a complete result matrix"),
    };
    #[cfg(not(test))]
    let result = kernel_result.expect("production always invokes one target-creation attempt");
    result
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum NewStateTargetCreateFault {
    ErrorWithoutApply,
    SuccessWithoutApply,
    ErrorAfterApply,
}

#[cfg(test)]
std::thread_local! {
    static CREATE_FAULT: std::cell::Cell<Option<NewStateTargetCreateFault>> = const { std::cell::Cell::new(None) };
    static CREATE_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static BEFORE_CREATE_ATTEMPT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_new_state_target_create_fault(fault: NewStateTargetCreateFault) {
    CREATE_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(fault)).is_none(),
            "target-creation fault already armed"
        );
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_before_new_state_target_create_attempt(hook: impl FnOnce() + 'static) {
    BEFORE_CREATE_ATTEMPT.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(in crate::client) fn reset_new_state_target_create_attempt_count() {
    CREATE_ATTEMPTS.with(|count| count.set(0));
    CREATE_FAULT.with(|slot| slot.set(None));
    BEFORE_CREATE_ATTEMPT.with(|slot| slot.borrow_mut().take());
}

#[cfg(test)]
pub(in crate::client) fn new_state_target_create_attempt_count() -> usize {
    CREATE_ATTEMPTS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn begin_create_attempt() -> Option<NewStateTargetCreateFault> {
    CREATE_ATTEMPTS.with(|count| count.set(count.get().saturating_add(1)));
    CREATE_FAULT.with(std::cell::Cell::take)
}

#[cfg(test)]
fn run_before_create_attempt() {
    BEFORE_CREATE_ATTEMPT.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn begin_create_attempt() -> Option<std::convert::Infallible> {
    None
}

#[cfg(not(test))]
fn run_before_create_attempt() {}
