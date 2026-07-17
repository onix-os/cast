//! Single-attempt wrapper exchange boundary for ActiveReblit preservation.

mod reconciliation;

use std::io;

use crate::linux_fs::renameat2_exchange_once;

use super::PreparedActiveReblitCandidatePreserveExchange;

pub(in crate::client) use reconciliation::arm_before_active_reblit_candidate_preserve_reconciliation_capture;
pub(in crate::client::startup_reconciliation::activation_namespace) use reconciliation::{
    ActiveReblitCandidatePreserveExchangeReconciliation, AppliedActiveReblitCandidatePreserveExchangeReconciliation,
};

/// Consumed descriptors plus an uninterpreted raw syscall report.
///
/// There is deliberately no report accessor. Only a fresh authenticated
/// namespace capture may classify the attempt.
#[must_use = "an ActiveReblit wrapper exchange attempt must be reconciled"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct PendingActiveReblitCandidatePreserveExchangeReconciliation
{
    parents: super::RetainedActiveReblitCandidatePreserveParents,
    authenticated_pre: super::NamespaceSnapshot,
    authenticated_projection: super::ProjectedActiveReblitCandidatePreserveNamespace,
    raw_report: io::Result<()>,
}

impl PreparedActiveReblitCandidatePreserveExchange {
    /// Revalidate the sealed PRE and make exactly one descriptor-relative
    /// exchange. Consuming `self` makes a retry impossible.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn attempt_exchange_once(
        self,
        installation: &crate::Installation,
        record: &crate::transition_journal::TransitionRecord,
    ) -> Result<
        PendingActiveReblitCandidatePreserveExchangeReconciliation,
        super::ActiveReblitCandidatePreserveEffectError,
    > {
        super::require_exact_pre(
            installation,
            record,
            &self.parents,
            &self.final_pre,
            &self.final_projection,
        )?;
        let Self {
            parents,
            final_pre,
            final_projection,
        } = self;
        let raw_report = attempt_raw_exchange_once(&parents);
        Ok(PendingActiveReblitCandidatePreserveExchangeReconciliation {
            parents,
            authenticated_pre: final_pre,
            authenticated_projection: final_projection,
            raw_report,
        })
    }
}

fn attempt_raw_exchange_once(parents: &super::RetainedActiveReblitCandidatePreserveParents) -> io::Result<()> {
    let injected = begin_exchange_attempt();
    let apply = !matches!(
        injected,
        Some(
            ActiveReblitCandidatePreserveExchangeFault::ErrorWithoutApply
                | ActiveReblitCandidatePreserveExchangeFault::SuccessWithoutApply
        )
    );
    let kernel_result =
        apply.then(|| renameat2_exchange_once(&parents.roots, c"staging", &parents.quarantine, &parents.target_name));
    match (injected, kernel_result) {
        (Some(ActiveReblitCandidatePreserveExchangeFault::ErrorWithoutApply), None) => {
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        }
        (Some(ActiveReblitCandidatePreserveExchangeFault::SuccessWithoutApply), None) => Ok(()),
        (Some(ActiveReblitCandidatePreserveExchangeFault::ErrorAfterApply), Some(Ok(()))) => {
            Err(io::Error::from_raw_os_error(nix::libc::EINTR))
        }
        (_, Some(result)) => result,
        _ => unreachable!("ActiveReblit wrapper-exchange fault matrix is complete"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitCandidatePreserveExchangeFault {
    ErrorWithoutApply,
    SuccessWithoutApply,
    ErrorAfterApply,
}

std::thread_local! {
    static EXCHANGE_FAULT: std::cell::Cell<Option<ActiveReblitCandidatePreserveExchangeFault>> =
        const { std::cell::Cell::new(None) };
    static EXCHANGE_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(in crate::client) fn arm_active_reblit_candidate_preserve_exchange_fault(
    fault: ActiveReblitCandidatePreserveExchangeFault,
) {
    EXCHANGE_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(fault)).is_none(),
            "ActiveReblit exchange fault already armed"
        );
    });
}

pub(in crate::client) fn reset_active_reblit_candidate_preserve_exchange_attempt_count() {
    EXCHANGE_ATTEMPTS.with(|count| count.set(0));
    EXCHANGE_FAULT.with(|slot| slot.set(None));
}

pub(in crate::client) fn active_reblit_candidate_preserve_exchange_attempt_count() -> usize {
    EXCHANGE_ATTEMPTS.with(std::cell::Cell::get)
}

fn begin_exchange_attempt() -> Option<ActiveReblitCandidatePreserveExchangeFault> {
    EXCHANGE_ATTEMPTS.with(|count| {
        count.set(
            count
                .get()
                .checked_add(1)
                .expect("ActiveReblit exchange attempt counter overflow"),
        );
    });
    EXCHANGE_FAULT.with(std::cell::Cell::take)
}
