//! One-attempt semantic reconciliation for fresh-database invalidation.
//!
//! Present authority makes exactly one call to the exact database substrate.
//! Joint absence makes none. Only a proved applied or already-satisfied result
//! retains capability for the later persistence checkpoint; known non-apply
//! and ambiguous outcomes are fieldless.

mod persistence;

use crate::{
    Installation, db,
    transition_journal::{RollbackActionOutcome, TransitionJournalBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    DatabaseEvidence, DatabaseInspection, FreshDbInvalidationDatabaseKind,
    UsrRollbackFreshDbInvalidationApplyAuthority, UsrRollbackFreshDbInvalidationAuthority,
    UsrRollbackFreshDbInvalidationAuthorityError, UsrRollbackFreshDbInvalidationAuthorityErrorKind,
    UsrRollbackFreshDbInvalidationDatabaseEvidence, UsrRollbackFreshDbInvalidationFinishAuthority,
    fresh_db_invalidation_plan_is_exact, inspect_current_database,
};
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::UsrRollbackFreshDbInvalidationNamespaceProof,
    startup_recovery::UsrRollbackFreshDbInvalidationEffectSeal,
};

/// Result of consuming one Present authority.
///
/// Only `Applied` retains capability. The other outcomes are deliberately
/// fieldless so neither can retry or be persisted as success.
#[must_use = "a consumed fresh-database invalidation authority must be handled"]
pub(in crate::client) enum UsrRollbackFreshDbInvalidationApplyReconciliation<'reservation> {
    Applied(UsrRollbackFreshDbInvalidationEffectAuthority<'reservation>),
    NotApplied,
    Ambiguous,
}

/// Opaque, absence-bound authority for the later persistence checkpoint.
#[must_use = "successful fresh-database invalidation still requires persistence"]
pub(in crate::client) struct UsrRollbackFreshDbInvalidationEffectAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    #[allow(dead_code)] // retained as non-authorizing pre-effect diagnostic context
    before_database: DatabaseEvidence,
    database: UsrRollbackFreshDbInvalidationDatabaseEvidence,
    namespace: UsrRollbackFreshDbInvalidationNamespaceProof,
    journal_binding: TransitionJournalBinding,
    origin: RollbackActionOutcome,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> UsrRollbackFreshDbInvalidationApplyAuthority<'reservation> {
    /// Consume Present authority through at most one exact removal call.
    pub(in crate::client) fn reconcile(
        self,
        _effect_seal: &UsrRollbackFreshDbInvalidationEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackFreshDbInvalidationApplyReconciliation<'reservation>,
        UsrRollbackFreshDbInvalidationAuthorityError,
    > {
        reset_removal_call_count();
        // This binding check is intentionally the first evidence observation.
        if !journal.has_binding(&self.evidence.journal_binding) {
            return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.evidence
            .revalidate(journal, FreshDbInvalidationDatabaseKind::Present)?;
        self.evidence.reconcile_apply_after_revalidation(journal)
    }
}

impl<'reservation> UsrRollbackFreshDbInvalidationFinishAuthority<'reservation> {
    /// Consume joint absence without calling the removal substrate.
    pub(in crate::client) fn reconcile(
        self,
        _effect_seal: &UsrRollbackFreshDbInvalidationEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackFreshDbInvalidationEffectAuthority<'reservation>, UsrRollbackFreshDbInvalidationAuthorityError>
    {
        reset_removal_call_count();
        // This binding check is intentionally the first evidence observation.
        if !journal.has_binding(&self.evidence.journal_binding) {
            return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.evidence
            .revalidate(journal, FreshDbInvalidationDatabaseKind::JointlyAbsent)?;
        self.evidence.into_already_satisfied_effect()
    }
}

impl<'reservation> UsrRollbackFreshDbInvalidationAuthority<'reservation> {
    fn reconcile_apply_after_revalidation(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackFreshDbInvalidationApplyReconciliation<'reservation>,
        UsrRollbackFreshDbInvalidationAuthorityError,
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
        let UsrRollbackFreshDbInvalidationDatabaseEvidence::Present { context, preimage } = database else {
            return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::EvidenceMismatch.into());
        };

        increment_removal_call_count();
        match state_db.remove_exact_fresh_transition(preimage) {
            Ok(absence) => {
                let database = capture_bound_absence(
                    &installation,
                    &state_db,
                    &record,
                    &namespace,
                    &journal_binding,
                    journal,
                    &absence,
                )?;
                Ok(UsrRollbackFreshDbInvalidationApplyReconciliation::Applied(
                    UsrRollbackFreshDbInvalidationEffectAuthority {
                        installation,
                        state_db,
                        record,
                        before_database: context,
                        database,
                        namespace,
                        journal_binding,
                        origin: RollbackActionOutcome::Applied,
                        _active_state_reservation,
                    },
                ))
            }
            Err(source) => {
                revalidate_trailing_non_database(&installation, &record, &namespace, &journal_binding, journal)?;
                Ok(match source.outcome() {
                    db::state::ExactFreshTransitionRemovalOutcome::DefinitelyNotApplied => {
                        UsrRollbackFreshDbInvalidationApplyReconciliation::NotApplied
                    }
                    db::state::ExactFreshTransitionRemovalOutcome::Ambiguous => {
                        UsrRollbackFreshDbInvalidationApplyReconciliation::Ambiguous
                    }
                })
            }
        }
    }

    fn into_already_satisfied_effect(
        self,
    ) -> Result<UsrRollbackFreshDbInvalidationEffectAuthority<'reservation>, UsrRollbackFreshDbInvalidationAuthorityError>
    {
        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_binding,
            _active_state_reservation,
        } = self;
        let context = match &database {
            UsrRollbackFreshDbInvalidationDatabaseEvidence::JointlyAbsent { context, .. } => context.clone(),
            UsrRollbackFreshDbInvalidationDatabaseEvidence::Present { .. } => {
                return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::EvidenceMismatch.into());
            }
        };
        Ok(UsrRollbackFreshDbInvalidationEffectAuthority {
            installation,
            state_db,
            record,
            before_database: context,
            database,
            namespace,
            journal_binding,
            origin: RollbackActionOutcome::AlreadySatisfied,
            _active_state_reservation,
        })
    }
}

fn capture_bound_absence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    namespace: &UsrRollbackFreshDbInvalidationNamespaceProof,
    journal_binding: &TransitionJournalBinding,
    journal: &TransitionJournalStore,
    applied_absence: &db::state::ExactFreshTransitionAbsence,
) -> Result<UsrRollbackFreshDbInvalidationDatabaseEvidence, UsrRollbackFreshDbInvalidationAuthorityError> {
    if !journal.has_binding(journal_binding) {
        return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::JournalBindingMismatch.into());
    }
    installation.revalidate_mutable_namespace()?;
    let database_before = require_joint_absence(inspect_current_database(record, state_db)?, applied_absence)?;
    namespace.revalidate(installation, journal, record)?;
    let database_after = require_joint_absence(inspect_current_database(record, state_db)?, applied_absence)?;
    if database_before != database_after || !fresh_db_invalidation_plan_is_exact(record) {
        return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::EvidenceMismatch.into());
    }
    installation.revalidate_mutable_namespace()?;
    Ok(database_after)
}

/// Revalidate every non-database input after a one-shot attempt whose database
/// result is not authoritative enough to retain. The journal binding remains
/// the first observation; the database's post-attempt state belongs solely to
/// the exact substrate classification.
fn revalidate_trailing_non_database(
    installation: &Installation,
    record: &TransitionRecord,
    namespace: &UsrRollbackFreshDbInvalidationNamespaceProof,
    journal_binding: &TransitionJournalBinding,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackFreshDbInvalidationAuthorityError> {
    if !journal.has_binding(journal_binding) {
        return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::JournalBindingMismatch.into());
    }
    installation.revalidate_mutable_namespace()?;
    namespace.revalidate(installation, journal, record)?;
    if !fresh_db_invalidation_plan_is_exact(record) {
        return Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::EvidenceMismatch.into());
    }
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn require_joint_absence(
    actual: DatabaseInspection,
    applied_absence: &db::state::ExactFreshTransitionAbsence,
) -> Result<UsrRollbackFreshDbInvalidationDatabaseEvidence, UsrRollbackFreshDbInvalidationAuthorityError> {
    match actual {
        DatabaseInspection::Exact(actual) => {
            let matches_applied_absence = matches!(
                &actual,
                UsrRollbackFreshDbInvalidationDatabaseEvidence::JointlyAbsent { absence, .. }
                    if absence == applied_absence
            );
            if matches_applied_absence {
                Ok(actual)
            } else {
                Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::DatabaseChanged.into())
            }
        }
        DatabaseInspection::Incompatible(evidence) => {
            Err(UsrRollbackFreshDbInvalidationAuthorityErrorKind::DatabaseIncompatible {
                evidence: Box::new(evidence),
            }
            .into())
        }
    }
}

impl UsrRollbackFreshDbInvalidationEffectAuthority<'_> {
    #[cfg(test)]
    pub(in crate::client) fn origin_for_test(&self) -> RollbackActionOutcome {
        self.origin
    }
}

#[cfg(test)]
std::thread_local! {
    static REMOVAL_CALL_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_removal_call_count() {
    REMOVAL_CALL_COUNT.with(|count| count.set(0));
}

#[cfg(not(test))]
fn reset_removal_call_count() {}

#[cfg(test)]
fn increment_removal_call_count() {
    REMOVAL_CALL_COUNT.with(|count| count.set(count.get() + 1));
}

#[cfg(not(test))]
fn increment_removal_call_count() {}

#[cfg(test)]
pub(in crate::client) fn fresh_db_invalidation_removal_call_count() -> usize {
    REMOVAL_CALL_COUNT.with(std::cell::Cell::get)
}
