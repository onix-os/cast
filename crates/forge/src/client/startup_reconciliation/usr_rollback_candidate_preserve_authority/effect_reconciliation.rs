//! Sealed consumption of one exact NewState candidate-preservation move lease.
//!
//! The journal binding is checked before any retained evidence. The by-value
//! path then surrounds namespace use with exact installation, database,
//! journal, and plan checks. A non-applied, ambiguous, or rejected result
//! returns no descriptor, lease, or retry authority.

use crate::{
    Installation, db,
    transition_journal::{Operation, TransitionJournalBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    DatabaseEvidence, UsrRollbackCandidatePreserveAuthorityError, UsrRollbackCandidatePreserveAuthorityErrorKind,
    UsrRollbackNewStateCandidatePreserveEffect, UsrRollbackNewStateCandidatePreserveEffectLease,
    candidate_preserve_plan_is_exact, inspect_current_database, require_exact_database,
};
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::activation_namespace::{
        UsrRollbackNewStateCandidatePreserveAppliedNamespace,
        UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation,
    },
    startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
};

/// Semantic result of consuming one exact empty-prefix NewState move lease.
///
/// Only `Applied` retains capability. The failure variants are fieldless so
/// neither an uncertain nor a known-unapplied one-shot attempt can be retried
/// with stale authority.
#[must_use = "a consumed NewState candidate-preservation move must be handled"]
pub(in crate::client) enum UsrRollbackNewStateCandidatePreserveApplyReconciliation<'reservation> {
    Applied(UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority<'reservation>),
    NotApplied,
    Ambiguous,
}

/// Opaque authority retained only after fresh namespace evidence proves that
/// this invocation applied the exact staging-to-quarantine candidate move.
///
/// This checkpoint intentionally supplies no durability or persistence API.
#[must_use = "an applied candidate-preservation move still requires durability"]
pub(in crate::client) struct UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority<'reservation> {
    _effect: ReconciledNewStateCandidatePreserveEffect<'reservation>,
}

/// Complete authority retained for the later durability checkpoint.
struct ReconciledNewStateCandidatePreserveEffect<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackNewStateCandidatePreserveAppliedNamespace,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> UsrRollbackNewStateCandidatePreserveEffectLease<'reservation> {
    /// Consume the lease through exactly one namespace-owned move attempt and
    /// classify only fresh semantic evidence.
    pub(in crate::client) fn reconcile(
        self,
        _effect_seal: &UsrRollbackCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackNewStateCandidatePreserveApplyReconciliation<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        // The per-open binding is intentionally the first evidence
        // observation. A mixed store cannot trigger namespace revalidation or
        // the candidate move.
        if !journal.has_binding(&self.effect.journal_binding) {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.effect.reconcile_after_binding(journal)
    }
}

impl<'reservation> UsrRollbackNewStateCandidatePreserveEffect<'reservation> {
    fn reconcile_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackNewStateCandidatePreserveApplyReconciliation<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_binding,
            _active_state_reservation,
        } = self;

        require_pre_effect_evidence(&installation, &state_db, &record, &database, journal)?;
        let namespace_result = namespace.reconcile_move(&installation, &record);
        let trailing_evidence = require_post_effect_evidence(&installation, &state_db, &record, &database, journal);
        let namespace_result = namespace_result?;
        trailing_evidence?;

        Ok(match namespace_result {
            UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation::Applied(namespace) => {
                UsrRollbackNewStateCandidatePreserveApplyReconciliation::Applied(
                    UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority {
                        _effect: ReconciledNewStateCandidatePreserveEffect {
                            installation,
                            state_db,
                            record,
                            database,
                            namespace,
                            journal_binding,
                            _active_state_reservation,
                        },
                    },
                )
            }
            UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation::NotApplied => {
                UsrRollbackNewStateCandidatePreserveApplyReconciliation::NotApplied
            }
            UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation::Ambiguous => {
                UsrRollbackNewStateCandidatePreserveApplyReconciliation::Ambiguous
            }
        })
    }
}

fn require_pre_effect_evidence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    expected_database: &DatabaseEvidence,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    installation.revalidate_mutable_namespace()?;
    require_exact_database(expected_database, inspect_current_database(record, state_db)?)?;
    require_exact_journal(journal, record)?;
    require_exact_new_state_move_plan(record)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn require_post_effect_evidence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    expected_database: &DatabaseEvidence,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    installation.revalidate_mutable_namespace()?;
    require_exact_journal(journal, record)?;
    require_exact_new_state_move_plan(record)?;
    require_exact_database(expected_database, inspect_current_database(record, state_db)?)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    match journal.load() {
        Ok(Some(actual)) if actual == *expected => Ok(()),
        Ok(Some(_)) | Ok(None) => {
            Err(UsrRollbackCandidatePreserveAuthorityErrorKind::JournalChangedDuringEffect.into())
        }
        Err(source) => Err(UsrRollbackCandidatePreserveAuthorityErrorKind::JournalReadDuringEffect(source).into()),
    }
}

fn require_exact_new_state_move_plan(
    record: &TransitionRecord,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    if record.operation == Operation::NewState && candidate_preserve_plan_is_exact(record) {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into())
    }
}

#[cfg(test)]
mod tests;
