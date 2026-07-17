//! Sealed consumption of one exact ActiveReblit wrapper exchange.

mod post_exchange_durability;

pub(in crate::client) use post_exchange_durability::UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority;
#[cfg(test)]
pub(in crate::client) use post_exchange_durability::{
    arm_before_active_reblit_candidate_preserve_durable_trailing_evidence,
    arm_before_active_reblit_candidate_preserve_persistence_durable_trailing_evidence,
};

use crate::{
    Installation, db,
    transition_journal::{Operation, TransitionJournalBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    DatabaseEvidence, UsrRollbackCandidatePreserveAuthority, UsrRollbackCandidatePreserveAuthorityError,
    UsrRollbackCandidatePreserveAuthorityErrorKind, UsrRollbackCandidatePreserveFinishAuthority,
    UsrRollbackCandidatePreserveTopology, candidate_preserve_plan_is_exact, effect_evidence::require_effect_binding,
    inspect_current_database, require_exact_database,
};
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::activation_namespace::{
        UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedNamespace,
        UsrRollbackActiveReblitCandidatePreserveAppliedNamespace,
        UsrRollbackActiveReblitCandidatePreserveNamespaceApplyReconciliation,
        UsrRollbackActiveReblitCandidatePreserveNamespaceEffectEvidence,
    },
    startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
};

struct UsrRollbackActiveReblitCandidatePreserveEffect<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackActiveReblitCandidatePreserveNamespaceEffectEvidence,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Opaque one-shot ActiveReblit wrapper-exchange capability.
#[must_use = "an ActiveReblit candidate-preservation lease must be reconciled"]
pub(in crate::client) struct UsrRollbackActiveReblitCandidatePreserveEffectLease<'reservation> {
    effect: UsrRollbackActiveReblitCandidatePreserveEffect<'reservation>,
}

struct ReconciledActiveReblitCandidatePreserveEffect<'reservation, Namespace> {
    _installation: Installation,
    _state_db: db::state::Database,
    _record: TransitionRecord,
    _database: DatabaseEvidence,
    _namespace: Namespace,
    _journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Opaque authority retained only after fresh evidence proves application.
#[must_use = "applied ActiveReblit preservation still requires post-exchange durability"]
pub(in crate::client) struct UsrRollbackActiveReblitCandidatePreserveAppliedEffectAuthority<'reservation> {
    _effect: ReconciledActiveReblitCandidatePreserveEffect<
        'reservation,
        UsrRollbackActiveReblitCandidatePreserveAppliedNamespace,
    >,
}

/// Opaque authority retained from exact Finish evidence without an exchange.
#[must_use = "already-preserved ActiveReblit evidence still requires post-exchange durability"]
pub(in crate::client) struct UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation> {
    _effect: ReconciledActiveReblitCandidatePreserveEffect<
        'reservation,
        UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedNamespace,
    >,
}

/// Semantic result of consuming the sealed one-shot effect.
#[must_use = "a consumed ActiveReblit wrapper exchange must be handled"]
pub(in crate::client) enum UsrRollbackActiveReblitCandidatePreserveApplyReconciliation<'reservation> {
    Applied(UsrRollbackActiveReblitCandidatePreserveAppliedEffectAuthority<'reservation>),
    NotApplied,
    Ambiguous,
}

impl<'reservation> UsrRollbackCandidatePreserveAuthority<'reservation> {
    /// Convert already revalidated generic admission into exact ActiveReblit
    /// effect evidence without exposing the private wrapper index.
    pub(super) fn into_active_reblit_effect_after_revalidation(
        self,
        wrapper_index: usize,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveEffectLease<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        if self.namespace.topology() != (UsrRollbackCandidatePreserveTopology::ActiveReblitStaged { wrapper_index }) {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into());
        }
        let UsrRollbackCandidatePreserveAuthority {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_binding,
            _active_state_reservation,
        } = self;
        let namespace = namespace.into_active_reblit_apply_effect_evidence(&record, wrapper_index)?;
        Ok(UsrRollbackActiveReblitCandidatePreserveEffectLease {
            effect: UsrRollbackActiveReblitCandidatePreserveEffect {
                installation,
                state_db,
                record,
                database,
                namespace,
                journal_binding,
                _active_state_reservation,
            },
        })
    }
}

impl<'reservation> UsrRollbackCandidatePreserveFinishAuthority<'reservation> {
    /// Convert already revalidated Finish admission into exact ActiveReblit
    /// POST evidence without an exchange attempt.
    pub(super) fn into_active_reblit_finish_after_revalidation(
        self,
        journal: &TransitionJournalStore,
        wrapper_index: usize,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        let evidence = self.evidence;
        if evidence.namespace.topology()
            != (UsrRollbackCandidatePreserveTopology::ActiveReblitPreserved { wrapper_index })
        {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into());
        }
        let UsrRollbackCandidatePreserveAuthority {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_binding,
            _active_state_reservation,
        } = evidence;
        require_active_reblit_pre_effect_evidence(&installation, &state_db, &record, &database, journal)?;
        let namespace = namespace
            .into_active_reblit_finish_effect_evidence(&record, wrapper_index)?
            .reconcile_finish(&installation, &record)?;
        require_active_reblit_post_effect_evidence(&installation, &state_db, &record, &database, journal)?;
        Ok(
            UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority {
                _effect: ReconciledActiveReblitCandidatePreserveEffect {
                    _installation: installation,
                    _state_db: state_db,
                    _record: record,
                    _database: database,
                    _namespace: namespace,
                    _journal_binding: journal_binding,
                    _active_state_reservation,
                },
            },
        )
    }
}

impl<'reservation> UsrRollbackActiveReblitCandidatePreserveEffectLease<'reservation> {
    /// Consume the lease through exactly one namespace-owned exchange attempt.
    pub(in crate::client) fn reconcile(
        self,
        _effect_seal: &UsrRollbackCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveApplyReconciliation<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        require_effect_binding(&self.effect.journal_binding, journal)?;
        self.effect.reconcile_after_binding(journal)
    }
}

impl<'reservation> UsrRollbackActiveReblitCandidatePreserveEffect<'reservation> {
    fn reconcile_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveApplyReconciliation<'reservation>,
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
        require_active_reblit_pre_effect_evidence(&installation, &state_db, &record, &database, journal)?;
        let prepared = namespace.prepare_exchange(&installation, &record);
        if prepared.is_err() {
            let _ = require_active_reblit_post_effect_evidence(&installation, &state_db, &record, &database, journal);
        }
        let prepared = prepared?;
        require_effect_binding(&journal_binding, journal)?;
        require_active_reblit_pre_effect_evidence(&installation, &state_db, &record, &database, journal)?;
        let namespace_result = prepared.reconcile_exchange(&installation, &record);
        let trailing =
            require_active_reblit_post_effect_evidence(&installation, &state_db, &record, &database, journal);
        let namespace_result = namespace_result?;
        trailing?;
        Ok(match namespace_result {
            UsrRollbackActiveReblitCandidatePreserveNamespaceApplyReconciliation::Applied(namespace) => {
                UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::Applied(
                    UsrRollbackActiveReblitCandidatePreserveAppliedEffectAuthority {
                        _effect: ReconciledActiveReblitCandidatePreserveEffect {
                            _installation: installation,
                            _state_db: state_db,
                            _record: record,
                            _database: database,
                            _namespace: namespace,
                            _journal_binding: journal_binding,
                            _active_state_reservation,
                        },
                    },
                )
            }
            UsrRollbackActiveReblitCandidatePreserveNamespaceApplyReconciliation::NotApplied => {
                UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::NotApplied
            }
            UsrRollbackActiveReblitCandidatePreserveNamespaceApplyReconciliation::Ambiguous => {
                UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::Ambiguous
            }
        })
    }
}

fn require_active_reblit_pre_effect_evidence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    expected_database: &DatabaseEvidence,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    installation.revalidate_mutable_namespace()?;
    require_exact_database(expected_database, inspect_current_database(record, state_db)?)?;
    require_exact_active_reblit_journal(journal, record)?;
    require_exact_active_reblit_plan(record)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn require_active_reblit_post_effect_evidence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    expected_database: &DatabaseEvidence,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    installation.revalidate_mutable_namespace()?;
    require_exact_active_reblit_journal(journal, record)?;
    require_exact_active_reblit_plan(record)?;
    require_exact_database(expected_database, inspect_current_database(record, state_db)?)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn require_exact_active_reblit_journal(
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

fn require_exact_active_reblit_plan(
    record: &TransitionRecord,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    if record.operation == Operation::ActiveReblit && candidate_preserve_plan_is_exact(record) {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into())
    }
}
