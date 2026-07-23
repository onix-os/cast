//! Persistence-facing revalidation and exact advance of durable candidate proof.
//!
//! The post-move durability constructor remains isolated from journal
//! successor semantics. This child owns the complete evidence recheck and the
//! one exact authority-derived journal advance required by persistence.

use crate::{
    Installation,
    transition_journal::{
        CodecError, Phase, StorageError, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    UsrRollbackNewStateCandidatePreserveDurableEffectAuthority, require_post_effect_evidence,
    require_pre_effect_evidence,
};
use crate::client::startup_reconciliation::{
    UsrRollbackCandidatePreserveAuthorityError,
    usr_rollback_candidate_preserve_authority::effect_evidence::require_effect_binding,
};

/// Exact authority-derived `CandidatePreserved` publication and its new inode
/// binding.
pub(in crate::client) struct UsrRollbackCandidatePreservePublishedRecord {
    record: TransitionRecord,
    binding: TransitionJournalRecordBinding,
}

impl UsrRollbackCandidatePreservePublishedRecord {
    pub(in crate::client) fn into_parts(self) -> (TransitionRecord, TransitionJournalRecordBinding) {
        (self.record, self.binding)
    }
}

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

    /// Revalidate, then consume the durable authority through the exact
    /// `CandidatePreserveIntent` to `CandidatePreserved` journal boundary. The
    /// caller cannot supply or override the successor fixed by the private
    /// origin.
    pub(in crate::client) fn advance_candidate_preserved_record_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackCandidatePreservePublishedRecord, UsrRollbackCandidatePreserveRecordAdvanceError> {
        self.revalidate(journal)?;
        let successor = self
            ._effect
            .record
            .rollback_successor(Some(self.origin))
            .map_err(UsrRollbackCandidatePreserveRecordAdvanceError::Successor)?;
        if successor.phase != Phase::CandidatePreserved {
            return Err(UsrRollbackCandidatePreserveRecordAdvanceError::UnexpectedSuccessor {
                phase: successor.phase,
            });
        }
        let cast = self._effect.installation.retained_mutable_cast_directory()?;
        match journal.advance_record_binding(cast, self._effect.journal_record_binding, &successor) {
            Ok(binding) => Ok(UsrRollbackCandidatePreservePublishedRecord {
                record: successor,
                binding,
            }),
            Err(source) => Err(UsrRollbackCandidatePreserveRecordAdvanceError::Storage { source, successor }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackCandidatePreserveRecordAdvanceError {
    #[error("revalidate exact durable NewState candidate-preservation authority before the bound journal advance")]
    Authority(#[from] UsrRollbackCandidatePreserveAuthorityError),
    #[error("revalidate retained installation before the bound CandidatePreserved journal advance")]
    Installation(#[from] crate::installation::Error),
    #[error("derive the authority-owned CandidatePreserved successor")]
    Successor(#[source] CodecError),
    #[error("authority-owned candidate-preservation successor has unexpected phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("advance the exact bound candidate-preservation journal record")]
    Storage {
        #[source]
        source: StorageError,
        successor: TransitionRecord,
    },
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
