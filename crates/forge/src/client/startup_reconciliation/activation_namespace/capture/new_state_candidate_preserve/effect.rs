//! One-shot no-replace move for retained NewState candidate parents.
//!
//! The raw syscall report is diagnostic only. It remains sealed behind a
//! pending-reconciliation value until a fresh namespace capture classifies the
//! actual effect.

mod reconciliation;

use std::io;

use crate::linux_fs::renameat2_noreplace_once;

use super::RetainedNewStateCandidatePreserveParents;

#[cfg(test)]
pub(in crate::client) use reconciliation::arm_before_new_state_candidate_preserve_move_reconciliation_capture;
pub(in crate::client::startup_reconciliation::activation_namespace) use reconciliation::{
    AppliedNewStateCandidatePreserveMoveReconciliation, NewStateCandidatePreserveMoveReconciliation,
};

#[must_use = "a NewState candidate move attempt must be reconciled against a fresh namespace"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct PendingNewStateCandidatePreserveMoveReconciliation
{
    parents: RetainedNewStateCandidatePreserveParents,
    raw_report: io::Result<()>,
}

impl RetainedNewStateCandidatePreserveParents {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn attempt_move_once(
        self,
    ) -> PendingNewStateCandidatePreserveMoveReconciliation {
        let raw_report = attempt_move_once(&self.staging, &self.target);
        PendingNewStateCandidatePreserveMoveReconciliation {
            parents: self,
            raw_report,
        }
    }
}

fn attempt_move_once(staging: &std::fs::File, target: &std::fs::File) -> io::Result<()> {
    #[cfg(test)]
    let injected = begin_move_attempt();
    #[cfg(not(test))]
    let _injected = begin_move_attempt();
    #[cfg(test)]
    let apply = !matches!(
        injected,
        Some(
            NewStateCandidatePreserveMoveFault::ErrorWithoutApply
                | NewStateCandidatePreserveMoveFault::SuccessWithoutApply
        )
    );
    #[cfg(not(test))]
    let apply = true;

    let kernel_result = apply.then(|| renameat2_noreplace_once(staging, c"usr", target, c"usr"));
    #[cfg(test)]
    let result = match (injected, kernel_result) {
        (Some(NewStateCandidatePreserveMoveFault::ErrorWithoutApply), None) => {
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        }
        (Some(NewStateCandidatePreserveMoveFault::SuccessWithoutApply), None) => Ok(()),
        (Some(NewStateCandidatePreserveMoveFault::ErrorAfterApply), Some(Ok(()))) => {
            Err(io::Error::from_raw_os_error(nix::libc::EINTR))
        }
        (_, Some(result)) => result,
        _ => unreachable!("NewState candidate-move fault injection has a complete result matrix"),
    };
    #[cfg(not(test))]
    let result = kernel_result.expect("production always invokes the one-shot candidate move");
    result
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum NewStateCandidatePreserveMoveFault {
    ErrorWithoutApply,
    SuccessWithoutApply,
    ErrorAfterApply,
}

#[cfg(test)]
std::thread_local! {
    static MOVE_FAULT: std::cell::Cell<Option<NewStateCandidatePreserveMoveFault>> = const { std::cell::Cell::new(None) };
    static MOVE_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(in crate::client) fn arm_new_state_candidate_preserve_move_fault(fault: NewStateCandidatePreserveMoveFault) {
    MOVE_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(fault)).is_none(),
            "candidate-move fault already armed"
        );
    });
}

#[cfg(test)]
pub(in crate::client) fn reset_new_state_candidate_preserve_move_attempt_count() {
    MOVE_ATTEMPTS.with(|count| count.set(0));
    MOVE_FAULT.with(|slot| slot.set(None));
}

#[cfg(test)]
pub(in crate::client) fn new_state_candidate_preserve_move_attempt_count() -> usize {
    MOVE_ATTEMPTS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn begin_move_attempt() -> Option<NewStateCandidatePreserveMoveFault> {
    MOVE_ATTEMPTS.with(|count| count.set(count.get().saturating_add(1)));
    MOVE_FAULT.with(std::cell::Cell::take)
}

#[cfg(not(test))]
fn begin_move_attempt() -> Option<std::convert::Infallible> {
    None
}
