//! Read-only namespace proof for adopting an exact forward ActiveReblit
//! `BootSyncComplete` record at startup.
//!
//! The proof retains both sides of its admission sandwich and the exact
//! phase-authorized layout. Revalidation requires a fresh matching capture.
//! It exposes no journal, namespace, boot, or cleanup effect.

use crate::{
    Installation,
    transition_journal::{
        Operation, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    active_reblit_boot_repair_started_error_classification::capture_error_is_structural,
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
    policy::{LayoutAlternative, NamespacePolicyConflict, assess_snapshot_layout},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct ActiveReblitBootSyncCompleteNamespaceInspection {
    before: NamespaceSnapshot,
    layout: LayoutAlternative,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct ActiveReblitBootSyncCompleteNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    layout: LayoutAlternative,
}

impl ActiveReblitBootSyncCompleteNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<Self, ActiveReblitBootSyncCompleteNamespaceError> {
        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        let before = capture_snapshot(installation, expected)?;
        let layout = exact_layout(expected, &before)?;
        Ok(Self { before, layout })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<ActiveReblitBootSyncCompleteNamespaceProof, ActiveReblitBootSyncCompleteNamespaceError> {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_layout(expected, &self.before, self.layout)?;
        require_exact_layout(expected, &after, self.layout)?;
        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(ActiveReblitBootSyncCompleteNamespaceProof {
            before: self.before,
            after,
            layout: self.layout,
        })
    }
}

impl ActiveReblitBootSyncCompleteNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<(), ActiveReblitBootSyncCompleteNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_layout(expected, &self.before, self.layout)?;
        require_exact_layout(expected, &self.after, self.layout)?;
        require_exact_journal(installation, journal, journal_record_binding, expected)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_layout(expected, &fresh, self.layout)?;

        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    pub(in crate::client::startup_reconciliation) fn revalidate_successor_same_store(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        completed: &TransitionRecord,
        successor: &TransitionRecord,
    ) -> Result<(), ActiveReblitBootSyncCompleteNamespaceError> {
        self.revalidate_successor(
            installation,
            journal,
            successor_binding,
            completed,
            successor,
            SuccessorBindingMode::SameStore,
        )
    }

    pub(in crate::client::startup_reconciliation) fn revalidate_successor_reopened(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        completed: &TransitionRecord,
        successor: &TransitionRecord,
    ) -> Result<(), ActiveReblitBootSyncCompleteNamespaceError> {
        self.revalidate_successor(
            installation,
            journal,
            successor_binding,
            completed,
            successor,
            SuccessorBindingMode::Reopened,
        )
    }

    fn revalidate_successor(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        successor_binding: &TransitionJournalRecordBinding,
        completed: &TransitionRecord,
        successor: &TransitionRecord,
        binding_mode: SuccessorBindingMode,
    ) -> Result<(), ActiveReblitBootSyncCompleteNamespaceError> {
        require_exact_commit_decided_successor(completed, successor)?;
        require_exact_successor_journal(
            installation,
            journal,
            successor_binding,
            successor,
            binding_mode,
        )?;
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_layout(completed, &self.before, self.layout)?;
        require_exact_layout(completed, &self.after, self.layout)?;
        require_exact_successor_layout(successor, &self.before, self.layout)?;
        require_exact_successor_layout(successor, &self.after, self.layout)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, successor)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_layout(completed, &fresh, self.layout)?;
        require_exact_successor_layout(successor, &fresh, self.layout)?;

        require_exact_successor_journal(
            installation,
            journal,
            successor_binding,
            successor,
            binding_mode,
        )?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum SuccessorBindingMode {
    SameStore,
    Reopened,
}

fn exact_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<LayoutAlternative, ActiveReblitBootSyncCompleteNamespaceError> {
    if record.operation != Operation::ActiveReblit || record.phase != Phase::BootSyncComplete || record.rollback.is_some()
    {
        return Err(ActiveReblitBootSyncCompleteNamespaceError::WrongSource);
    }
    assess_snapshot_layout(record, snapshot).map_err(Into::into)
}

fn require_exact_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: LayoutAlternative,
) -> Result<(), ActiveReblitBootSyncCompleteNamespaceError> {
    if exact_layout(record, snapshot)? == expected {
        Ok(())
    } else {
        Err(ActiveReblitBootSyncCompleteNamespaceError::LayoutChanged)
    }
}

fn require_exact_commit_decided_successor(
    completed: &TransitionRecord,
    successor: &TransitionRecord,
) -> Result<(), ActiveReblitBootSyncCompleteNamespaceError> {
    let Some(successor_generation) = completed.generation.checked_add(1) else {
        return Err(ActiveReblitBootSyncCompleteNamespaceError::WrongSuccessor);
    };
    if completed.operation != Operation::ActiveReblit
        || completed.phase != Phase::BootSyncComplete
        || completed.rollback.is_some()
        || successor.operation != Operation::ActiveReblit
        || successor.phase != Phase::CommitDecided
        || successor.rollback.is_some()
        || successor.generation != successor_generation
        || successor.format != completed.format
        || successor.version != completed.version
        || successor.transition_id != completed.transition_id
        || successor.creation_epoch != completed.creation_epoch
        || successor.boot_publication_receipts != completed.boot_publication_receipts
        || successor.candidate != completed.candidate
        || successor.previous != completed.previous
        || successor.options != completed.options
        || successor.quarantine_name != completed.quarantine_name
    {
        return Err(ActiveReblitBootSyncCompleteNamespaceError::WrongSuccessor);
    }
    Ok(())
}

fn require_exact_successor_layout(
    successor: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: LayoutAlternative,
) -> Result<(), ActiveReblitBootSyncCompleteNamespaceError> {
    if assess_snapshot_layout(successor, snapshot)? == expected {
        Ok(())
    } else {
        Err(ActiveReblitBootSyncCompleteNamespaceError::LayoutChanged)
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), ActiveReblitBootSyncCompleteNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(ActiveReblitBootSyncCompleteNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    installation: &Installation,
    journal: &TransitionJournalStore,
    journal_record_binding: &TransitionJournalRecordBinding,
    expected: &TransitionRecord,
) -> Result<(), ActiveReblitBootSyncCompleteNamespaceError> {
    if !journal.has_record_store_binding(journal_record_binding) {
        return Err(ActiveReblitBootSyncCompleteNamespaceError::JournalChanged);
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, journal_record_binding, expected)? {
        Ok(())
    } else {
        Err(ActiveReblitBootSyncCompleteNamespaceError::JournalChanged)
    }
}

fn require_exact_successor_journal(
    installation: &Installation,
    journal: &TransitionJournalStore,
    successor_binding: &TransitionJournalRecordBinding,
    successor: &TransitionRecord,
    binding_mode: SuccessorBindingMode,
) -> Result<(), ActiveReblitBootSyncCompleteNamespaceError> {
    if matches!(binding_mode, SuccessorBindingMode::SameStore)
        && !journal.has_record_store_binding(successor_binding)
    {
        return Err(ActiveReblitBootSyncCompleteNamespaceError::JournalChanged);
    }
    let cast = installation.retained_mutable_cast_directory()?;
    let exact = match binding_mode {
        SuccessorBindingMode::SameStore => journal.has_record_binding(cast, successor_binding, successor)?,
        SuccessorBindingMode::Reopened => journal.has_reopened_record_binding(cast, successor_binding, successor)?,
    };
    if exact {
        Ok(())
    } else {
        Err(ActiveReblitBootSyncCompleteNamespaceError::JournalChanged)
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum ActiveReblitBootSyncCompleteNamespaceError {
    #[error("capture or revalidate the exact forward ActiveReblit BootSyncComplete namespace")]
    Capture(#[from] CaptureError),
    #[error("assess the exact forward ActiveReblit BootSyncComplete namespace against the journal phase")]
    Policy(#[from] NamespacePolicyConflict),
    #[error("read the retained canonical ActiveReblit BootSyncComplete transition journal")]
    Journal(#[from] StorageError),
    #[error("the source is not an exact forward ActiveReblit BootSyncComplete record")]
    WrongSource,
    #[error("the record is not the exact ActiveReblit CommitDecided successor")]
    WrongSuccessor,
    #[error("the exact ActiveReblit BootSyncComplete journal binding changed during namespace proof")]
    JournalChanged,
    #[error("the ActiveReblit BootSyncComplete namespace changed during proof")]
    NamespaceChanged,
    #[error("the exact ActiveReblit BootSyncComplete layout changed during namespace proof")]
    LayoutChanged,
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}

pub(in crate::client::startup_reconciliation) fn active_reblit_boot_sync_complete_namespace_error_is_mismatch(
    error: &ActiveReblitBootSyncCompleteNamespaceError,
) -> bool {
    match error {
        ActiveReblitBootSyncCompleteNamespaceError::Capture(source) => capture_error_is_structural(source),
        ActiveReblitBootSyncCompleteNamespaceError::Policy(_)
        | ActiveReblitBootSyncCompleteNamespaceError::WrongSource => true,
        ActiveReblitBootSyncCompleteNamespaceError::WrongSuccessor
        | ActiveReblitBootSyncCompleteNamespaceError::JournalChanged
        | ActiveReblitBootSyncCompleteNamespaceError::NamespaceChanged
        | ActiveReblitBootSyncCompleteNamespaceError::LayoutChanged
        | ActiveReblitBootSyncCompleteNamespaceError::Journal(_)
        | ActiveReblitBootSyncCompleteNamespaceError::Installation(_) => false,
    }
}

#[cfg(test)]
mod classification_tests {
    use super::*;

    #[test]
    fn stable_shape_mismatch_may_defer_but_changed_or_post_advance_evidence_does_not() {
        assert!(active_reblit_boot_sync_complete_namespace_error_is_mismatch(
            &ActiveReblitBootSyncCompleteNamespaceError::Policy(NamespacePolicyConflict::CandidateCount {
                actual: 0,
            }),
        ));
        assert!(active_reblit_boot_sync_complete_namespace_error_is_mismatch(
            &ActiveReblitBootSyncCompleteNamespaceError::WrongSource,
        ));
        assert!(!active_reblit_boot_sync_complete_namespace_error_is_mismatch(
            &ActiveReblitBootSyncCompleteNamespaceError::WrongSuccessor,
        ));
        assert!(!active_reblit_boot_sync_complete_namespace_error_is_mismatch(
            &ActiveReblitBootSyncCompleteNamespaceError::JournalChanged,
        ));
        assert!(!active_reblit_boot_sync_complete_namespace_error_is_mismatch(
            &ActiveReblitBootSyncCompleteNamespaceError::NamespaceChanged,
        ));
        assert!(!active_reblit_boot_sync_complete_namespace_error_is_mismatch(
            &ActiveReblitBootSyncCompleteNamespaceError::LayoutChanged,
        ));
    }

    #[test]
    fn missing_namespace_shape_may_defer_but_operational_capture_failure_does_not() {
        let missing = ActiveReblitBootSyncCompleteNamespaceError::Capture(CaptureError::Io {
            operation: "test missing ActiveReblit BootSyncComplete namespace shape",
            path: std::path::PathBuf::from("/test"),
            source: std::io::Error::from_raw_os_error(nix::libc::ENOENT),
        });
        let denied = ActiveReblitBootSyncCompleteNamespaceError::Capture(CaptureError::Io {
            operation: "test denied ActiveReblit BootSyncComplete namespace capture",
            path: std::path::PathBuf::from("/test"),
            source: std::io::Error::from_raw_os_error(nix::libc::EACCES),
        });
        assert!(active_reblit_boot_sync_complete_namespace_error_is_mismatch(&missing));
        assert!(!active_reblit_boot_sync_complete_namespace_error_is_mismatch(&denied));
        assert!(!active_reblit_boot_sync_complete_namespace_error_is_mismatch(
            &ActiveReblitBootSyncCompleteNamespaceError::Capture(CaptureError::Deadline),
        ));
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FRESH_NAMESPACE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_boot_sync_complete_fresh_namespace_capture(
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
