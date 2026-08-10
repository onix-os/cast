//! Sealed one-attempt consumption of an absent ActiveReblit reservation lease.
//!
//! Binding-first non-namespace evidence surrounds final namespace preparation
//! and the attempt. A reserved wrapper returns only an opaque, one-use
//! unchanged-source authority; it cannot retry the reservation or fall through
//! into the exchange. Unapplied and ambiguous results remain fieldless.

use crate::transition_journal::TransitionJournalStore;

use super::{
    UsrRollbackActiveReblitCandidatePreserveWrapperReservationEffect,
    UsrRollbackActiveReblitCandidatePreserveWrapperReservationLease, UsrRollbackCandidatePreserveAuthority,
    UsrRollbackCandidatePreserveAuthorityError, UsrRollbackCandidatePreserveRestartAuthority,
    active_reblit_effect::{require_active_reblit_post_effect_evidence, require_active_reblit_pre_effect_evidence},
    effect_evidence::require_effect_binding,
};
use crate::client::{
    startup_reconciliation::activation_namespace::UsrRollbackActiveReblitWrapperReservationNamespaceReconciliation,
    startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
};

/// Semantic result of consuming exactly one absent-reservation lease.
///
/// `RestartRequired` retains only enough sealed authority to authenticate the
/// unchanged source at the dispatch return boundary. Reclassification on the
/// next pass reaches the staged shape the exchange effect already owns.
#[must_use = "a consumed ActiveReblit reservation lease must be handled"]
pub(in crate::client) enum UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation<'reservation> {
    RestartRequired(UsrRollbackCandidatePreserveRestartAuthority<'reservation>),
    NotApplied,
    Ambiguous,
}

impl<'reservation> UsrRollbackCandidatePreserveAuthority<'reservation> {
    /// Convert already revalidated generic admission into exact absent-reservation
    /// evidence.
    ///
    /// This lives behind its own call frame rather than inline in the selection
    /// match: every retained field here is large, and a debug build gives each
    /// match arm its own stack slots.
    pub(super) fn into_active_reblit_wrapper_reservation_after_revalidation(
        self,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveWrapperReservationLease<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        let UsrRollbackCandidatePreserveAuthority {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        } = self;
        let namespace = namespace.into_active_reblit_wrapper_reservation_evidence(&record)?;
        Ok(UsrRollbackActiveReblitCandidatePreserveWrapperReservationLease {
            effect: Box::new(UsrRollbackActiveReblitCandidatePreserveWrapperReservationEffect {
                installation,
                state_db,
                record,
                database,
                namespace,
                journal_record_binding,
                _active_state_reservation,
            }),
        })
    }
}

impl<'reservation> UsrRollbackActiveReblitCandidatePreserveWrapperReservationLease<'reservation> {
    /// Consume the lease through one namespace-owned attempt and fresh semantic
    /// reconciliation. Possession of the result cannot continue in-process.
    pub(in crate::client) fn reconcile(
        self,
        _effect_seal: &UsrRollbackCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        require_effect_binding(
            &self.effect.installation,
            &self.effect.journal_record_binding,
            &self.effect.record,
            journal,
        )?;
        (*self.effect).reconcile_after_binding(journal)
    }
}

impl<'reservation> UsrRollbackActiveReblitCandidatePreserveWrapperReservationEffect<'reservation> {
    fn reconcile_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation<'reservation>,
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

        require_active_reblit_pre_effect_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;
        let prepared_namespace = namespace.prepare_wrapper_reservation(&installation, &record);
        if prepared_namespace.is_err() {
            let _ = require_active_reblit_post_effect_evidence(
                &installation,
                &state_db,
                &record,
                &database,
                &journal_record_binding,
                journal,
            );
        }
        let prepared_namespace = prepared_namespace?;

        require_effect_binding(&installation, &journal_record_binding, &record, journal)?;
        require_active_reblit_pre_effect_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;

        let namespace_result = prepared_namespace.reconcile_wrapper_reservation(&installation, &record);
        require_active_reblit_post_effect_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;

        Ok(match namespace_result {
            UsrRollbackActiveReblitWrapperReservationNamespaceReconciliation::RestartRequired => {
                UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation::RestartRequired(
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
            UsrRollbackActiveReblitWrapperReservationNamespaceReconciliation::NotApplied => {
                UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation::NotApplied
            }
            UsrRollbackActiveReblitWrapperReservationNamespaceReconciliation::Ambiguous => {
                UsrRollbackActiveReblitCandidatePreserveWrapperReservationReconciliation::Ambiguous
            }
        })
    }
}
