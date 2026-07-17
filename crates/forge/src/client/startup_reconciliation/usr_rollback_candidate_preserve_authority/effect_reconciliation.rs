//! Sealed consumption of one exact NewState candidate-preservation move lease.
//!
//! The journal binding is checked before any retained evidence. The by-value
//! path then surrounds namespace use with exact installation, database,
//! journal, and plan checks. A non-applied, ambiguous, or rejected result
//! returns no descriptor, lease, or retry authority.

mod post_move_durability;

use crate::{
    Installation, db,
    transition_journal::{TransitionJournalBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    DatabaseEvidence, UsrRollbackCandidatePreserveAuthorityError, UsrRollbackNewStateCandidatePreserveEffect,
    UsrRollbackNewStateCandidatePreserveEffectLease,
    effect_evidence::{require_effect_binding, require_post_effect_evidence, require_pre_effect_evidence},
};
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::activation_namespace::{
        UsrRollbackNewStateCandidatePreserveAppliedNamespace,
        UsrRollbackNewStateCandidatePreserveNamespaceApplyReconciliation,
    },
    startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
};

#[cfg(test)]
pub(in crate::client) use post_move_durability::arm_before_usr_rollback_candidate_preserve_durable_trailing_evidence;
pub(in crate::client) use post_move_durability::{
    UsrRollbackCandidatePreserveFinishDurabilitySelection,
    UsrRollbackNewStateCandidatePreserveAlreadySatisfiedEffectAuthority,
    UsrRollbackNewStateCandidatePreserveDurableEffectAuthority,
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
/// Only the distinct consuming durability child can consume this authority;
/// no persistence API exists at this checkpoint.
#[must_use = "an applied candidate-preservation move still requires post-move durability"]
pub(in crate::client) struct UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority<'reservation> {
    _effect:
        ReconciledNewStateCandidatePreserveEffect<'reservation, UsrRollbackNewStateCandidatePreserveAppliedNamespace>,
}

/// Complete authority retained for the post-move durability checkpoint.
struct ReconciledNewStateCandidatePreserveEffect<'reservation, Namespace> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: Namespace,
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
        require_effect_binding(&self.effect.journal_binding, journal)?;
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
        let prepared_namespace = namespace.prepare_move(&installation, &record);
        if prepared_namespace.is_err() {
            // Preserve the existing trailing evidence observation even when
            // final namespace preparation fails before the move authority is
            // produced. The namespace error remains the primary result.
            let _ = require_post_effect_evidence(&installation, &state_db, &record, &database, journal);
        }
        let prepared_namespace = prepared_namespace?;

        // Namespace preparation runs candidate, target, and quarantine-parent
        // barriers plus a fresh PRE1 capture. Repeat the binding-first
        // non-namespace sandwich afterward so a journal or database change
        // during that work cannot reach rename.
        require_effect_binding(&journal_binding, journal)?;
        require_pre_effect_evidence(&installation, &state_db, &record, &database, journal)?;

        let namespace_result = prepared_namespace.reconcile_move(&installation, &record);
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

#[cfg(test)]
mod tests;
