//! Persistence projection for one durable archived candidate outcome.
//!
//! Namespace durability remains independent from journal advancement. This
//! child exposes only complete retained-evidence revalidation and the sole
//! successor fixed by the authority's private origin.

use crate::{
    Installation,
    transition_journal::{CodecError, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

use super::{
    ArchivedDurabilityOrigin, UsrRollbackArchivedCandidatePreserveDurableEffectAuthority,
    require_archived_post_effect_evidence, require_archived_pre_effect_evidence, require_binding,
};
use crate::client::startup_reconciliation::UsrRollbackCandidatePreserveAuthorityError;

impl UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'_> {
    /// Revalidate complete durable authority without repeating movement or
    /// synchronization.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
        let effect = &self.effect;
        require_binding(&effect.journal_binding, journal)?;
        require_archived_pre_effect_evidence(
            &effect.installation,
            &effect.state_db,
            &effect.record,
            &effect.database,
            journal,
        )?;
        let namespace_result = effect.namespace.revalidate(&effect.installation, &effect.record);
        run_before_persistence_durable_trailing_evidence();
        let trailing = require_binding(&effect.journal_binding, journal).and_then(|()| {
            require_archived_post_effect_evidence(
                &effect.installation,
                &effect.state_db,
                &effect.record,
                &effect.database,
                journal,
            )
        });
        namespace_result?;
        trailing
    }

    pub(in crate::client) fn installation(&self) -> &Installation {
        &self.effect.installation
    }

    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self.effect.record
    }

    pub(in crate::client) fn candidate_preserved_successor(&self) -> Result<TransitionRecord, CodecError> {
        let outcome = match self.origin {
            ArchivedDurabilityOrigin::Applied => RollbackActionOutcome::Applied,
            ArchivedDurabilityOrigin::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        };
        self.effect.record.rollback_successor(Some(outcome))
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_PERSISTENCE_DURABLE_TRAILING_EVIDENCE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_archived_candidate_preserve_persistence_durable_trailing_evidence(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_PERSISTENCE_DURABLE_TRAILING_EVIDENCE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_persistence_durable_trailing_evidence() {
    BEFORE_PERSISTENCE_DURABLE_TRAILING_EVIDENCE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_persistence_durable_trailing_evidence() {}
