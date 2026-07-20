//! Persistence projection for one durable ActiveReblit candidate outcome.
//!
//! The durability constructor remains independent from journal advancement.
//! This child exposes only complete retained-evidence revalidation and the
//! authority-owned successor required by the persistence boundary.

use crate::{
    Installation,
    transition_journal::{CodecError, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

use super::{
    ActiveReblitDurabilityOrigin, UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority,
    require_active_reblit_post_effect_evidence, require_active_reblit_pre_effect_evidence,
};
use crate::client::startup_reconciliation::{
    UsrRollbackCandidatePreserveAuthorityError,
    usr_rollback_candidate_preserve_authority::effect_evidence::require_effect_binding,
};

impl UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority<'_> {
    /// Revalidate the complete durable authority without repeating any
    /// namespace mutation or synchronization.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
        let effect = &self._effect;
        // The per-open binding remains the first retained-evidence
        // observation at the persistence boundary.
        require_effect_binding(
            &effect.installation,
            &effect.journal_record_binding,
            &effect.record,
            journal,
        )?;
        require_active_reblit_pre_effect_evidence(
            &effect.installation,
            &effect.state_db,
            &effect.record,
            &effect.database,
            &effect.journal_record_binding,
            journal,
        )?;
        let namespace_result = effect.namespace.revalidate(&effect.installation, &effect.record);
        run_before_persistence_durable_trailing_evidence();
        let trailing_evidence = require_effect_binding(
            &effect.installation,
            &effect.journal_record_binding,
            &effect.record,
            journal,
        )
        .and_then(|()| {
            require_active_reblit_post_effect_evidence(
                &effect.installation,
                &effect.state_db,
                &effect.record,
                &effect.database,
                &effect.journal_record_binding,
                journal,
            )
        });
        namespace_result?;
        trailing_evidence
    }

    /// Borrow the retained installation which owns this authority.
    pub(in crate::client) fn installation(&self) -> &Installation {
        &self._effect.installation
    }

    /// Borrow the exact ActiveReblit `CandidatePreserveIntent` source.
    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self._effect.record
    }

    /// Derive the sole legal `CandidatePreserved` successor from the private
    /// origin fixed by the authority's construction path.
    pub(in crate::client) fn candidate_preserved_successor(&self) -> Result<TransitionRecord, CodecError> {
        let outcome = match self.origin {
            ActiveReblitDurabilityOrigin::Applied => RollbackActionOutcome::Applied,
            ActiveReblitDurabilityOrigin::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        };
        self._effect.record.rollback_successor(Some(outcome))
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_PERSISTENCE_DURABLE_TRAILING_EVIDENCE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_candidate_preserve_persistence_durable_trailing_evidence(
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
