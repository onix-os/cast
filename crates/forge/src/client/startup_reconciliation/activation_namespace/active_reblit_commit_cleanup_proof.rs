//! Descriptor-backed read-only proof for exact forward ActiveReblit commit
//! cleanup.
//!
//! `CommitDecided` admits only two namespace layouts. `Apply` retains the
//! exact post-`/usr`-exchange layout: the candidate is live, the corrupt
//! previous tree remains inside the fixed staging wrapper, and the journal-
//! reserved empty replacement remains in quarantine. `Finish` retains the
//! exact completed layout: the corrupt previous wrapper is quarantined and
//! the empty replacement occupies the fixed staging name.
//!
//! Both proofs own complete `NamespaceSnapshot`s, including retained roots,
//! quarantine, wrapper, tree, state-ID, and root-ABI descriptors. Admission
//! exposes no descriptor or mutation operation. The specialized effect child
//! consumes the appropriate proof to project the exact two-parent, two-wrapper
//! exchange capability without reopening a pathname.

use crate::{
    Installation,
    transition_journal::{
        Operation, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    active_reblit_boot_repair_started_error_classification::capture_error_is_structural,
    capture::{
        ActiveReblitCommitCleanupCaptureError, ActiveReblitCommitCleanupEffectError,
        ActiveReblitCommitCleanupLayout, CaptureError, NamespaceSnapshot,
        PendingActiveReblitCommitCleanupDurability, PreparedActiveReblitCommitCleanupExchange,
        RetainedActiveReblitCommitCleanupNamespace, capture_snapshot,
    },
    policy::{
        CandidatePlace, LayoutAlternative, NamespacePolicyConflict, PreviousPlace, assess_snapshot_layout,
    },
};

/// First half of the stable namespace sandwich.
#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct ActiveReblitCommitCleanupNamespaceInspection {
    before: RetainedActiveReblitCommitCleanupNamespace,
    layout: ActiveReblitCommitCleanupLayout,
}

/// Exact descriptor-backed post-exchange layout which authorizes one
/// wrapper exchange. This type intentionally implements neither `Clone` nor
/// `Copy`.
#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct ActiveReblitCommitCleanupApplyNamespaceProof {
    before: RetainedActiveReblitCommitCleanupNamespace,
    after: RetainedActiveReblitCommitCleanupNamespace,
}

/// Exact descriptor-backed completed layout which authorizes the
/// zero-exchange Finish durability suffix. This type intentionally implements
/// neither `Clone` nor `Copy`.
#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct ActiveReblitCommitCleanupFinishNamespaceProof {
    before: RetainedActiveReblitCommitCleanupNamespace,
    after: RetainedActiveReblitCommitCleanupNamespace,
}

/// Consuming descriptor projection accepted only by the specialized Apply
/// effect child.
pub(in crate::client::startup_reconciliation) struct ActiveReblitCommitCleanupApplyNamespaceEffectEvidence {
    before: RetainedActiveReblitCommitCleanupNamespace,
    after: RetainedActiveReblitCommitCleanupNamespace,
}

/// Consuming descriptor projection accepted only by the specialized
/// zero-exchange Finish durability path.
pub(in crate::client::startup_reconciliation) struct ActiveReblitCommitCleanupFinishNamespaceEffectEvidence {
    before: RetainedActiveReblitCommitCleanupNamespace,
    after: RetainedActiveReblitCommitCleanupNamespace,
}

/// Layout-specific result of the complete read-only namespace sandwich.
pub(in crate::client::startup_reconciliation) enum ActiveReblitCommitCleanupNamespaceProof {
    Apply(ActiveReblitCommitCleanupApplyNamespaceProof),
    Finish(ActiveReblitCommitCleanupFinishNamespaceProof),
}

impl ActiveReblitCommitCleanupNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<Self, ActiveReblitCommitCleanupNamespaceError> {
        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        let snapshot = capture_snapshot(installation, expected)?;
        let layout = exact_layout(expected, &snapshot)?;
        let before = RetainedActiveReblitCommitCleanupNamespace::capture(snapshot, expected)?;
        if before.layout() != layout {
            return Err(ActiveReblitCommitCleanupNamespaceError::LayoutChanged);
        }
        Ok(Self { before, layout })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<ActiveReblitCommitCleanupNamespaceProof, ActiveReblitCommitCleanupNamespaceError> {
        let after_snapshot = capture_snapshot(installation, expected)?;
        let after = RetainedActiveReblitCommitCleanupNamespace::capture(after_snapshot, expected)?;
        self.before.revalidate(expected)?;
        after.revalidate(expected)?;
        require_matching_fingerprints(&self.before, &after)?;
        require_retained_layout(&self.before, self.layout)?;
        require_retained_layout(&after, self.layout)?;
        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        installation.revalidate_mutable_namespace()?;

        Ok(match self.layout {
            ActiveReblitCommitCleanupLayout::Apply => {
                ActiveReblitCommitCleanupNamespaceProof::Apply(
                    ActiveReblitCommitCleanupApplyNamespaceProof {
                        before: self.before,
                        after,
                    },
                )
            }
            ActiveReblitCommitCleanupLayout::Finish => {
                ActiveReblitCommitCleanupNamespaceProof::Finish(
                    ActiveReblitCommitCleanupFinishNamespaceProof {
                        before: self.before,
                        after,
                    },
                )
            }
        })
    }
}

impl ActiveReblitCommitCleanupApplyNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<(), ActiveReblitCommitCleanupNamespaceError> {
        revalidate_proof(
            installation,
            journal,
            journal_record_binding,
            expected,
            &self.before,
            &self.after,
            ActiveReblitCommitCleanupLayout::Apply,
        )
    }

    pub(in crate::client::startup_reconciliation) fn into_effect_evidence(
        self,
    ) -> ActiveReblitCommitCleanupApplyNamespaceEffectEvidence {
        ActiveReblitCommitCleanupApplyNamespaceEffectEvidence {
            before: self.before,
            after: self.after,
        }
    }
}

impl ActiveReblitCommitCleanupFinishNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<(), ActiveReblitCommitCleanupNamespaceError> {
        revalidate_proof(
            installation,
            journal,
            journal_record_binding,
            expected,
            &self.before,
            &self.after,
            ActiveReblitCommitCleanupLayout::Finish,
        )
    }

    pub(in crate::client::startup_reconciliation) fn into_effect_evidence(
        self,
    ) -> ActiveReblitCommitCleanupFinishNamespaceEffectEvidence {
        ActiveReblitCommitCleanupFinishNamespaceEffectEvidence {
            before: self.before,
            after: self.after,
        }
    }
}

impl ActiveReblitCommitCleanupApplyNamespaceEffectEvidence {
    pub(in crate::client::startup_reconciliation) fn prepare_exchange(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<PreparedActiveReblitCommitCleanupExchange, ActiveReblitCommitCleanupEffectError> {
        self.before.revalidate(record)?;
        self.after.revalidate(record)?;
        if self.before.fingerprint() != self.after.fingerprint() {
            return Err(ActiveReblitCommitCleanupEffectError::FinalNamespaceChanged);
        }
        self.after.prepare_exchange(installation, record)
    }
}

impl ActiveReblitCommitCleanupFinishNamespaceEffectEvidence {
    pub(in crate::client::startup_reconciliation) fn into_durability(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<PendingActiveReblitCommitCleanupDurability, ActiveReblitCommitCleanupEffectError> {
        self.before.revalidate(record)?;
        self.after.revalidate(record)?;
        if self.before.fingerprint() != self.after.fingerprint() {
            return Err(ActiveReblitCommitCleanupEffectError::FinalNamespaceChanged);
        }
        self.after.into_finish_durability(installation, record)
    }
}

fn revalidate_proof(
    installation: &Installation,
    journal: &TransitionJournalStore,
    journal_record_binding: &TransitionJournalRecordBinding,
    expected: &TransitionRecord,
    before: &RetainedActiveReblitCommitCleanupNamespace,
    after: &RetainedActiveReblitCommitCleanupNamespace,
    layout: ActiveReblitCommitCleanupLayout,
) -> Result<(), ActiveReblitCommitCleanupNamespaceError> {
    require_exact_journal(installation, journal, journal_record_binding, expected)?;
    installation.revalidate_mutable_namespace()?;
    before.revalidate(expected)?;
    after.revalidate(expected)?;
    require_matching_fingerprints(before, after)?;
    require_retained_layout(before, layout)?;
    require_retained_layout(after, layout)?;

    run_before_fresh_namespace_capture();
    let fresh_snapshot = capture_snapshot(installation, expected)?;
    let fresh = RetainedActiveReblitCommitCleanupNamespace::capture(fresh_snapshot, expected)?;
    fresh.revalidate(expected)?;
    require_matching_fingerprints(before, &fresh)?;
    require_retained_layout(&fresh, layout)?;

    require_exact_journal(installation, journal, journal_record_binding, expected)?;
    before.revalidate(expected)?;
    after.revalidate(expected)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn exact_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<ActiveReblitCommitCleanupLayout, ActiveReblitCommitCleanupNamespaceError> {
    require_exact_source(record)?;
    let layout = assess_snapshot_layout(record, snapshot)?;
    classify_layout(layout).ok_or(ActiveReblitCommitCleanupNamespaceError::UnsupportedLayout)
}

fn classify_layout(layout: LayoutAlternative) -> Option<ActiveReblitCommitCleanupLayout> {
    match (layout.candidate, layout.previous) {
        (CandidatePlace::Live, PreviousPlace::Staging) => Some(ActiveReblitCommitCleanupLayout::Apply),
        (CandidatePlace::Live, PreviousPlace::ActiveReblitWrapper) => {
            Some(ActiveReblitCommitCleanupLayout::Finish)
        }
        _ => None,
    }
}

fn require_exact_source(record: &TransitionRecord) -> Result<(), ActiveReblitCommitCleanupNamespaceError> {
    if record.operation == Operation::ActiveReblit
        && record.phase == Phase::CommitDecided
        && record.rollback.is_none()
    {
        Ok(())
    } else {
        Err(ActiveReblitCommitCleanupNamespaceError::WrongSource)
    }
}

fn require_retained_layout(
    retained: &RetainedActiveReblitCommitCleanupNamespace,
    expected: ActiveReblitCommitCleanupLayout,
) -> Result<(), ActiveReblitCommitCleanupNamespaceError> {
    if retained.layout() == expected {
        Ok(())
    } else {
        Err(ActiveReblitCommitCleanupNamespaceError::LayoutChanged)
    }
}

fn require_matching_fingerprints(
    before: &RetainedActiveReblitCommitCleanupNamespace,
    after: &RetainedActiveReblitCommitCleanupNamespace,
) -> Result<(), ActiveReblitCommitCleanupNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(ActiveReblitCommitCleanupNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    installation: &Installation,
    journal: &TransitionJournalStore,
    journal_record_binding: &TransitionJournalRecordBinding,
    expected: &TransitionRecord,
) -> Result<(), ActiveReblitCommitCleanupNamespaceError> {
    if !journal.has_record_store_binding(journal_record_binding) {
        return Err(ActiveReblitCommitCleanupNamespaceError::JournalChanged);
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, journal_record_binding, expected)? {
        Ok(())
    } else {
        Err(ActiveReblitCommitCleanupNamespaceError::JournalChanged)
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum ActiveReblitCommitCleanupNamespaceError {
    #[error("capture or revalidate the exact forward ActiveReblit CommitDecided namespace")]
    Capture(#[from] CaptureError),
    #[error("retain the exact descriptor-backed ActiveReblit commit-cleanup projection")]
    Projection(#[from] ActiveReblitCommitCleanupCaptureError),
    #[error("assess the exact forward ActiveReblit CommitDecided namespace against the journal phase")]
    Policy(#[from] NamespacePolicyConflict),
    #[error("read the retained canonical ActiveReblit CommitDecided transition journal")]
    Journal(#[from] StorageError),
    #[error("the source is not an exact forward ActiveReblit CommitDecided record")]
    WrongSource,
    #[error("the ActiveReblit CommitDecided namespace is not Apply or Finish")]
    UnsupportedLayout,
    #[error("the exact ActiveReblit CommitDecided journal binding changed during namespace proof")]
    JournalChanged,
    #[error("the ActiveReblit CommitDecided namespace changed during proof")]
    NamespaceChanged,
    #[error("the exact ActiveReblit CommitDecided layout changed during proof")]
    LayoutChanged,
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}

pub(in crate::client::startup_reconciliation) fn active_reblit_commit_cleanup_namespace_error_is_mismatch(
    error: &ActiveReblitCommitCleanupNamespaceError,
) -> bool {
    match error {
        ActiveReblitCommitCleanupNamespaceError::Capture(source) => capture_error_is_structural(source),
        ActiveReblitCommitCleanupNamespaceError::Projection(source) => projection_error_is_mismatch(source),
        ActiveReblitCommitCleanupNamespaceError::Policy(_)
        | ActiveReblitCommitCleanupNamespaceError::WrongSource
        | ActiveReblitCommitCleanupNamespaceError::UnsupportedLayout => true,
        ActiveReblitCommitCleanupNamespaceError::Journal(_)
        | ActiveReblitCommitCleanupNamespaceError::JournalChanged
        | ActiveReblitCommitCleanupNamespaceError::NamespaceChanged
        | ActiveReblitCommitCleanupNamespaceError::LayoutChanged
        | ActiveReblitCommitCleanupNamespaceError::Installation(_) => false,
    }
}

fn projection_error_is_mismatch(error: &ActiveReblitCommitCleanupCaptureError) -> bool {
    match error {
        ActiveReblitCommitCleanupCaptureError::Capture(source) => capture_error_is_structural(source),
        ActiveReblitCommitCleanupCaptureError::WrongOperation
        | ActiveReblitCommitCleanupCaptureError::WrongPhase
        | ActiveReblitCommitCleanupCaptureError::PreviousStateMissing
        | ActiveReblitCommitCleanupCaptureError::PreviousCount { .. }
        | ActiveReblitCommitCleanupCaptureError::WrapperCount { .. }
        | ActiveReblitCommitCleanupCaptureError::WrongTargetName
        | ActiveReblitCommitCleanupCaptureError::NotCleanupLayout
        | ActiveReblitCommitCleanupCaptureError::CrossDevice => true,
        ActiveReblitCommitCleanupCaptureError::ProjectionChanged => false,
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FRESH_NAMESPACE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_commit_cleanup_fresh_namespace_capture(
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

#[cfg(test)]
mod classification_tests {
    use super::*;

    #[test]
    fn only_stable_shape_mismatches_may_defer() {
        assert!(active_reblit_commit_cleanup_namespace_error_is_mismatch(
            &ActiveReblitCommitCleanupNamespaceError::Policy(
                NamespacePolicyConflict::ActiveReblitWrapper,
            ),
        ));
        assert!(active_reblit_commit_cleanup_namespace_error_is_mismatch(
            &ActiveReblitCommitCleanupNamespaceError::WrongSource,
        ));
        assert!(active_reblit_commit_cleanup_namespace_error_is_mismatch(
            &ActiveReblitCommitCleanupNamespaceError::UnsupportedLayout,
        ));
        assert!(!active_reblit_commit_cleanup_namespace_error_is_mismatch(
            &ActiveReblitCommitCleanupNamespaceError::JournalChanged,
        ));
        assert!(!active_reblit_commit_cleanup_namespace_error_is_mismatch(
            &ActiveReblitCommitCleanupNamespaceError::NamespaceChanged,
        ));
        assert!(!active_reblit_commit_cleanup_namespace_error_is_mismatch(
            &ActiveReblitCommitCleanupNamespaceError::LayoutChanged,
        ));
    }
}
