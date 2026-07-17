//! Sealed one-attempt consumption of a restrictive NewState target lease.
//!
//! Binding-first non-namespace evidence surrounds final namespace preparation
//! and the attempt. Every authority result is fieldless: even a safely
//! normalized target completes its sealed target and quarantine-parent
//! durability suffix, then forces a new startup entry without falling through
//! into movement.

use crate::transition_journal::TransitionJournalStore;

use super::{
    UsrRollbackCandidatePreserveAuthorityError, UsrRollbackNewStateCandidatePreserveNormalizeTargetEffect,
    UsrRollbackNewStateCandidatePreserveNormalizeTargetLease,
    effect_evidence::{require_effect_binding, require_post_effect_evidence, require_pre_effect_evidence},
};
use crate::client::{
    startup_reconciliation::activation_namespace::UsrRollbackNewStateTargetNormalizeNamespaceReconciliation,
    startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
};

/// Semantic result of consuming exactly one target-normalization lease.
///
/// No variant retains evidence, descriptors, a retry, or move capability.
/// `RestartRequired` is constructed only after the safe on-disk prefix has
/// completed both durability barriers and a final exact canonical capture; it
/// still does not attribute the mode change to this invocation.
#[must_use = "a consumed NewState normalize-target lease must be handled"]
pub(in crate::client) enum UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation {
    RestartRequired,
    NotApplied,
    Ambiguous,
}

impl<'reservation> UsrRollbackNewStateCandidatePreserveNormalizeTargetLease<'reservation> {
    /// Consume the lease through one descriptor-bound attempt, fresh semantic
    /// reconciliation, and ordered target-then-parent durability. Possession
    /// of the result cannot continue in-process.
    pub(in crate::client) fn reconcile(
        self,
        _effect_seal: &UsrRollbackCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        // A different open store must fail before any retained namespace or
        // database evidence is observed.
        require_effect_binding(&self.effect.journal_binding, journal)?;
        self.effect.reconcile_after_binding(journal)
    }
}

impl<'reservation> UsrRollbackNewStateCandidatePreserveNormalizeTargetEffect<'reservation> {
    fn reconcile_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation,
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
        let prepared_namespace = namespace.prepare_target_normalization(&installation, &record);
        if prepared_namespace.is_err() {
            // Keep the established trailing evidence observation when final
            // namespace PRE fails before an attempt capability exists.
            let _ = require_post_effect_evidence(&installation, &state_db, &record, &database, journal);
        }
        let prepared_namespace = prepared_namespace?;

        // Final PRE capture may be slow. Repeat binding first, then the complete
        // non-namespace PRE immediately before consuming the one-attempt value.
        require_effect_binding(&journal_binding, journal)?;
        require_pre_effect_evidence(&installation, &state_db, &record, &database, journal)?;

        let namespace_result = prepared_namespace.reconcile_target_normalization(&installation, &record);
        require_post_effect_evidence(&installation, &state_db, &record, &database, journal)?;

        Ok(match namespace_result {
            UsrRollbackNewStateTargetNormalizeNamespaceReconciliation::RestartRequired => {
                UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::RestartRequired
            }
            UsrRollbackNewStateTargetNormalizeNamespaceReconciliation::NotApplied => {
                UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::NotApplied
            }
            UsrRollbackNewStateTargetNormalizeNamespaceReconciliation::Ambiguous => {
                UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::Ambiguous
            }
        })
    }
}
