//! One-shot mutation boundary for retained reverse-exchange parents.
//!
//! Calling this boundary consumes the opaque parent capabilities and makes
//! exactly one direction-neutral exchange attempt. The raw syscall report is
//! retained only as diagnostic evidence behind a pending-reconciliation
//! typestate: success does not prove application, and failure does not prove
//! non-application. The private reconciliation child consumes that typestate
//! and classifies only a fresh authenticated namespace.

mod reconciliation;

use crate::transition_identity::{Error, exchange_retained_usr_once};

use super::RetainedReverseExchangeParents;

#[cfg(test)]
pub(in crate::client) use reconciliation::arm_before_reverse_exchange_reconciliation_capture;
pub(in crate::client::startup_reconciliation::activation_namespace) use reconciliation::{
    AppliedReverseExchangeReconciliation, DurableAppliedReverseExchangeReconciliation, ReverseExchangeReconciliation,
};

/// Consumed parent capabilities plus an uninterpreted raw syscall report.
///
/// The fields intentionally remain private. In particular, this value has no
/// descriptor getter and no success/failure accessor. A later reconciliation
/// step must consume the whole value and compare a fresh authenticated
/// namespace before it can expose any semantic result.
#[must_use = "a reverse exchange attempt must be reconciled against a fresh namespace"]
#[allow(dead_code)] // consumed by the later rollback-reverse executor
pub(in crate::client::startup_reconciliation::activation_namespace) struct PendingReverseExchangeReconciliation {
    parents: RetainedReverseExchangeParents,
    raw_report: Result<(), Error>,
}

impl RetainedReverseExchangeParents {
    /// Consume both retained parent capabilities into exactly one exchange
    /// attempt. The returned value deliberately does not classify the effect.
    #[allow(dead_code)] // invoked by the later rollback-reverse executor
    pub(in crate::client::startup_reconciliation::activation_namespace) fn attempt_usr_exchange_once(
        self,
    ) -> PendingReverseExchangeReconciliation {
        let diagnostic_usr_path = self.root_path.join("usr");
        let raw_report = exchange_retained_usr_once(&self.staging, &self.root, &diagnostic_usr_path);
        PendingReverseExchangeReconciliation {
            parents: self,
            raw_report,
        }
    }
}

#[cfg(test)]
mod tests;
