//! Single-attempt descriptor-relative ActiveReblit cleanup exchange.

mod reconciliation;

use std::io;

use crate::linux_fs::renameat2_exchange_once;

use super::{PreparedActiveReblitCommitCleanupExchange, pre_exchange_safety::require_exact_apply};

#[cfg(test)]
pub(in crate::client) use reconciliation::arm_before_active_reblit_commit_cleanup_reconciliation_capture;
pub(in crate::client::startup_reconciliation) use reconciliation::ActiveReblitCommitCleanupExchangeReconciliation;

/// Consumed descriptors plus an uninterpreted raw syscall report. Only a
/// fresh exact namespace capture may classify the semantic outcome.
#[must_use = "an ActiveReblit cleanup exchange attempt must be reconciled"]
pub(in crate::client::startup_reconciliation) struct PendingActiveReblitCommitCleanupExchangeReconciliation
{
    parents: super::RetainedActiveReblitCommitCleanupParents,
    authenticated_apply: super::NamespaceSnapshot,
    authenticated_projection: super::ProjectedActiveReblitCommitCleanupNamespace,
    raw_report: io::Result<()>,
}

impl PreparedActiveReblitCommitCleanupExchange {
    /// Revalidate exact Apply evidence and issue exactly one exchange.
    /// Consuming `self` prevents retry in this entry.
    pub(in crate::client::startup_reconciliation) fn attempt_exchange_once(
        self,
        installation: &crate::Installation,
        record: &crate::transition_journal::TransitionRecord,
    ) -> Result<
        PendingActiveReblitCommitCleanupExchangeReconciliation,
        super::ActiveReblitCommitCleanupEffectError,
    > {
        require_exact_apply(
            installation,
            record,
            &self.parents,
            &self.final_apply,
            &self.final_projection,
        )?;
        let Self {
            parents,
            final_apply,
            final_projection,
        } = self;
        let raw_report = attempt_raw_exchange_once(&parents);
        Ok(PendingActiveReblitCommitCleanupExchangeReconciliation {
            parents,
            authenticated_apply: final_apply,
            authenticated_projection: final_projection,
            raw_report,
        })
    }
}

#[cfg(not(test))]
fn attempt_raw_exchange_once(parents: &super::RetainedActiveReblitCommitCleanupParents) -> io::Result<()> {
    renameat2_exchange_once(
        &parents.roots,
        c"staging",
        &parents.quarantine,
        &parents.target_name,
    )
}

#[cfg(test)]
fn attempt_raw_exchange_once(parents: &super::RetainedActiveReblitCommitCleanupParents) -> io::Result<()> {
    let injected = begin_exchange_attempt();
    let apply = !matches!(
        injected,
        Some(
            ActiveReblitCommitCleanupExchangeFault::ErrorWithoutApply
                | ActiveReblitCommitCleanupExchangeFault::SuccessWithoutApply
        )
    );
    let kernel_result = apply.then(|| {
        renameat2_exchange_once(
            &parents.roots,
            c"staging",
            &parents.quarantine,
            &parents.target_name,
        )
    });
    match (injected, kernel_result) {
        (Some(ActiveReblitCommitCleanupExchangeFault::ErrorWithoutApply), None) => {
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        }
        (Some(ActiveReblitCommitCleanupExchangeFault::SuccessWithoutApply), None) => Ok(()),
        (Some(ActiveReblitCommitCleanupExchangeFault::ErrorAfterApply), Some(Ok(()))) => {
            Err(io::Error::from_raw_os_error(nix::libc::EINTR))
        }
        (_, Some(result)) => result,
        _ => unreachable!("ActiveReblit cleanup exchange fault matrix is complete"),
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitCommitCleanupExchangeFault {
    ErrorWithoutApply,
    SuccessWithoutApply,
    ErrorAfterApply,
}

#[cfg(test)]
std::thread_local! {
    static EXCHANGE_FAULT: std::cell::Cell<Option<ActiveReblitCommitCleanupExchangeFault>> =
        const { std::cell::Cell::new(None) };
    static EXCHANGE_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(in crate::client) fn arm_active_reblit_commit_cleanup_exchange_fault(
    fault: ActiveReblitCommitCleanupExchangeFault,
) {
    EXCHANGE_FAULT.with(|slot| {
        assert!(slot.replace(Some(fault)).is_none(), "cleanup exchange fault already armed");
    });
}

#[cfg(test)]
pub(in crate::client) fn reset_active_reblit_commit_cleanup_exchange_attempt_count() {
    EXCHANGE_ATTEMPTS.with(|count| count.set(0));
    EXCHANGE_FAULT.with(|slot| slot.set(None));
}

#[cfg(test)]
pub(in crate::client) fn active_reblit_commit_cleanup_exchange_attempt_count() -> usize {
    EXCHANGE_ATTEMPTS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn begin_exchange_attempt() -> Option<ActiveReblitCommitCleanupExchangeFault> {
    EXCHANGE_ATTEMPTS.with(|count| {
        count.set(count.get().checked_add(1).expect("cleanup exchange attempt overflow"));
    });
    EXCHANGE_FAULT.with(std::cell::Cell::take)
}
