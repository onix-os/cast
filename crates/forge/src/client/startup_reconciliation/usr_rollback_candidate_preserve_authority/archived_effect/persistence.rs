//! Persistence revalidation and exact advance for one durable archived outcome.
//!
//! Namespace durability remains independent from journal advancement. This
//! child owns complete retained-evidence revalidation and the sole exact
//! authority-derived journal advance fixed by the private origin.

use crate::{
    Installation,
    transition_journal::{
        CodecError, Phase, RollbackActionOutcome, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    ArchivedDurabilityOrigin, UsrRollbackArchivedCandidatePreserveDurableEffectAuthority,
    require_archived_post_effect_evidence, require_archived_pre_effect_evidence,
};
use crate::client::startup_reconciliation::{
    UsrRollbackCandidatePreserveAuthorityError,
    usr_rollback_candidate_preserve_authority::require_journal_record_binding,
};

/// Exact authority-derived archived `CandidatePreserved` publication and its
/// new inode binding.
pub(in crate::client) struct UsrRollbackArchivedCandidatePreservePublishedRecord {
    record: TransitionRecord,
    binding: TransitionJournalRecordBinding,
}

impl UsrRollbackArchivedCandidatePreservePublishedRecord {
    pub(in crate::client) fn into_parts(self) -> (TransitionRecord, TransitionJournalRecordBinding) {
        (self.record, self.binding)
    }
}

impl UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'_> {
    /// Revalidate complete durable authority without repeating movement or
    /// synchronization.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
        let effect = &self.effect;
        require_journal_record_binding(
            &effect.installation,
            journal,
            &effect.journal_record_binding,
            &effect.record,
        )?;
        require_archived_pre_effect_evidence(
            &effect.installation,
            &effect.state_db,
            &effect.record,
            &effect.database,
            &effect.journal_record_binding,
            journal,
        )?;
        let namespace_result = effect.namespace.revalidate(&effect.installation, &effect.record);
        run_before_persistence_durable_trailing_evidence();
        let trailing = require_journal_record_binding(
            &effect.installation,
            journal,
            &effect.journal_record_binding,
            &effect.record,
        )
        .and_then(|()| {
            require_archived_post_effect_evidence(
                &effect.installation,
                &effect.state_db,
                &effect.record,
                &effect.database,
                &effect.journal_record_binding,
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

    /// Revalidate, then consume the durable archived authority through the
    /// exact `CandidatePreserveIntent` to `CandidatePreserved` boundary. The
    /// caller cannot supply or override the successor fixed by the private
    /// origin.
    pub(in crate::client) fn advance_candidate_preserved_record_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackArchivedCandidatePreservePublishedRecord,
        UsrRollbackArchivedCandidatePreserveRecordAdvanceError,
    > {
        self.revalidate(journal)?;
        let outcome = match self.origin {
            ArchivedDurabilityOrigin::Applied => RollbackActionOutcome::Applied,
            ArchivedDurabilityOrigin::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        };
        let successor = self
            .effect
            .record
            .rollback_successor(Some(outcome))
            .map_err(UsrRollbackArchivedCandidatePreserveRecordAdvanceError::Successor)?;
        if successor.phase != Phase::CandidatePreserved {
            return Err(
                UsrRollbackArchivedCandidatePreserveRecordAdvanceError::UnexpectedSuccessor {
                    phase: successor.phase,
                },
            );
        }
        let cast = self.effect.installation.retained_mutable_cast_directory()?;
        match journal.advance_record_binding(cast, self.effect.journal_record_binding, &successor) {
            Ok(binding) => Ok(UsrRollbackArchivedCandidatePreservePublishedRecord {
                record: successor,
                binding,
            }),
            Err(source) => Err(UsrRollbackArchivedCandidatePreserveRecordAdvanceError::Storage {
                source,
                successor,
            }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackArchivedCandidatePreserveRecordAdvanceError {
    #[error("revalidate exact durable archived candidate-preservation authority before the bound journal advance")]
    Authority(#[from] UsrRollbackCandidatePreserveAuthorityError),
    #[error("revalidate retained installation before the bound archived CandidatePreserved journal advance")]
    Installation(#[from] crate::installation::Error),
    #[error("derive the authority-owned archived CandidatePreserved successor")]
    Successor(#[source] CodecError),
    #[error("authority-owned archived candidate-preservation successor has unexpected phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("advance the exact bound archived candidate-preservation journal record")]
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
