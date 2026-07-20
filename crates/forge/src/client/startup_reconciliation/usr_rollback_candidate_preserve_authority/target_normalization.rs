//! Sealed one-attempt consumption of a restrictive NewState target lease.
//!
//! Binding-first non-namespace evidence surrounds final namespace preparation
//! and the attempt. A safely normalized target completes its sealed target and
//! quarantine-parent durability suffix, then yields only an opaque one-use
//! unchanged-source authority. It cannot retry or fall through into movement;
//! unapplied and ambiguous results remain fieldless.

use crate::transition_journal::TransitionJournalStore;

use super::{
    UsrRollbackCandidatePreserveAuthorityError, UsrRollbackCandidatePreserveRestartAuthority,
    UsrRollbackNewStateCandidatePreserveNormalizeTargetEffect, UsrRollbackNewStateCandidatePreserveNormalizeTargetLease,
    effect_evidence::{require_effect_binding, require_post_effect_evidence, require_pre_effect_evidence},
};
use crate::client::{
    startup_reconciliation::activation_namespace::UsrRollbackNewStateTargetNormalizeNamespaceReconciliation,
    startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
};

/// Semantic result of consuming exactly one target-normalization lease.
///
/// `RestartRequired` retains only enough sealed authority to authenticate the
/// unchanged source at the dispatch return boundary. It exposes no descriptor,
/// retry, or move capability. It is constructed only after the safe on-disk
/// prefix completed both durability barriers and a final exact canonical
/// capture; the uncertainty variants remain fieldless.
#[must_use = "a consumed NewState normalize-target lease must be handled"]
pub(in crate::client) enum UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation<'reservation> {
    RestartRequired(UsrRollbackCandidatePreserveRestartAuthority<'reservation>),
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
        UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation<'reservation>,
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

impl<'reservation> UsrRollbackNewStateCandidatePreserveNormalizeTargetEffect<'reservation> {
    fn reconcile_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation<'reservation>,
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
        let prepared_namespace = namespace.prepare_target_normalization(&installation, &record);
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

        let namespace_result = prepared_namespace.reconcile_target_normalization(&installation, &record);
        require_post_effect_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;

        Ok(match namespace_result {
            UsrRollbackNewStateTargetNormalizeNamespaceReconciliation::RestartRequired => {
                UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::RestartRequired(
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
            UsrRollbackNewStateTargetNormalizeNamespaceReconciliation::NotApplied => {
                UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::NotApplied
            }
            UsrRollbackNewStateTargetNormalizeNamespaceReconciliation::Ambiguous => {
                UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::Ambiguous
            }
        })
    }
}
