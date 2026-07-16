//! Descriptor-rooted, bounded startup inventory of the activation namespace.
//!
//! This module is intentionally read-only. The diagnostic inventory and the
//! independent rollback-decision proof expose no rename, link, unlink,
//! creation, repair, or journal-advance operation.

mod capture;
mod decision_proof;
mod parent_durability;
mod policy;

#[cfg(test)]
mod tests;

use crate::{
    Installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

use capture::{CaptureError, NamespaceSnapshot, capture_snapshot};
#[cfg(test)]
pub(in crate::client) use decision_proof::arm_before_usr_rollback_decision_fresh_namespace_capture;
pub(super) use decision_proof::{
    UsrRollbackDecisionNamespaceError, UsrRollbackDecisionNamespaceInspection, UsrRollbackDecisionNamespaceProof,
};
pub(super) use policy::UsrExchangeLayout;
use policy::{LayoutAlternative, NamespacePolicyConflict, assess_snapshot_layout};

/// Complete read-only evidence collected around one startup assessment.
///
/// Both snapshots retain descriptors for every accepted directory, tree,
/// marker, state-ID, state-slot link, and root-ABI link.  Keeping both sides
/// prevents a matching-looking replacement after the first walk from being
/// mistaken for stable evidence.
#[derive(Debug)]
#[allow(dead_code)] // retained by PendingSystemTransition for structured diagnostics
pub(super) struct ActivationNamespaceEvidence {
    before: Result<NamespaceSnapshot, CaptureError>,
    after: Result<NamespaceSnapshot, CaptureError>,
    journal_before: JournalObservation,
    journal_after: JournalObservation,
    retained_revalidation: Result<(), CaptureError>,
    stability: ActivationNamespaceStability,
    policy: NamespacePolicyAssessment,
}

/// First half of the startup namespace sandwich.
///
/// This value deliberately cannot assess policy.  The final inventory and
/// retained/public-name revalidation must run only after every other startup
/// evidence source, including the second database inspection, has completed.
#[derive(Debug)]
pub(super) struct ActivationNamespaceInspection {
    before: Result<NamespaceSnapshot, CaptureError>,
    journal_before: JournalObservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ActivationNamespaceStability {
    Stable,
    Changed,
    Rejected,
}

#[derive(Debug)]
#[allow(dead_code)] // exact storage errors are part of the diagnostic snapshot
enum JournalObservation {
    Exact,
    Missing,
    Different(Box<TransitionRecord>),
    Rejected(StorageError),
}

#[derive(Debug)]
#[allow(dead_code)] // retains exact conflict/unavailability for structured diagnostics
enum NamespacePolicyAssessment {
    Exact(LayoutAlternative),
    Conflict(NamespacePolicyConflict),
    Unavailable(ActivationNamespaceStability),
}

impl ActivationNamespaceInspection {
    pub(super) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Self {
        let journal_before = observe_journal(journal, expected);
        let before = capture_snapshot(installation, expected);
        Self { before, journal_before }
    }

    pub(super) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> ActivationNamespaceEvidence {
        let after = capture_snapshot(installation, expected);
        run_before_final_namespace_revalidation();
        let retained_revalidation = match (&self.before, &after) {
            (Ok(before), Ok(after)) => before.revalidate_retained().and_then(|()| after.revalidate_retained()),
            (Err(_), _) | (_, Err(_)) => Ok(()),
        };
        // This is intentionally the last journal read.  No later startup
        // evidence collection can race ahead of the namespace sandwich.
        let journal_after = observe_journal(journal, expected);

        let stability = match (&self.before, &after, &retained_revalidation) {
            (Ok(before), Ok(after), Ok(())) if before.fingerprint() == after.fingerprint() => {
                ActivationNamespaceStability::Stable
            }
            (Ok(_), Ok(_), _) => ActivationNamespaceStability::Changed,
            (Err(_), _, _) | (_, Err(_), _) => ActivationNamespaceStability::Rejected,
        };
        let policy = match (&after, stability) {
            (Ok(snapshot), ActivationNamespaceStability::Stable) => match assess_snapshot_layout(expected, snapshot) {
                Ok(layout) => NamespacePolicyAssessment::Exact(layout),
                Err(conflict) => NamespacePolicyAssessment::Conflict(conflict),
            },
            (_, unavailable) => NamespacePolicyAssessment::Unavailable(unavailable),
        };

        ActivationNamespaceEvidence {
            before: self.before,
            after,
            journal_before: self.journal_before,
            journal_after,
            retained_revalidation,
            stability,
            policy,
        }
    }
}

impl ActivationNamespaceEvidence {
    pub(super) fn stability(&self) -> ActivationNamespaceStability {
        self.stability
    }

    pub(super) fn journal_is_exact(&self) -> bool {
        matches!(self.journal_before, JournalObservation::Exact)
            && matches!(self.journal_after, JournalObservation::Exact)
    }

    pub(super) fn phase_layout_is_exact(&self) -> bool {
        self.stability == ActivationNamespaceStability::Stable
            && self.journal_is_exact()
            && matches!(self.policy, NamespacePolicyAssessment::Exact(_))
    }

    pub(super) fn usr_exchange_layout(&self) -> Option<UsrExchangeLayout> {
        if self.stability != ActivationNamespaceStability::Stable || !self.journal_is_exact() {
            return None;
        }
        match self.policy {
            NamespacePolicyAssessment::Exact(layout) => layout.usr_exchange_layout(),
            NamespacePolicyAssessment::Conflict(_) | NamespacePolicyAssessment::Unavailable(_) => None,
        }
    }

    #[cfg(test)]
    pub(super) fn policy_was_assessed(&self) -> bool {
        !matches!(self.policy, NamespacePolicyAssessment::Unavailable(_))
    }
}

fn observe_journal(journal: &TransitionJournalStore, expected: &TransitionRecord) -> JournalObservation {
    match journal.load() {
        Ok(Some(actual)) if actual == *expected => JournalObservation::Exact,
        Ok(Some(actual)) => JournalObservation::Different(Box::new(actual)),
        Ok(None) => JournalObservation::Missing,
        Err(source) => JournalObservation::Rejected(source),
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FINAL_NAMESPACE_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_before_final_namespace_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_FINAL_NAMESPACE_REVALIDATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_final_namespace_revalidation() {
    BEFORE_FINAL_NAMESPACE_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_final_namespace_revalidation() {}
