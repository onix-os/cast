//! Independent namespace proof for the NewState terminal phases.
//!
//! Read-only. Retains both sides of its admission sandwich and requires a fresh
//! matching capture whenever the enclosing authority is revalidated.
//!
//! The accepted namespace is whatever the record's own phase says it should be,
//! assessed through the shared policy rather than a hand-written topology. That
//! works because `policy::commit_layouts` is already record-driven: for a
//! predecessor of origin `ActiveState` it yields `{candidate: Live, previous:
//! Archived}` at `CommitDecided`, `CommitCleanupComplete` and `Complete` — which
//! is exactly the shape a NewState transition leaves behind once its candidate
//! is live and its predecessor is archived.
//!
//! Deliberately *not* built on `ProjectedActiveReblitCommitCleanupNamespace`:
//! that projection tracks a staging-wrapper identity through an exchange, which
//! is what makes the ActiveReblit terminal authorities large, and which NewState
//! never performs (`plans/future_impl.md` §1.1c).

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
    policy::{LayoutAlternative, NamespacePolicyConflict, assess_snapshot_layout},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct NewStateTerminalNamespaceInspection {
    before: NamespaceSnapshot,
}

/// Opaque proof that the namespace matches the record's expected terminal
/// layout. The retained snapshots have no accessor at this layer.
#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct NewStateTerminalNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    layout: LayoutAlternative,
}

impl NewStateTerminalNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<Self, NewStateTerminalNamespaceError> {
        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        let before = capture_snapshot(installation, expected)?;
        assess_snapshot_layout(expected, &before)?;
        Ok(Self { before })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<NewStateTerminalNamespaceProof, NewStateTerminalNamespaceError> {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        let before_layout = assess_snapshot_layout(expected, &self.before)?;
        let layout = assess_snapshot_layout(expected, &after)?;
        if before_layout != layout {
            return Err(NewStateTerminalNamespaceError::LayoutChanged);
        }
        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(NewStateTerminalNamespaceProof {
            before: self.before,
            after,
            layout,
        })
    }
}

impl NewStateTerminalNamespaceProof {
    /// Repeat the full admission sandwich against a fresh capture. The layout
    /// must still be the one originally admitted.
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        expected: &TransitionRecord,
    ) -> Result<(), NewStateTerminalNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        let fresh = capture_snapshot(installation, expected)?;
        require_matching_fingerprints(&self.after, &fresh)?;
        if assess_snapshot_layout(expected, &fresh)? != self.layout {
            return Err(NewStateTerminalNamespaceError::LayoutChanged);
        }
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), NewStateTerminalNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(NewStateTerminalNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    expected: &TransitionRecord,
) -> Result<(), NewStateTerminalNamespaceError> {
    if !journal.has_record_store_binding(binding) {
        return Err(NewStateTerminalNamespaceError::JournalRecordBindingChanged);
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if !journal.has_record_binding(cast, binding, expected)? {
        return Err(NewStateTerminalNamespaceError::JournalRecordBindingChanged);
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum NewStateTerminalNamespaceError {
    #[error("the retained namespace changed during admission")]
    NamespaceChanged,
    #[error("the admitted terminal layout changed")]
    LayoutChanged,
    #[error("the retained journal record binding changed")]
    JournalRecordBindingChanged,
    #[error("namespace policy")]
    Policy(#[from] NamespacePolicyConflict),
    #[error("namespace capture")]
    Capture(#[from] CaptureError),
    #[error("installation")]
    Installation(#[from] crate::installation::Error),
    #[error("journal storage")]
    Storage(#[from] StorageError),
}
