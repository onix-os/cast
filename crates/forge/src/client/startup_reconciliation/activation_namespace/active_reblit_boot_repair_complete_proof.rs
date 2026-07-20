//! Independent preserved-wrapper proof for an ActiveReblit
//! `BootRepairComplete` record.
//!
//! Successful completion routing recaptures this phase-specific read-only
//! proof. It exposes neither boot capability nor journal mutation and cannot
//! be reused for Started-to-Unverified retention or terminal finalization.

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    candidate_preserve_proof::{
        UsrRollbackCandidatePreserveNamespaceError, require_exact_active_reblit_boot_repair_complete_topology,
    },
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitBootRepairCompleteNamespaceInspection {
    before: NamespaceSnapshot,
    wrapper_index: usize,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackActiveReblitBootRepairCompleteNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
    wrapper_index: usize,
}

impl UsrRollbackActiveReblitBootRepairCompleteNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackActiveReblitBootRepairCompleteNamespaceError> {
        require_exact_journal(installation, journal, expected)?;
        run_before_complete_namespace_capture(installation)?;
        let before = capture_snapshot(installation, expected)?;
        let wrapper_index = require_exact_active_reblit_boot_repair_complete_topology(expected, &before)?;
        Ok(Self { before, wrapper_index })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitBootRepairCompleteNamespaceProof,
        UsrRollbackActiveReblitBootRepairCompleteNamespaceError,
    > {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_wrapper_index(expected, &self.before, self.wrapper_index)?;
        require_exact_wrapper_index(expected, &after, self.wrapper_index)?;
        require_exact_journal(installation, journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackActiveReblitBootRepairCompleteNamespaceProof {
            before: self.before,
            after,
            wrapper_index: self.wrapper_index,
        })
    }
}

impl UsrRollbackActiveReblitBootRepairCompleteNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackActiveReblitBootRepairCompleteNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_wrapper_index(expected, &self.before, self.wrapper_index)?;
        require_exact_wrapper_index(expected, &self.after, self.wrapper_index)?;
        require_exact_journal(installation, journal, expected)?;

        run_before_fresh_namespace_capture();
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

    #[cfg(test)]
    pub(in crate::client) fn wrapper_index(&self) -> usize {
        self.wrapper_index
    }
}

fn require_exact_wrapper_index(
    expected: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
    wrapper_index: usize,
) -> Result<(), UsrRollbackActiveReblitBootRepairCompleteNamespaceError> {
    let actual = require_exact_active_reblit_boot_repair_complete_topology(expected, snapshot)?;
    if actual == wrapper_index {
        Ok(())
    } else {
        Err(
            UsrRollbackActiveReblitBootRepairCompleteNamespaceError::WrapperIndexChanged {
                expected: wrapper_index,
                actual,
            },
        )
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackActiveReblitBootRepairCompleteNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackActiveReblitBootRepairCompleteNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    installation: &Installation,
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackActiveReblitBootRepairCompleteNamespaceError> {
    let cast = installation.retained_mutable_cast_directory()?;
    match journal.load_revalidated_retained_cast(cast)? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrRollbackActiveReblitBootRepairCompleteNamespaceError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackActiveReblitBootRepairCompleteNamespaceError {
    #[error("capture or revalidate the exact ActiveReblit BootRepairComplete namespace")]
    Capture(#[from] CaptureError),
    #[error("prove the exact preserved ActiveReblit whole-wrapper topology at BootRepairComplete")]
    Topology(#[from] UsrRollbackCandidatePreserveNamespaceError),
    #[error("read the retained canonical ActiveReblit BootRepairComplete journal")]
    Journal(#[from] StorageError),
    #[error("the retained ActiveReblit BootRepairComplete journal changed during namespace proof")]
    JournalChanged,
    #[error("the ActiveReblit BootRepairComplete namespace changed during proof")]
    NamespaceChanged,
    #[error("the ActiveReblit replacement-wrapper index changed from {expected} to {actual}")]
    WrapperIndexChanged { expected: usize, actual: usize },
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_COMPLETE_NAMESPACE_CAPTURE_FAULT:
        std::cell::RefCell<Option<ActiveReblitBootRepairCompleteCaptureFault>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(in crate::client) enum ActiveReblitBootRepairCompleteCaptureFault {
    PermissionDenied,
    Io,
    Timeout,
    RetryExhausted,
}

#[cfg(test)]
pub(in crate::client) fn arm_active_reblit_boot_repair_complete_capture_fault(
    fault: ActiveReblitBootRepairCompleteCaptureFault,
) {
    BEFORE_COMPLETE_NAMESPACE_CAPTURE_FAULT.with(|slot| {
        assert!(slot.borrow_mut().replace(fault).is_none());
    });
}

#[cfg(test)]
fn run_before_complete_namespace_capture(
    installation: &Installation,
) -> Result<(), UsrRollbackActiveReblitBootRepairCompleteNamespaceError> {
    BEFORE_COMPLETE_NAMESPACE_CAPTURE_FAULT.with(|slot| {
        let Some(fault) = slot.borrow_mut().take() else {
            return Ok(());
        };
        let error = match fault {
            ActiveReblitBootRepairCompleteCaptureFault::PermissionDenied => {
                std::io::Error::from_raw_os_error(nix::libc::EACCES)
            }
            ActiveReblitBootRepairCompleteCaptureFault::Io => {
                std::io::Error::from_raw_os_error(nix::libc::EIO)
            }
            ActiveReblitBootRepairCompleteCaptureFault::Timeout => {
                return Err(CaptureError::Deadline.into());
            }
            ActiveReblitBootRepairCompleteCaptureFault::RetryExhausted => {
                std::io::Error::from_raw_os_error(nix::libc::EINTR)
            }
        };
        Err(CaptureError::Io {
            operation: "inject BootRepairComplete capture fault",
            path: installation.root.clone(),
            source: error,
        }
        .into())
    })
}

#[cfg(not(test))]
fn run_before_complete_namespace_capture(
    _installation: &Installation,
) -> Result<(), UsrRollbackActiveReblitBootRepairCompleteNamespaceError> {
    Ok(())
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FRESH_NAMESPACE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_active_reblit_boot_repair_complete_fresh_namespace_capture(
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
