//! Independent retained namespace proof for the rollback-decision boundary.
//!
//! The diagnostic inventory is deliberately not reused as mutation authority.
//! This proof performs its own journal/namespace sandwich, retains both exact
//! inventories, and requires a fresh matching capture immediately before the
//! caller may persist `RollbackDecided`.

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
    policy::{NamespacePolicyConflict, UsrExchangeLayout, assess_snapshot_layout},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackDecisionNamespaceInspection {
    before: NamespaceSnapshot,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackDecisionNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    layout: UsrExchangeLayout,
}

impl UsrRollbackDecisionNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackDecisionNamespaceError> {
        require_exact_journal(journal, expected)?;
        let before = capture_snapshot(installation, expected)?;
        Ok(Self { before })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<UsrRollbackDecisionNamespaceProof, UsrRollbackDecisionNamespaceError> {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        let before_layout = exchange_layout(expected, &self.before)?;
        let after_layout = exchange_layout(expected, &after)?;
        if before_layout != after_layout {
            return Err(UsrRollbackDecisionNamespaceError::LayoutChanged);
        }
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackDecisionNamespaceProof {
            before: self.before,
            after,
            layout: after_layout,
        })
    }
}

impl UsrRollbackDecisionNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn layout(&self) -> UsrExchangeLayout {
        self.layout
    }

    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackDecisionNamespaceError> {
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
) -> Result<UsrExchangeLayout, UsrRollbackDecisionNamespaceError> {
    assess_snapshot_layout(record, snapshot)?
        .usr_exchange_layout()
        .ok_or(UsrRollbackDecisionNamespaceError::NotExchangeLayout)
}

fn require_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: UsrExchangeLayout,
) -> Result<(), UsrRollbackDecisionNamespaceError> {
    let actual = exchange_layout(record, snapshot)?;
    if actual == expected {
        Ok(())
    } else {
        Err(UsrRollbackDecisionNamespaceError::LayoutChanged)
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackDecisionNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackDecisionNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackDecisionNamespaceError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackDecisionNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackDecisionNamespaceError {
    #[error("capture or revalidate the exact activation namespace")]
    Capture(#[from] CaptureError),
    #[error("assess the exact activation namespace against the journal phase")]
    Policy(#[from] NamespacePolicyConflict),
    #[error("read the retained canonical transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical transition journal changed during rollback-decision proof")]
    JournalChanged,
    #[error("the activation namespace changed during rollback-decision proof")]
    NamespaceChanged,
    #[error("the exact activation layout is not a pre/post `/usr` exchange layout")]
    NotExchangeLayout,
    #[error("the exact pre/post `/usr` exchange layout changed during rollback-decision proof")]
    LayoutChanged,
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FRESH_NAMESPACE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[allow(dead_code)] // armed by focused rollback-decision race contracts
pub(in crate::client) fn arm_before_usr_rollback_decision_fresh_namespace_capture(hook: impl FnOnce() + 'static) {
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
