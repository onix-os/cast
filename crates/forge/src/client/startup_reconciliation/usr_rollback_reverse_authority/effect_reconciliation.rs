//! Sealed consumption of rollback-reverse effect leases.
//!
//! The journal binding is always checked before any retained database or
//! namespace evidence. Both by-value paths then surround namespace use with
//! exact installation, database, and journal checks. No descriptor or retry
//! capability escapes a non-applied result.

mod durability;

use crate::{
    Installation, db,
    transition_journal::{TransitionJournalBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    DatabaseEvidence, UsrRollbackReverseApplyEffectLease, UsrRollbackReverseAuthorityError,
    UsrRollbackReverseAuthorityErrorKind, UsrRollbackReverseEffectLease, UsrRollbackReverseFinishEffectLease,
    inspect_current_database, require_exact_database, reverse_plan_is_exact,
};
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::activation_namespace::{
        UsrRollbackReverseAlreadySatisfiedNamespace, UsrRollbackReverseAppliedNamespace,
        UsrRollbackReverseNamespaceApplyReconciliation,
    },
    startup_recovery::UsrRollbackReverseEffectSeal,
};

pub(in crate::client) use durability::UsrRollbackReverseDurableEffectAuthority;

/// Result of consuming one POST effect lease.
///
/// Only `Applied` retains capability. The other variants are fieldless so a
/// caller cannot retry an uncertain or known-unapplied one-shot attempt.
#[must_use = "a consumed rollback-reverse apply lease must be handled"]
#[allow(dead_code)] // consumed only through the unwired durability executor
pub(in crate::client) enum UsrRollbackReverseApplyReconciliation<'reservation> {
    Applied(UsrRollbackReverseAppliedEffectAuthority<'reservation>),
    NotApplied,
    Ambiguous,
}

/// Opaque authority for the durability suffix after this invocation applied
/// the exact POST-to-PRE exchange.
#[must_use = "an applied rollback-reverse effect still requires durability"]
#[allow(dead_code)] // consumed only through the unwired durability executor
pub(in crate::client) struct UsrRollbackReverseAppliedEffectAuthority<'reservation> {
    _effect: ReconciledReverseEffect<'reservation, UsrRollbackReverseAppliedNamespace>,
}

/// Opaque authority for the durability suffix after this invocation found an
/// exact PRE namespace and made no exchange attempt.
#[must_use = "an already-satisfied rollback-reverse effect still requires durability"]
#[allow(dead_code)] // consumed only through the unwired durability executor
pub(in crate::client) struct UsrRollbackReverseAlreadySatisfiedEffectAuthority<'reservation> {
    _effect: ReconciledReverseEffect<'reservation, UsrRollbackReverseAlreadySatisfiedNamespace>,
}

/// Evidence retained for the later parent-durability and persistence suffix.
/// The namespace parameter keeps Applied and AlreadySatisfied disjoint.
#[allow(dead_code)] // retained behind the unwired durability executor
struct ReconciledReverseEffect<'reservation, Namespace> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: Namespace,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> UsrRollbackReverseApplyEffectLease<'reservation> {
    /// Consume an exact POST lease into one namespace-derived semantic result.
    #[allow(dead_code)] // invoked by the later rollback-reverse executor
    pub(in crate::client) fn reconcile(
        self,
        _effect_seal: &UsrRollbackReverseEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackReverseApplyReconciliation<'reservation>, UsrRollbackReverseAuthorityError> {
        // This binding check is intentionally the first evidence observation.
        if !journal.has_binding(&self.lease.journal_binding) {
            return Err(UsrRollbackReverseAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.lease.reconcile_apply_after_binding(journal)
    }
}

impl<'reservation> UsrRollbackReverseFinishEffectLease<'reservation> {
    /// Consume an exact PRE lease without issuing an exchange attempt.
    #[allow(dead_code)] // invoked by the later rollback-reverse executor
    pub(in crate::client) fn reconcile(
        self,
        _effect_seal: &UsrRollbackReverseEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackReverseAlreadySatisfiedEffectAuthority<'reservation>, UsrRollbackReverseAuthorityError> {
        // This binding check is intentionally the first evidence observation.
        if !journal.has_binding(&self.lease.journal_binding) {
            return Err(UsrRollbackReverseAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.lease.reconcile_finish_after_binding(journal)
    }
}

impl<'reservation> UsrRollbackReverseEffectLease<'reservation> {
    fn reconcile_apply_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackReverseApplyReconciliation<'reservation>, UsrRollbackReverseAuthorityError> {
        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_binding,
            _active_state_reservation,
        } = self;

        require_pre_namespace_evidence(&installation, &state_db, &record, &database, journal)?;
        let namespace_result = namespace.reconcile_apply(&installation, &record);
        let trailing_evidence = require_post_namespace_evidence(&installation, &state_db, &record, &database, journal);
        let namespace_result = namespace_result?;
        trailing_evidence?;

        Ok(match namespace_result {
            UsrRollbackReverseNamespaceApplyReconciliation::Applied(namespace) => {
                UsrRollbackReverseApplyReconciliation::Applied(UsrRollbackReverseAppliedEffectAuthority {
                    _effect: ReconciledReverseEffect {
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
            UsrRollbackReverseNamespaceApplyReconciliation::NotApplied => {
                UsrRollbackReverseApplyReconciliation::NotApplied
            }
            UsrRollbackReverseNamespaceApplyReconciliation::Ambiguous => {
                UsrRollbackReverseApplyReconciliation::Ambiguous
            }
        })
    }

    fn reconcile_finish_after_binding(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackReverseAlreadySatisfiedEffectAuthority<'reservation>, UsrRollbackReverseAuthorityError> {
        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_binding,
            _active_state_reservation,
        } = self;

        require_pre_namespace_evidence(&installation, &state_db, &record, &database, journal)?;
        let namespace_result = namespace.reconcile_finish(&installation, &record);
        let trailing_evidence = require_post_namespace_evidence(&installation, &state_db, &record, &database, journal);
        let namespace = namespace_result?;
        trailing_evidence?;

        Ok(UsrRollbackReverseAlreadySatisfiedEffectAuthority {
            _effect: ReconciledReverseEffect {
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

fn require_pre_namespace_evidence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    expected_database: &DatabaseEvidence,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackReverseAuthorityError> {
    installation.revalidate_mutable_namespace()?;
    require_exact_database(expected_database, inspect_current_database(record, state_db)?)?;
    require_exact_journal(journal, record)?;
    require_exact_reverse_plan(record)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn require_post_namespace_evidence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    expected_database: &DatabaseEvidence,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackReverseAuthorityError> {
    installation.revalidate_mutable_namespace()?;
    require_exact_journal(journal, record)?;
    require_exact_reverse_plan(record)?;
    require_exact_database(expected_database, inspect_current_database(record, state_db)?)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrRollbackReverseAuthorityError> {
    match journal.load() {
        Ok(Some(actual)) if actual == *expected => Ok(()),
        Ok(Some(_)) | Ok(None) => Err(UsrRollbackReverseAuthorityErrorKind::JournalChangedDuringEffect.into()),
        Err(source) => Err(UsrRollbackReverseAuthorityErrorKind::JournalReadDuringEffect(source).into()),
    }
}

fn require_exact_reverse_plan(record: &TransitionRecord) -> Result<(), UsrRollbackReverseAuthorityError> {
    if reverse_plan_is_exact(record) {
        Ok(())
    } else {
        Err(UsrRollbackReverseAuthorityErrorKind::ReverseEvidenceMismatch.into())
    }
}

#[cfg(test)]
mod tests;
