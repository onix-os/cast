//! Independent retained namespace proof for one persisted previous-restore.
//!
//! Admission through this proof is read-only. It authenticates an exact
//! `PreviousRestoreIntent` namespace as either `Archived` (the predecessor is
//! still in its roots slot and the compensating move has to run) or `Restored`
//! (the move already happened and only journal completion remains). The proof
//! retains both inventories and requires a fresh matching capture immediately
//! before the caller may consume it; it exposes no descriptor and performs no
//! move.

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
    policy::{NamespacePolicyConflict, PreviousRestoreLayout, assess_snapshot_layout},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackPreviousRestoreNamespaceInspection {
    before: NamespaceSnapshot,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackPreviousRestoreNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    layout: PreviousRestoreLayout,
}

/// Opaque normalized evidence handed to the consuming effect lease. The
/// retained snapshots deliberately have no accessor at this layer.
pub(in crate::client::startup_reconciliation) struct UsrRollbackPreviousRestoreNamespaceEffectEvidence {
    baseline: NamespaceSnapshot,
    layout: PreviousRestoreLayout,
}

impl UsrRollbackPreviousRestoreNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackPreviousRestoreNamespaceError> {
        require_exact_journal(journal, expected)?;
        let before = capture_snapshot(installation, expected)?;
        Ok(Self { before })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<UsrRollbackPreviousRestoreNamespaceProof, UsrRollbackPreviousRestoreNamespaceError> {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        let before_layout = restore_layout(expected, &self.before)?;
        let after_layout = restore_layout(expected, &after)?;
        if before_layout != after_layout {
            return Err(UsrRollbackPreviousRestoreNamespaceError::LayoutChanged);
        }
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackPreviousRestoreNamespaceProof {
            before: self.before,
            after,
            layout: after_layout,
        })
    }
}

impl UsrRollbackPreviousRestoreNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn layout(&self) -> PreviousRestoreLayout {
        self.layout
    }

    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackPreviousRestoreNamespaceError> {
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

    /// Consume the proof after authority-level binding-first revalidation. The
    /// final exact stable snapshot crosses privately as the retained baseline;
    /// the duplicate first snapshot is dropped.
    pub(in crate::client::startup_reconciliation) fn into_effect_evidence(
        self,
        expected_layout: PreviousRestoreLayout,
    ) -> Result<UsrRollbackPreviousRestoreNamespaceEffectEvidence, UsrRollbackPreviousRestoreNamespaceError> {
        if self.layout != expected_layout {
            return Err(UsrRollbackPreviousRestoreNamespaceError::LayoutChanged);
        }
        Ok(UsrRollbackPreviousRestoreNamespaceEffectEvidence {
            baseline: self.after,
            layout: self.layout,
        })
    }
}

impl UsrRollbackPreviousRestoreNamespaceEffectEvidence {
    /// Re-prove the retained baseline immediately before the move.
    pub(in crate::client::startup_reconciliation) fn require_stable(
        &self,
        installation: &Installation,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackPreviousRestoreNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.baseline.revalidate_retained()?;
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.baseline, &fresh)?;
        require_layout(expected, &fresh, self.layout)?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    /// Prove the namespace now presents the predecessor back in staging.
    ///
    /// This is the only observation that decides whether the move happened.
    /// A raw syscall result is deliberately not treated as the outcome.
    pub(in crate::client::startup_reconciliation) fn require_restored(
        &self,
        installation: &Installation,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackPreviousRestoreNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_layout(expected, &fresh, PreviousRestoreLayout::Restored)?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn restore_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<PreviousRestoreLayout, UsrRollbackPreviousRestoreNamespaceError> {
    assess_snapshot_layout(record, snapshot)?
        .previous_restore_layout()
        .ok_or(UsrRollbackPreviousRestoreNamespaceError::NotRestoreLayout)
}

fn require_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: PreviousRestoreLayout,
) -> Result<(), UsrRollbackPreviousRestoreNamespaceError> {
    if restore_layout(record, snapshot)? == expected {
        Ok(())
    } else {
        Err(UsrRollbackPreviousRestoreNamespaceError::LayoutChanged)
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackPreviousRestoreNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackPreviousRestoreNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackPreviousRestoreNamespaceError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackPreviousRestoreNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackPreviousRestoreNamespaceError {
    #[error("capture or revalidate the exact previous-restore namespace")]
    Capture(#[from] CaptureError),
    #[error("assess the exact previous-restore namespace against the journal phase")]
    Policy(#[from] NamespacePolicyConflict),
    #[error("read the retained canonical transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical transition journal changed during previous-restore proof")]
    JournalChanged,
    #[error("the previous-restore activation namespace changed during proof")]
    NamespaceChanged,
    #[error("the exact previous-restore layout is neither archived nor restored")]
    NotRestoreLayout,
    #[error("the exact previous-restore layout changed during proof")]
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
pub(in crate::client) fn arm_before_usr_rollback_previous_restore_fresh_namespace_capture(
    hook: impl FnOnce() + 'static,
) {
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
