//! Independent retained namespace proof for journal-only rollback routing.
//!
//! The diagnostic inventory is never reused as persistence authority. This
//! proof performs its own journal/namespace sandwich, retains both exact
//! inventories, and requires a fresh matching capture immediately before the
//! caller may persist the first unresolved rollback intent.

use crate::{
    Installation,
    transition_journal::{Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    capture::{CaptureError, NamespaceSnapshot, TreeLocation, capture_snapshot},
    policy::{NamespacePolicyConflict, UsrExchangeLayout, assess_snapshot_layout},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackResumeRouteNamespaceInspection {
    before: NamespaceSnapshot,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackResumeRouteNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    layout: UsrExchangeLayout,
}

impl UsrRollbackResumeRouteNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackResumeRouteNamespaceError> {
        require_exact_journal(journal, expected)?;
        let before = capture_snapshot(installation, expected)?;
        Ok(Self { before })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<UsrRollbackResumeRouteNamespaceProof, UsrRollbackResumeRouteNamespaceError> {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        let before_layout = exchange_layout(expected, &self.before)?;
        let after_layout = exchange_layout(expected, &after)?;
        if before_layout != after_layout {
            return Err(UsrRollbackResumeRouteNamespaceError::LayoutChanged);
        }
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackResumeRouteNamespaceProof {
            before: self.before,
            after,
            layout: after_layout,
        })
    }
}

impl UsrRollbackResumeRouteNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn layout(&self) -> UsrExchangeLayout {
        self.layout
    }

    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackResumeRouteNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_layout(expected, &self.before, self.layout)?;
        require_layout(expected, &self.after, self.layout)?;
        require_exact_journal(journal, expected)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_layout(expected, &fresh, self.layout)?;

        require_exact_journal(journal, expected)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn exchange_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<UsrExchangeLayout, UsrRollbackResumeRouteNamespaceError> {
    if record.phase == Phase::UsrRestored
        && snapshot
            .wrappers()
            .any(|wrapper| wrapper.role == TreeLocation::TransitionQuarantine)
    {
        return Err(UsrRollbackResumeRouteNamespaceError::PrematureTransitionQuarantine);
    }
    assess_snapshot_layout(record, snapshot)?
        .usr_exchange_layout()
        .ok_or(UsrRollbackResumeRouteNamespaceError::NotExchangeLayout)
}

fn require_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: UsrExchangeLayout,
) -> Result<(), UsrRollbackResumeRouteNamespaceError> {
    if exchange_layout(record, snapshot)? == expected {
        Ok(())
    } else {
        Err(UsrRollbackResumeRouteNamespaceError::LayoutChanged)
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackResumeRouteNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackResumeRouteNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackResumeRouteNamespaceError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackResumeRouteNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackResumeRouteNamespaceError {
    #[error("capture or revalidate the exact rollback-resume namespace")]
    Capture(#[from] CaptureError),
    #[error("assess the exact rollback-resume namespace against the journal phase")]
    Policy(#[from] NamespacePolicyConflict),
    #[error("read the retained canonical transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical transition journal changed during rollback-resume proof")]
    JournalChanged,
    #[error("the rollback-resume activation namespace changed during proof")]
    NamespaceChanged,
    #[error("the exact rollback-resume layout is not a pre/post `/usr` exchange layout")]
    NotExchangeLayout,
    #[error("the exact pre/post `/usr` exchange layout changed during rollback-resume proof")]
    LayoutChanged,
    #[error("the transition quarantine wrapper exists before CandidatePreserveIntent was persisted")]
    PrematureTransitionQuarantine,
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FRESH_NAMESPACE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_resume_route_fresh_namespace_capture(hook: impl FnOnce() + 'static) {
    BEFORE_FRESH_NAMESPACE_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_fresh_namespace_capture() {
    BEFORE_FRESH_NAMESPACE_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_fresh_namespace_capture() {}
