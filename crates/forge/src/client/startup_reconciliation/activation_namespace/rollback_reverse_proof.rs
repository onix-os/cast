//! Independent retained namespace proof for one persisted `/usr` reversal.
//!
//! Admission through this proof is read-only. It authenticates an exact
//! `ReverseExchangeIntent` namespace as either POST (the reverse still needs
//! applying) or PRE (only the future durability suffix remains). Only its
//! private child can consume the resulting opaque effect evidence into the
//! one-shot exchange boundary; no descriptor, sync, or persistence operation
//! is exposed.

mod effect_reconciliation;

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    capture::{
        CaptureError, NamespaceSnapshot, ProjectedReverseNamespace, RetainedReverseExchangeParents,
        ReverseExchangeCaptureError, capture_snapshot,
    },
    policy::{NamespacePolicyConflict, UsrExchangeLayout, assess_snapshot_layout},
};

#[cfg(test)]
pub(in crate::client) use effect_reconciliation::arm_before_usr_rollback_reverse_effect_final_namespace_capture;
pub(in crate::client::startup_reconciliation) use effect_reconciliation::{
    UsrRollbackReverseAlreadySatisfiedNamespace, UsrRollbackReverseAppliedNamespace,
    UsrRollbackReverseNamespaceApplyReconciliation,
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackReverseNamespaceInspection {
    before: NamespaceSnapshot,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackReverseNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    projection: ProjectedReverseNamespace,
    parents: RetainedReverseExchangeParents,
    layout: UsrExchangeLayout,
}

/// Opaque normalized namespace evidence transferred into the future effect
/// lease. The projection and retained parent descriptors deliberately have no
/// accessor at this layer.
#[allow(dead_code)] // consumed by the later reverse-effect checkpoint
pub(in crate::client::startup_reconciliation) struct UsrRollbackReverseNamespaceEffectEvidence {
    baseline: NamespaceSnapshot,
    projection: ProjectedReverseNamespace,
    parents: RetainedReverseExchangeParents,
    layout: UsrExchangeLayout,
}

impl UsrRollbackReverseNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackReverseNamespaceError> {
        require_exact_journal(journal, expected)?;
        let before = capture_snapshot(installation, expected)?;
        Ok(Self { before })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<UsrRollbackReverseNamespaceProof, UsrRollbackReverseNamespaceError> {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        let before_layout = exchange_layout(expected, &self.before)?;
        let after_layout = exchange_layout(expected, &after)?;
        if before_layout != after_layout {
            return Err(UsrRollbackReverseNamespaceError::LayoutChanged);
        }
        let before_projection = ProjectedReverseNamespace::capture(&self.before, expected)?;
        let projection = ProjectedReverseNamespace::capture(&after, expected)?;
        if before_projection != projection {
            return Err(UsrRollbackReverseNamespaceError::ProjectionChanged);
        }
        let parents = RetainedReverseExchangeParents::capture(&after, expected)?;
        parents.revalidate_value_identity(installation)?;
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackReverseNamespaceProof {
            before: self.before,
            after,
            projection,
            parents,
            layout: after_layout,
        })
    }
}

impl UsrRollbackReverseNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn layout(&self) -> UsrExchangeLayout {
        self.layout
    }

    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackReverseNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_projection(expected, &self.before, &self.projection)?;
        require_projection(expected, &self.after, &self.projection)?;
        self.parents.revalidate_value_identity(installation)?;
        require_layout(expected, &self.before, self.layout)?;
        require_layout(expected, &self.after, self.layout)?;
        require_exact_journal(journal, expected)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_projection(expected, &fresh, &self.projection)?;
        require_layout(expected, &fresh, self.layout)?;

        require_exact_journal(journal, expected)?;
        self.parents.revalidate_value_identity(installation)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    /// Consume the exact proof after authority-level binding-first
    /// revalidation. The final exact stable snapshot crosses privately as the
    /// retained baseline; the duplicate first snapshot is dropped.
    pub(in crate::client::startup_reconciliation) fn into_effect_evidence(
        self,
        expected_layout: UsrExchangeLayout,
    ) -> Result<UsrRollbackReverseNamespaceEffectEvidence, UsrRollbackReverseNamespaceError> {
        if self.layout != expected_layout || self.projection.layout() != expected_layout {
            return Err(UsrRollbackReverseNamespaceError::LayoutChanged);
        }
        Ok(UsrRollbackReverseNamespaceEffectEvidence {
            baseline: self.after,
            projection: self.projection,
            parents: self.parents,
            layout: self.layout,
        })
    }
}

fn require_projection(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: &ProjectedReverseNamespace,
) -> Result<(), UsrRollbackReverseNamespaceError> {
    if ProjectedReverseNamespace::capture(snapshot, record)? == *expected {
        Ok(())
    } else {
        Err(UsrRollbackReverseNamespaceError::ProjectionChanged)
    }
}

fn exchange_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<UsrExchangeLayout, UsrRollbackReverseNamespaceError> {
    assess_snapshot_layout(record, snapshot)?
        .usr_exchange_layout()
        .ok_or(UsrRollbackReverseNamespaceError::NotExchangeLayout)
}

fn require_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: UsrExchangeLayout,
) -> Result<(), UsrRollbackReverseNamespaceError> {
    if exchange_layout(record, snapshot)? == expected {
        Ok(())
    } else {
        Err(UsrRollbackReverseNamespaceError::LayoutChanged)
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackReverseNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackReverseNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackReverseNamespaceError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackReverseNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackReverseNamespaceError {
    #[error("capture or revalidate the exact rollback-reverse namespace")]
    Capture(#[from] CaptureError),
    #[error("assess the exact rollback-reverse namespace against the journal phase")]
    Policy(#[from] NamespacePolicyConflict),
    #[error("read the retained canonical transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical transition journal changed during rollback-reverse proof")]
    JournalChanged,
    #[error("the rollback-reverse activation namespace changed during proof")]
    NamespaceChanged,
    #[error("capture the exact normalized rollback-reverse namespace")]
    Projection(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("the normalized rollback-reverse namespace changed during proof")]
    ProjectionChanged,
    #[error("the exact rollback-reverse layout is not a pre/post `/usr` exchange layout")]
    NotExchangeLayout,
    #[error("the exact pre/post `/usr` exchange layout changed during rollback-reverse proof")]
    LayoutChanged,
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}

impl From<ReverseExchangeCaptureError> for UsrRollbackReverseNamespaceError {
    fn from(source: ReverseExchangeCaptureError) -> Self {
        Self::Projection(Box::new(source))
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FRESH_NAMESPACE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_reverse_fresh_namespace_capture(hook: impl FnOnce() + 'static) {
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
