//! Sealed one-attempt consumption of an absent NewState target lease.
//!
//! Binding-first non-namespace evidence surrounds final namespace preparation
//! and the attempt. Every semantic result is fieldless: even a safe prepared
//! target forces a new startup entry and cannot fall through into movement.

use crate::transition_journal::TransitionJournalStore;

use super::{
    UsrRollbackCandidatePreserveAuthorityError, UsrRollbackNewStateCandidatePreserveCreateTargetEffect,
    UsrRollbackNewStateCandidatePreserveCreateTargetLease,
    effect_evidence::{require_effect_binding, require_post_effect_evidence, require_pre_effect_evidence},
};
use crate::client::{
    startup_reconciliation::activation_namespace::UsrRollbackNewStateTargetCreateNamespaceReconciliation,
    startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
};

/// Semantic result of consuming exactly one absent-target creation lease.
///
/// No variant retains evidence, descriptors, a retry, normalization, or move
/// capability. `RestartRequired` describes the safe on-disk prefix, not the raw
/// operation report and not proof that this invocation created the target.
#[must_use = "a consumed NewState create-target lease must be handled"]
pub(in crate::client) enum UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation {
    RestartRequired,
    NotApplied,
    Ambiguous,
}

impl<'reservation> UsrRollbackNewStateCandidatePreserveCreateTargetLease<'reservation> {
    /// Consume the lease through one namespace-owned attempt and fresh semantic
    /// reconciliation. Possession of the result cannot continue in-process.
    pub(in crate::client) fn reconcile(
        self,
        _effect_seal: &UsrRollbackCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        // A different open store must fail before any retained namespace or
        // database evidence is observed.
        require_effect_binding(&self.effect.journal_binding, journal)?;
        self.effect.reconcile_after_binding(journal)
    }
}

impl<'reservation> UsrRollbackNewStateCandidatePreserveCreateTargetEffect<'reservation> {
    fn reconcile_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation,
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
        let prepared_namespace = namespace.prepare_target_creation(&installation, &record);
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

        let namespace_result = prepared_namespace.reconcile_target_creation(&installation, &record);
        require_post_effect_evidence(&installation, &state_db, &record, &database, journal)?;

        Ok(match namespace_result {
            UsrRollbackNewStateTargetCreateNamespaceReconciliation::RestartRequired => {
                UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired
            }
            UsrRollbackNewStateTargetCreateNamespaceReconciliation::NotApplied => {
                UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::NotApplied
            }
            UsrRollbackNewStateTargetCreateNamespaceReconciliation::Ambiguous => {
                UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::Ambiguous
            }
        })
    }
}
