//! Persistence projection for one durable ActiveReblit candidate outcome.
//!
//! The durability constructor remains independent from journal advancement.
//! This child owns complete retained-evidence revalidation and the sole exact
//! authority-derived journal advance fixed by the private origin.

use crate::{
    Installation,
    transition_journal::{
        CodecError, Phase, RollbackActionOutcome, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    ActiveReblitDurabilityOrigin, UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority,
    require_active_reblit_post_effect_evidence, require_active_reblit_pre_effect_evidence,
};
use crate::client::startup_reconciliation::{
    UsrRollbackCandidatePreserveAuthorityError,
    usr_rollback_candidate_preserve_authority::effect_evidence::require_effect_binding,
};

/// Exact authority-derived ActiveReblit `CandidatePreserved` publication and
/// its new inode binding.
pub(in crate::client) struct UsrRollbackActiveReblitCandidatePreservePublishedRecord {
    record: TransitionRecord,
    binding: TransitionJournalRecordBinding,
}

impl UsrRollbackActiveReblitCandidatePreservePublishedRecord {
    pub(in crate::client) fn into_parts(self) -> (TransitionRecord, TransitionJournalRecordBinding) {
        (self.record, self.binding)
    }
}

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

    /// Revalidate, then consume the durable ActiveReblit authority through the
    /// exact `CandidatePreserveIntent` to `CandidatePreserved` boundary. The
    /// caller cannot supply or override the successor fixed by the private
    /// origin.
    pub(in crate::client) fn advance_candidate_preserved_record_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreservePublishedRecord,
        UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError,
    > {
        self.revalidate(journal)?;
        let outcome = match self.origin {
            ActiveReblitDurabilityOrigin::Applied => RollbackActionOutcome::Applied,
            ActiveReblitDurabilityOrigin::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        };
        let successor = self
            ._effect
            .record
            .rollback_successor(Some(outcome))
            .map_err(UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError::Successor)?;
        if successor.phase != Phase::CandidatePreserved {
            return Err(
                UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError::UnexpectedSuccessor {
                    phase: successor.phase,
                },
            );
        }
        let cast = self._effect.installation.retained_mutable_cast_directory()?;
        match journal.advance_record_binding(cast, self._effect.journal_record_binding, &successor) {
            Ok(binding) => Ok(UsrRollbackActiveReblitCandidatePreservePublishedRecord {
                record: successor,
                binding,
            }),
            Err(source) => Err(UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError::Storage {
                source,
                successor,
            }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError {
    #[error("revalidate exact durable ActiveReblit candidate-preservation authority before the bound journal advance")]
    Authority(#[from] UsrRollbackCandidatePreserveAuthorityError),
    #[error("revalidate retained installation before the bound ActiveReblit CandidatePreserved journal advance")]
    Installation(#[from] crate::installation::Error),
    #[error("derive the authority-owned ActiveReblit CandidatePreserved successor")]
    Successor(#[source] CodecError),
    #[error("authority-owned ActiveReblit candidate-preservation successor has unexpected phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("advance the exact bound ActiveReblit candidate-preservation journal record")]
    Storage {
        #[source]
        source: StorageError,
        successor: TransitionRecord,
    },
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
