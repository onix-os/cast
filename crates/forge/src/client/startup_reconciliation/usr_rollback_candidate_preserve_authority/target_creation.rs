//! Sealed one-attempt consumption of an absent NewState target lease.
//!
//! Binding-first non-namespace evidence surrounds final namespace preparation
//! and the attempt. A safe prepared target returns only an opaque, one-use
//! unchanged-source authority; it cannot retry creation or fall through into
//! movement. Unapplied and ambiguous results remain fieldless.

use crate::transition_journal::TransitionJournalStore;

use super::{
    UsrRollbackCandidatePreserveAuthorityError, UsrRollbackCandidatePreserveRestartAuthority,
    UsrRollbackNewStateCandidatePreserveCreateTargetEffect, UsrRollbackNewStateCandidatePreserveCreateTargetLease,
    effect_evidence::{require_effect_binding, require_post_effect_evidence, require_pre_effect_evidence},
};
use crate::client::{
    startup_reconciliation::activation_namespace::UsrRollbackNewStateTargetCreateNamespaceReconciliation,
    startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
};

/// Semantic result of consuming exactly one absent-target creation lease.
///
/// `RestartRequired` retains only enough sealed authority to authenticate the
/// unchanged source at the dispatch return boundary. It exposes no descriptor,
/// retry, normalization, or move capability. The uncertainty variants remain
/// fieldless.
#[must_use = "a consumed NewState create-target lease must be handled"]
pub(in crate::client) enum UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation<'reservation> {
    RestartRequired(UsrRollbackCandidatePreserveRestartAuthority<'reservation>),
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
        UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        // A different open store must fail before any retained namespace or
        // database evidence is observed.
        require_effect_binding(
            &self.effect.installation,
            &self.effect.journal_record_binding,
            &self.effect.record,
            journal,
        )?;
        self.effect.reconcile_after_binding(journal)
    }
}

impl<'reservation> UsrRollbackNewStateCandidatePreserveCreateTargetEffect<'reservation> {
    fn reconcile_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        } = self;

        require_pre_effect_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;
        let prepared_namespace = namespace.prepare_target_creation(&installation, &record);
        if prepared_namespace.is_err() {
            // Keep the established trailing evidence observation when final
            // namespace PRE fails before an attempt capability exists.
            let _ = require_post_effect_evidence(
                &installation,
                &state_db,
                &record,
                &database,
                &journal_record_binding,
                journal,
            );
        }
        let prepared_namespace = prepared_namespace?;

        // Final PRE capture may be slow. Repeat binding first, then the complete
        // non-namespace PRE immediately before consuming the one-attempt value.
        require_effect_binding(&installation, &journal_record_binding, &record, journal)?;
        require_pre_effect_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;

        let namespace_result = prepared_namespace.reconcile_target_creation(&installation, &record);
        require_post_effect_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;

        Ok(match namespace_result {
            UsrRollbackNewStateTargetCreateNamespaceReconciliation::RestartRequired => {
                UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired(
                    UsrRollbackCandidatePreserveRestartAuthority {
                        installation,
                        state_db,
                        record,
                        database,
                        journal_record_binding,
                        _active_state_reservation,
                    },
                )
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
