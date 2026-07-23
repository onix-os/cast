//! Sealed authority bridge for one archived candidate child move.
//!
//! Exact staged and already-preserved evidence converge only after their
//! operation-specific durability suffix. The resulting authority fixes its
//! origin privately for the separate journal-persistence boundary.

mod persistence;

#[cfg(test)]
pub(in crate::client) use persistence::arm_before_archived_candidate_preserve_persistence_durable_trailing_evidence;
pub(in crate::client) use persistence::UsrRollbackArchivedCandidatePreserveRecordAdvanceError;

use crate::{
    Installation, db,
    transition_journal::{Operation, TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    DatabaseEvidence, UsrRollbackCandidatePreserveAuthority, UsrRollbackCandidatePreserveAuthorityError,
    UsrRollbackCandidatePreserveAuthorityErrorKind, UsrRollbackCandidatePreserveFinishAuthority,
    UsrRollbackCandidatePreserveTopology, candidate_preserve_plan_is_exact, inspect_current_database,
    require_exact_database,
};
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::{
        UsrRollbackArchivedCandidatePreserveAlreadySatisfiedNamespace,
        UsrRollbackArchivedCandidatePreserveAppliedNamespace, UsrRollbackArchivedCandidatePreserveDurableNamespace,
        UsrRollbackArchivedCandidatePreserveNamespaceApplyReconciliation,
        UsrRollbackArchivedCandidatePreserveNamespaceEffectEvidence,
    },
    startup_recovery::{UsrRollbackArchivedCandidatePreserveDurabilitySeal, UsrRollbackCandidatePreserveEffectSeal},
};

struct ArchivedEffect<'reservation, Namespace> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: Namespace,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[must_use = "an archived candidate move lease must be reconciled"]
pub(in crate::client) struct UsrRollbackArchivedCandidatePreserveEffectLease<'reservation> {
    effect: ArchivedEffect<'reservation, UsrRollbackArchivedCandidatePreserveNamespaceEffectEvidence>,
}

#[must_use = "applied archived candidate authority still requires POST durability"]
pub(in crate::client) struct UsrRollbackArchivedCandidatePreserveAppliedEffectAuthority<'reservation> {
    effect: ArchivedEffect<'reservation, UsrRollbackArchivedCandidatePreserveAppliedNamespace>,
}

#[must_use = "preserved archived candidate authority still requires POST durability"]
pub(in crate::client) struct UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation> {
    effect: ArchivedEffect<'reservation, UsrRollbackArchivedCandidatePreserveAlreadySatisfiedNamespace>,
}

#[must_use = "durable archived candidate authority must remain sealed"]
pub(in crate::client) struct UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'reservation> {
    effect: ArchivedEffect<'reservation, UsrRollbackArchivedCandidatePreserveDurableNamespace>,
    origin: ArchivedDurabilityOrigin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArchivedDurabilityOrigin {
    Applied,
    AlreadySatisfied,
}

#[must_use = "an archived candidate reconciliation must be handled"]
pub(in crate::client) enum UsrRollbackArchivedCandidatePreserveApplyReconciliation<'reservation> {
    Applied(UsrRollbackArchivedCandidatePreserveAppliedEffectAuthority<'reservation>),
    NotApplied,
    Ambiguous,
}

impl<'reservation> UsrRollbackCandidatePreserveAuthority<'reservation> {
    pub(super) fn into_archived_effect_after_revalidation(
        self,
    ) -> Result<UsrRollbackArchivedCandidatePreserveEffectLease<'reservation>, UsrRollbackCandidatePreserveAuthorityError>
    {
        if self.namespace.topology() != UsrRollbackCandidatePreserveTopology::ArchivedStagedWithCanonicalSlot {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into());
        }
        let UsrRollbackCandidatePreserveAuthority {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        } = self;
        let namespace = namespace.into_archived_apply_effect_evidence(&record)?;
        Ok(UsrRollbackArchivedCandidatePreserveEffectLease {
            effect: ArchivedEffect {
                installation,
                state_db,
                record,
                database,
                namespace,
                journal_record_binding,
                _active_state_reservation,
            },
        })
    }
}

impl<'reservation> UsrRollbackCandidatePreserveFinishAuthority<'reservation> {
    pub(super) fn into_archived_finish_after_revalidation(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        if self.evidence.namespace.topology() != UsrRollbackCandidatePreserveTopology::ArchivedPreserved {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into());
        }
        let UsrRollbackCandidatePreserveAuthority {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        } = self.evidence;
        require_archived_pre_effect_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;
        let namespace = namespace
            .into_archived_finish_effect_evidence(&record)?
            .reconcile_finish(&installation, &record)?;
        require_archived_post_effect_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;
        Ok(UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority {
            effect: ArchivedEffect {
                installation,
                state_db,
                record,
                database,
                namespace,
                journal_record_binding,
                _active_state_reservation,
            },
        })
    }
}

impl<'reservation> UsrRollbackArchivedCandidatePreserveEffectLease<'reservation> {
    pub(in crate::client) fn reconcile(
        self,
        _seal: &UsrRollbackCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackArchivedCandidatePreserveApplyReconciliation<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        super::require_journal_record_binding(
            &self.effect.installation,
            journal,
            &self.effect.journal_record_binding,
            &self.effect.record,
        )?;
        let ArchivedEffect {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        } = self.effect;
        require_archived_pre_effect_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;
        let prepared = namespace.prepare_move(&installation, &record);
        if prepared.is_err() {
            let _ = require_archived_post_effect_evidence(
                &installation,
                &state_db,
                &record,
                &database,
                &journal_record_binding,
                journal,
            );
        }
        let prepared = prepared?;
        super::require_journal_record_binding(&installation, journal, &journal_record_binding, &record)?;
        require_archived_pre_effect_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        )?;
        let namespace_result = prepared.reconcile_move(&installation, &record);
        let trailing = require_archived_post_effect_evidence(
            &installation,
            &state_db,
            &record,
            &database,
            &journal_record_binding,
            journal,
        );
        let namespace_result = namespace_result?;
        trailing?;
        Ok(match namespace_result {
            UsrRollbackArchivedCandidatePreserveNamespaceApplyReconciliation::Applied(namespace) => {
                UsrRollbackArchivedCandidatePreserveApplyReconciliation::Applied(
                    UsrRollbackArchivedCandidatePreserveAppliedEffectAuthority {
                        effect: ArchivedEffect {
                            installation,
                            state_db,
                            record,
                            database,
                            namespace,
                            journal_record_binding,
                            _active_state_reservation,
                        },
                    },
                )
            }
            UsrRollbackArchivedCandidatePreserveNamespaceApplyReconciliation::NotApplied => {
                UsrRollbackArchivedCandidatePreserveApplyReconciliation::NotApplied
            }
            UsrRollbackArchivedCandidatePreserveNamespaceApplyReconciliation::Ambiguous => {
                UsrRollbackArchivedCandidatePreserveApplyReconciliation::Ambiguous
            }
        })
    }
}

impl<'reservation> UsrRollbackArchivedCandidatePreserveAppliedEffectAuthority<'reservation> {
    pub(in crate::client) fn complete_post_move_durability(
        self,
        _seal: &UsrRollbackArchivedCandidatePreserveDurabilitySeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        complete_post_move(self.effect, journal, ArchivedDurabilityOrigin::Applied)
    }
}

impl<'reservation> UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation> {
    pub(in crate::client) fn complete_post_move_durability(
        self,
        _seal: &UsrRollbackArchivedCandidatePreserveDurabilitySeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        complete_post_move(self.effect, journal, ArchivedDurabilityOrigin::AlreadySatisfied)
    }
}

fn complete_post_move<'reservation, Namespace>(
    effect: ArchivedEffect<'reservation, Namespace>,
    journal: &TransitionJournalStore,
    origin: ArchivedDurabilityOrigin,
) -> Result<
    UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'reservation>,
    UsrRollbackCandidatePreserveAuthorityError,
>
where
    Namespace: CompleteArchivedPostMove,
{
    super::require_journal_record_binding(
        &effect.installation,
        journal,
        &effect.journal_record_binding,
        &effect.record,
    )?;
    require_archived_pre_effect_evidence(
        &effect.installation,
        &effect.state_db,
        &effect.record,
        &effect.database,
        &effect.journal_record_binding,
        journal,
    )?;
    let ArchivedEffect {
        installation,
        state_db,
        record,
        database,
        namespace,
        journal_record_binding,
        _active_state_reservation,
    } = effect;
    let namespace_result = namespace.complete(&installation, &record);
    let trailing = super::require_journal_record_binding(&installation, journal, &journal_record_binding, &record)
        .and_then(|()| {
            require_archived_post_effect_evidence(
                &installation,
                &state_db,
                &record,
                &database,
                &journal_record_binding,
                journal,
            )
        });
    let namespace = namespace_result?;
    trailing?;
    Ok(UsrRollbackArchivedCandidatePreserveDurableEffectAuthority {
        effect: ArchivedEffect {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        },
        origin,
    })
}

trait CompleteArchivedPostMove {
    fn complete(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackArchivedCandidatePreserveDurableNamespace, UsrRollbackCandidatePreserveAuthorityError>;
}

impl CompleteArchivedPostMove for UsrRollbackArchivedCandidatePreserveAppliedNamespace {
    fn complete(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackArchivedCandidatePreserveDurableNamespace, UsrRollbackCandidatePreserveAuthorityError> {
        Ok(self.complete_post_move_durability(installation, record)?)
    }
}

impl CompleteArchivedPostMove for UsrRollbackArchivedCandidatePreserveAlreadySatisfiedNamespace {
    fn complete(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<UsrRollbackArchivedCandidatePreserveDurableNamespace, UsrRollbackCandidatePreserveAuthorityError> {
        Ok(self.complete_post_move_durability(installation, record)?)
    }
}

fn require_archived_pre_effect_evidence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    expected_database: &DatabaseEvidence,
    journal_record_binding: &TransitionJournalRecordBinding,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    super::require_journal_record_binding(installation, journal, journal_record_binding, record)?;
    installation.revalidate_mutable_namespace()?;
    require_exact_database(expected_database, inspect_current_database(record, state_db)?)?;
    require_exact_archived_plan(record)?;
    installation.revalidate_mutable_namespace()?;
    super::require_journal_record_binding(installation, journal, journal_record_binding, record)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn require_archived_post_effect_evidence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    expected_database: &DatabaseEvidence,
    journal_record_binding: &TransitionJournalRecordBinding,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    super::require_journal_record_binding(installation, journal, journal_record_binding, record)?;
    installation.revalidate_mutable_namespace()?;
    require_exact_archived_plan(record)?;
    require_exact_database(expected_database, inspect_current_database(record, state_db)?)?;
    installation.revalidate_mutable_namespace()?;
    super::require_journal_record_binding(installation, journal, journal_record_binding, record)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn require_exact_archived_plan(record: &TransitionRecord) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    if record.operation == Operation::ActivateArchived && candidate_preserve_plan_is_exact(record) {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into())
    }
}
