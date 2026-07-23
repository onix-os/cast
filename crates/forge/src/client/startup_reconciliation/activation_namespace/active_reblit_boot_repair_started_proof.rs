//! Independent preserved-wrapper proof for an ActiveReblit
//! `BootRepairStarted` record.
//!
//! A later startup's conservative transition to `BootRepairUnverified`
//! performs a fresh capture of this read-only phase-specific proof. The proof
//! exposes neither boot capability nor journal mutation.

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    candidate_preserve_proof::{
        UsrRollbackCandidatePreserveNamespaceError, require_exact_active_reblit_boot_repair_started_topology,
    },
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitBootRepairStartedNamespaceInspection {
    before: NamespaceSnapshot,
    wrapper_index: usize,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitBootRepairStartedNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    wrapper_index: usize,
}

impl UsrRollbackActiveReblitBootRepairStartedNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackActiveReblitBootRepairStartedNamespaceError> {
        require_exact_journal(installation, journal, expected)?;
        run_before_started_namespace_capture(installation)?;
        let before = capture_snapshot(installation, expected)?;
        let wrapper_index = require_exact_active_reblit_boot_repair_started_topology(expected, &before)?;
        Ok(Self { before, wrapper_index })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitBootRepairStartedNamespaceProof,
        UsrRollbackActiveReblitBootRepairStartedNamespaceError,
    > {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_wrapper_index(expected, &self.before, self.wrapper_index)?;
        require_exact_wrapper_index(expected, &after, self.wrapper_index)?;
        require_exact_journal(installation, journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackActiveReblitBootRepairStartedNamespaceProof {
            before: self.before,
            after,
            wrapper_index: self.wrapper_index,
        })
    }
}

impl UsrRollbackActiveReblitBootRepairStartedNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackActiveReblitBootRepairStartedNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_wrapper_index(expected, &self.before, self.wrapper_index)?;
        require_exact_wrapper_index(expected, &self.after, self.wrapper_index)?;
        require_exact_journal(installation, journal, expected)?;

        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_wrapper_index(expected, &fresh, self.wrapper_index)?;

        require_exact_journal(installation, journal, expected)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn require_exact_wrapper_index(
    expected: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    wrapper_index: usize,
) -> Result<(), UsrRollbackActiveReblitBootRepairStartedNamespaceError> {
    let actual = require_exact_active_reblit_boot_repair_started_topology(expected, snapshot)?;
    if actual == wrapper_index {
        Ok(())
    } else {
        Err(
            UsrRollbackActiveReblitBootRepairStartedNamespaceError::WrapperIndexChanged {
                expected: wrapper_index,
                actual,
            },
        )
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackActiveReblitBootRepairStartedNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackActiveReblitBootRepairStartedNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    installation: &Installation,
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackActiveReblitBootRepairStartedNamespaceError> {
    let cast = installation.retained_mutable_cast_directory()?;
    match journal.load_revalidated_retained_cast(cast)? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackActiveReblitBootRepairStartedNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackActiveReblitBootRepairStartedNamespaceError {
    #[error("capture or revalidate the exact ActiveReblit BootRepairStarted namespace")]
    Capture(#[from] CaptureError),
    #[error("prove the exact preserved ActiveReblit whole-wrapper topology at BootRepairStarted")]
    Topology(#[from] UsrRollbackCandidatePreserveNamespaceError),
    #[error("read the retained canonical ActiveReblit BootRepairStarted journal")]
    Journal(#[from] StorageError),
    #[error("the retained ActiveReblit BootRepairStarted journal changed during namespace proof")]
    JournalChanged,
    #[error("the ActiveReblit BootRepairStarted namespace changed during proof")]
    NamespaceChanged,
    #[error("the ActiveReblit replacement-wrapper index changed from {expected} to {actual}")]
    WrapperIndexChanged { expected: usize, actual: usize },
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_STARTED_NAMESPACE_CAPTURE_FAULT:
        std::cell::RefCell<Option<ActiveReblitBootRepairStartedCaptureFault>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(in crate::client) enum ActiveReblitBootRepairStartedCaptureFault {
    PermissionDenied,
    Io,
    Timeout,
    RetryExhausted,
}

#[cfg(test)]
pub(in crate::client) fn arm_active_reblit_boot_repair_started_capture_fault(
    fault: ActiveReblitBootRepairStartedCaptureFault,
) {
    BEFORE_STARTED_NAMESPACE_CAPTURE_FAULT.with(|slot| {
        assert!(slot.borrow_mut().replace(fault).is_none());
    });
}

#[cfg(test)]
fn run_before_started_namespace_capture(
    installation: &Installation,
) -> Result<(), UsrRollbackActiveReblitBootRepairStartedNamespaceError> {
    BEFORE_STARTED_NAMESPACE_CAPTURE_FAULT.with(|slot| {
        let Some(fault) = slot.borrow_mut().take() else {
            return Ok(());
        };
        let error = match fault {
            ActiveReblitBootRepairStartedCaptureFault::PermissionDenied => {
                std::io::Error::from_raw_os_error(nix::libc::EACCES)
            }
            ActiveReblitBootRepairStartedCaptureFault::Io => std::io::Error::from_raw_os_error(nix::libc::EIO),
            ActiveReblitBootRepairStartedCaptureFault::Timeout => {
                return Err(CaptureError::Deadline.into());
            }
            ActiveReblitBootRepairStartedCaptureFault::RetryExhausted => {
                std::io::Error::from_raw_os_error(nix::libc::EINTR)
            }
        };
        Err(CaptureError::Io {
            operation: "inject BootRepairStarted capture fault",
            path: installation.root.clone(),
            source: error,
        }
        .into())
    })
}

#[cfg(not(test))]
fn run_before_started_namespace_capture(
    _installation: &Installation,
) -> Result<(), UsrRollbackActiveReblitBootRepairStartedNamespaceError> {
    Ok(())
}
