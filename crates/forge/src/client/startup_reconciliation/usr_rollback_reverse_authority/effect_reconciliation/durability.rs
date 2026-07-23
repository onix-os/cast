//! Authority-level completion of reverse `/usr` parent durability.
//!
//! Distinct Applied and AlreadySatisfied reconciliation typestates converge
//! only after the namespace layer has synced both retained parents and proved
//! a final exact durable PRE layout. The rollback outcome is constructed here,
//! after that proof, and is never accepted from a caller.

use crate::{
    Installation, db,
    transition_journal::{
        CodecError, Phase, RollbackActionOutcome, StorageError, TransitionJournalRecordBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    ReconciledReverseEffect, UsrRollbackReverseAlreadySatisfiedEffectAuthority,
    UsrRollbackReverseAppliedEffectAuthority, require_post_namespace_evidence, require_pre_namespace_evidence,
};
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::{
        DatabaseEvidence, UsrRollbackReverseAuthorityError,
        activation_namespace::{
            UsrRollbackReverseAlreadySatisfiedNamespace, UsrRollbackReverseAppliedNamespace,
            UsrRollbackReverseDurableNamespace,
        },
        usr_rollback_reverse_authority::require_journal_record_binding,
    },
    startup_recovery::UsrRollbackReverseDurabilitySeal,
};

/// Opaque post-durability authority for the exact journal successor.
///
/// This type can exist only after staging-parent sync, installation-root sync,
/// and the low-level final durable PRE proof all succeeded.
#[must_use = "durable rollback-reverse evidence still requires exact journal persistence"]
pub(in crate::client) struct UsrRollbackReverseDurableEffectAuthority<'reservation> {
    _effect: DurableReverseEffect<'reservation>,
    outcome: RollbackActionOutcome,
}

/// Exact authority-derived `UsrRestored` publication and its new inode binding.
pub(in crate::client) struct UsrRollbackReversePublishedRecord {
    record: TransitionRecord,
    binding: TransitionJournalRecordBinding,
}

impl UsrRollbackReversePublishedRecord {
    pub(in crate::client) fn into_parts(self) -> (TransitionRecord, TransitionJournalRecordBinding) {
        (self.record, self.binding)
    }
}

struct DurableReverseEffect<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackReverseDurableNamespace,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl UsrRollbackReverseDurableEffectAuthority<'_> {
    /// Revalidate the complete durable reverse authority without consuming it.
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackReverseAuthorityError> {
        let effect = &self._effect;
        // Exact record identity is the first retained evidence observation.
        require_journal_record_binding(
            &effect.installation,
            journal,
            &effect.journal_record_binding,
            &effect.record,
        )?;
        require_pre_namespace_evidence(
            &effect.installation,
            &effect.state_db,
            &effect.record,
            &effect.database,
            &effect.journal_record_binding,
            journal,
        )?;
        let namespace_result = effect.namespace.revalidate(&effect.installation, &effect.record);
        before_durable_trailing_evidence();
        let trailing_evidence = require_post_namespace_evidence(
            &effect.installation,
            &effect.state_db,
            &effect.record,
            &effect.database,
            &effect.journal_record_binding,
            journal,
        );
        namespace_result?;
        trailing_evidence
    }

    /// Borrow the retained installation which owns this authority.
    pub(in crate::client) fn installation(&self) -> &Installation {
        &self._effect.installation
    }

    /// Borrow the exact `ReverseExchangeIntent` source record.
    pub(in crate::client) fn record(&self) -> &TransitionRecord {
        &self._effect.record
    }

    /// Revalidate, then consume the durable authority through the exact
    /// `ReverseExchangeIntent` to `UsrRestored` journal boundary. The caller
    /// cannot supply or override the successor fixed by the private outcome.
    pub(in crate::client) fn advance_usr_restored_record_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackReversePublishedRecord, UsrRollbackReverseRecordAdvanceError> {
        self.revalidate(journal)?;
        let successor = self
            ._effect
            .record
            .rollback_successor(Some(self.outcome))
            .map_err(UsrRollbackReverseRecordAdvanceError::Successor)?;
        if successor.phase != Phase::UsrRestored {
            return Err(UsrRollbackReverseRecordAdvanceError::UnexpectedSuccessor {
                phase: successor.phase,
            });
        }
        let cast = self._effect.installation.retained_mutable_cast_directory()?;
        match journal.advance_record_binding(cast, self._effect.journal_record_binding, &successor) {
            Ok(binding) => Ok(UsrRollbackReversePublishedRecord {
                record: successor,
                binding,
            }),
            Err(source) => Err(UsrRollbackReverseRecordAdvanceError::Storage { source, successor }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum UsrRollbackReverseRecordAdvanceError {
    #[error("revalidate exact durable rollback-reverse authority before the bound journal advance")]
    Authority(#[from] UsrRollbackReverseAuthorityError),
    #[error("revalidate retained installation before the bound rollback-reverse journal advance")]
    Installation(#[from] crate::installation::Error),
    #[error("derive the authority-owned rollback-reverse successor")]
    Successor(#[source] CodecError),
    #[error("authority-owned rollback-reverse successor has unexpected phase {phase:?}")]
    UnexpectedSuccessor { phase: Phase },
    #[error("advance the exact bound rollback-reverse journal record")]
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
fn arm_before_durable_trailing_evidence(hook: impl FnOnce() + 'static) {
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

impl<'reservation> UsrRollbackReverseAppliedEffectAuthority<'reservation> {
    /// Complete parent durability for an exchange applied by this invocation.
    pub(in crate::client) fn complete_parent_durability(
        self,
        _seal: &UsrRollbackReverseDurabilitySeal,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackReverseDurableEffectAuthority<'reservation>, UsrRollbackReverseAuthorityError> {
        // Exact record identity is the first retained evidence observation.
        require_journal_record_binding(
            &self._effect.installation,
            journal,
            &self._effect.journal_record_binding,
            &self._effect.record,
        )?;
        let effect = self._effect.complete_parent_durability_after_binding(journal)?;
        Ok(UsrRollbackReverseDurableEffectAuthority {
            _effect: effect,
            outcome: RollbackActionOutcome::Applied,
        })
    }
}

impl<'reservation> UsrRollbackReverseAlreadySatisfiedEffectAuthority<'reservation> {
    /// Complete parent durability for exact PRE evidence without an exchange.
    pub(in crate::client) fn complete_parent_durability(
        self,
        _seal: &UsrRollbackReverseDurabilitySeal,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackReverseDurableEffectAuthority<'reservation>, UsrRollbackReverseAuthorityError> {
        // Exact record identity is the first retained evidence observation.
        require_journal_record_binding(
            &self._effect.installation,
            journal,
            &self._effect.journal_record_binding,
            &self._effect.record,
        )?;
        let effect = self._effect.complete_parent_durability_after_binding(journal)?;
        Ok(UsrRollbackReverseDurableEffectAuthority {
            _effect: effect,
            outcome: RollbackActionOutcome::AlreadySatisfied,
        })
    }
}

impl<'reservation> ReconciledReverseEffect<'reservation, UsrRollbackReverseAppliedNamespace> {
    fn complete_parent_durability_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<DurableReverseEffect<'reservation>, UsrRollbackReverseAuthorityError> {
        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        } = self;

        require_pre_namespace_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;
        let namespace_result = namespace.complete_parent_durability(&installation, &record);
        let trailing_evidence = require_post_namespace_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        );
        let namespace = namespace_result?;
        trailing_evidence?;

        Ok(DurableReverseEffect {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        })
    }
}

impl<'reservation> ReconciledReverseEffect<'reservation, UsrRollbackReverseAlreadySatisfiedNamespace> {
    fn complete_parent_durability_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<DurableReverseEffect<'reservation>, UsrRollbackReverseAuthorityError> {
        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        } = self;

        require_pre_namespace_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;
        let namespace_result = namespace.complete_parent_durability(&installation, &record);
        let trailing_evidence = require_post_namespace_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        );
        let namespace = namespace_result?;
        trailing_evidence?;

        Ok(DurableReverseEffect {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        })
    }
}

#[cfg(test)]
impl UsrRollbackReverseDurableEffectAuthority<'_> {
    pub(in crate::client) fn outcome_for_test(&self) -> RollbackActionOutcome {
        self.outcome
    }
}

#[cfg(test)]
mod tests;
