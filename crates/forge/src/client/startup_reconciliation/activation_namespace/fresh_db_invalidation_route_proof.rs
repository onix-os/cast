//! Independent namespace proof for routing one preserved NewState candidate.
//!
//! This proof is read-only. It retains both sides of its admission sandwich
//! and requires a fresh matching capture whenever the enclosing authority is
//! revalidated. The accepted namespace is the exact NewState
//! `CandidatePreserved` topology: the candidate is alone in its private
//! transition quarantine, staging is empty, and no target residue, parking,
//! or current-transition state wrapper exists.

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    candidate_preserve_proof::{
        UsrRollbackCandidatePreserveNamespaceError, require_exact_new_state_candidate_preserved_topology,
    },
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackFreshDbInvalidationRouteNamespaceInspection {
    before: NamespaceSnapshot,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrRollbackFreshDbInvalidationRouteNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
}

impl UsrRollbackFreshDbInvalidationRouteNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrRollbackFreshDbInvalidationRouteNamespaceError> {
        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        let before = capture_snapshot(installation, expected)?;
        require_exact_new_state_candidate_preserved_topology(expected, &before)?;
        Ok(Self { before })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<UsrRollbackFreshDbInvalidationRouteNamespaceProof, UsrRollbackFreshDbInvalidationRouteNamespaceError>
    {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &after)?;
        require_exact_new_state_candidate_preserved_topology(expected, &self.before)?;
        require_exact_new_state_candidate_preserved_topology(expected, &after)?;
        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackFreshDbInvalidationRouteNamespaceProof {
            before: self.before,
            after,
        })
    }
}

impl UsrRollbackFreshDbInvalidationRouteNamespaceProof {
    pub(in crate::client::startup_reconciliation) fn revalidate(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        journal_record_binding: &TransitionJournalRecordBinding,
        expected: &TransitionRecord,
    ) -> Result<(), UsrRollbackFreshDbInvalidationRouteNamespaceError> {
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &self.after)?;
        require_exact_new_state_candidate_preserved_topology(expected, &self.before)?;
        require_exact_new_state_candidate_preserved_topology(expected, &self.after)?;
        require_exact_journal(installation, journal, journal_record_binding, expected)?;

        run_before_fresh_namespace_capture();
        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_matching_fingerprints(&self.before, &fresh)?;
        require_exact_new_state_candidate_preserved_topology(expected, &fresh)?;

        require_exact_journal(installation, journal, journal_record_binding, expected)?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }
}

fn require_matching_fingerprints(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrRollbackFreshDbInvalidationRouteNamespaceError> {
    if before.fingerprint() == after.fingerprint() {
        Ok(())
    } else {
        Err(UsrRollbackFreshDbInvalidationRouteNamespaceError::NamespaceChanged)
    }
}

fn require_exact_journal(
    installation: &Installation,
    journal: &TransitionJournalStore,
    journal_record_binding: &TransitionJournalRecordBinding,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackFreshDbInvalidationRouteNamespaceError> {
    if !journal.has_record_store_binding(journal_record_binding) {
        return Err(UsrRollbackFreshDbInvalidationRouteNamespaceError::JournalChanged);
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, journal_record_binding, expected)? {
        Ok(())
    } else {
        Err(UsrRollbackFreshDbInvalidationRouteNamespaceError::JournalChanged)
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrRollbackFreshDbInvalidationRouteNamespaceError {
    #[error("capture or revalidate the exact fresh-database invalidation route namespace")]
    Capture(#[from] CaptureError),
    #[error("prove the exact NewState CandidatePreserved namespace topology")]
    Topology(#[from] UsrRollbackCandidatePreserveNamespaceError),
    #[error("read the retained canonical transition journal")]
    Journal(#[from] StorageError),
    #[error("the retained canonical transition journal changed during fresh-database invalidation route proof")]
    JournalChanged,
    #[error("the fresh-database invalidation route namespace changed during proof")]
    NamespaceChanged,
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FRESH_NAMESPACE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_fresh_db_invalidation_route_fresh_namespace_capture(
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
