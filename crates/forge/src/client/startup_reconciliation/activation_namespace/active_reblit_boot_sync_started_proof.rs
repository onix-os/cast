//! Retained read-only namespace proof for exact forward ActiveReblit
//! `BootSyncStarted` restart recovery.
//!
//! Both sides of admission retain descriptor-rooted namespace snapshots and
//! the phase-authorized forward layout. Revalidation requires a fresh matching
//! capture while the exact source journal inode remains bound. No namespace,
//! journal, boot, cleanup, or trigger effect is exposed.

use crate::{
    Installation,
    transition_journal::{
        Operation, Phase, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    active_reblit_boot_repair_started_error_classification::capture_error_is_structural,
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
    policy::{LayoutAlternative, NamespacePolicyConflict, assess_snapshot_layout},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct ActiveReblitBootSyncStartedNamespaceInspection {
    before: NamespaceSnapshot,
    layout: LayoutAlternative,
}

/// Exact forward namespace evidence retained across restart admission.
///
/// This type intentionally implements neither `Clone` nor `Copy`.
#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct ActiveReblitBootSyncStartedNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    layout: LayoutAlternative,
}

impl ActiveReblitBootSyncStartedNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<Self, ActiveReblitBootSyncStartedNamespaceError> {
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
    ) -> Result<ActiveReblitBootSyncStartedNamespaceProof, ActiveReblitBootSyncStartedNamespaceError> {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_layout(expected, &self.before, self.layout)?;
        require_exact_layout(expected, &after, self.layout)?;
        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(ActiveReblitBootSyncStartedNamespaceProof {
            before: self.before,
            after,
            layout: self.layout,
        })
    }
}

impl ActiveReblitBootSyncStartedNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<(), ActiveReblitBootSyncStartedNamespaceError> {
        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_layout(expected, &self.before, self.layout)?;
        require_exact_layout(expected, &self.after, self.layout)?;

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
}

fn exact_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<LayoutAlternative, ActiveReblitBootSyncStartedNamespaceError> {
    if record.operation != Operation::ActiveReblit
        || record.phase != Phase::BootSyncStarted
        || record.rollback.is_some()
    {
        return Err(ActiveReblitBootSyncStartedNamespaceError::WrongSource);
    }
    assess_snapshot_layout(record, snapshot).map_err(Into::into)
}

fn require_exact_layout(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    expected: LayoutAlternative,
) -> Result<(), ActiveReblitBootSyncStartedNamespaceError> {
    if exact_layout(record, snapshot)? == expected {
        Ok(())
    } else {
        Err(ActiveReblitBootSyncStartedNamespaceError::LayoutChanged)
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), ActiveReblitBootSyncStartedNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(ActiveReblitBootSyncStartedNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    installation: &Installation,
    journal: &TransitionJournalStore,
    journal_record_binding: &TransitionJournalRecordBinding,
    expected: &TransitionRecord,
) -> Result<(), ActiveReblitBootSyncStartedNamespaceError> {
    if !journal.has_record_store_binding(journal_record_binding) {
        return Err(ActiveReblitBootSyncStartedNamespaceError::JournalChanged);
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, journal_record_binding, expected)? {
        Ok(())
    } else {
        Err(ActiveReblitBootSyncStartedNamespaceError::JournalChanged)
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum ActiveReblitBootSyncStartedNamespaceError {
    #[error("capture or revalidate the exact forward ActiveReblit BootSyncStarted namespace")]
    Capture(#[from] CaptureError),
    #[error("assess the exact forward ActiveReblit BootSyncStarted namespace against the journal phase")]
    Policy(#[from] NamespacePolicyConflict),
    #[error("read the retained canonical ActiveReblit BootSyncStarted transition journal")]
    Journal(#[from] StorageError),
    #[error("the source is not an exact forward ActiveReblit BootSyncStarted record")]
    WrongSource,
    #[error("the exact ActiveReblit BootSyncStarted journal binding changed during namespace proof")]
    JournalChanged,
    #[error("the ActiveReblit BootSyncStarted namespace changed during proof")]
    NamespaceChanged,
    #[error("the exact ActiveReblit BootSyncStarted layout changed during proof")]
    LayoutChanged,
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}

pub(in crate::client::startup_reconciliation) fn active_reblit_boot_sync_started_namespace_error_is_mismatch(
    error: &ActiveReblitBootSyncStartedNamespaceError,
) -> bool {
    match error {
        ActiveReblitBootSyncStartedNamespaceError::Capture(source) => {
            capture_error_is_structural(source)
        }
        ActiveReblitBootSyncStartedNamespaceError::Policy(_)
        | ActiveReblitBootSyncStartedNamespaceError::WrongSource => true,
        ActiveReblitBootSyncStartedNamespaceError::JournalChanged
        | ActiveReblitBootSyncStartedNamespaceError::NamespaceChanged
        | ActiveReblitBootSyncStartedNamespaceError::LayoutChanged
        | ActiveReblitBootSyncStartedNamespaceError::Journal(_)
        | ActiveReblitBootSyncStartedNamespaceError::Installation(_) => false,
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FRESH_NAMESPACE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_boot_sync_started_fresh_namespace_capture(
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
