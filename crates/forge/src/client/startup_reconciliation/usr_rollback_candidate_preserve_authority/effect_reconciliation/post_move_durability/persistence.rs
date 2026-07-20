//! Persistence-facing revalidation and projection of durable candidate proof.
//!
//! The post-move durability constructor remains isolated from journal
//! successor semantics. This child exposes only the read-only evidence checks
//! and authority-owned projection needed at the persistence boundary.

use crate::{
    Installation,
    transition_journal::{CodecError, TransitionJournalStore, TransitionRecord},
};

use super::{
    UsrRollbackNewStateCandidatePreserveDurableEffectAuthority, require_post_effect_evidence,
    require_pre_effect_evidence,
};
use crate::client::startup_reconciliation::{
    UsrRollbackCandidatePreserveAuthorityError,
    usr_rollback_candidate_preserve_authority::effect_evidence::require_effect_binding,
};

impl UsrRollbackNewStateCandidatePreserveDurableEffectAuthority<'_> {
    /// Revalidate the complete durable authority without repeating any
    /// namespace synchronization or mutation.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
        let effect = &self._effect;
        // The per-open binding is deliberately the first retained-evidence
        // observation on every persistence-side revalidation.
        require_effect_binding(
            &effect.installation,
            &effect.journal_record_binding,
            &effect.record,
            journal,
        )?;
        require_pre_effect_evidence(
            &effect.installation,
            &effect.state_db,
            &effect.record,
            &effect.database,
            &effect.journal_record_binding,
            journal,
        )?;
        let namespace_result = effect.namespace.revalidate(&effect.installation, &effect.record);
        before_durable_trailing_evidence();
        let trailing_evidence = require_effect_binding(
            &effect.installation,
            &effect.journal_record_binding,
            &effect.record,
            journal,
        )
        .and_then(|()| {
            require_post_effect_evidence(
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

    /// Borrow the exact `CandidatePreserveIntent` source record.
    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self._effect.record
    }

    /// Derive the sole legal `CandidatePreserved` successor from the origin
    /// fixed by the authority's construction path.
    pub(in crate::client) fn candidate_preserved_successor(&self) -> Result<TransitionRecord, CodecError> {
        self._effect.record.rollback_successor(Some(self.origin))
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_DURABLE_TRAILING_EVIDENCE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_rollback_candidate_preserve_durable_trailing_evidence(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_DURABLE_TRAILING_EVIDENCE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_durable_trailing_evidence() {
    BEFORE_DURABLE_TRAILING_EVIDENCE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_durable_trailing_evidence() {}
