//! Test-sealed authority bridge for the archived child-move foundation.
//!
//! No production dispatcher or persistence path imports this module. It keeps
//! the namespace foundation unreachable until its complete leaf is designed.

use crate::{
    Installation, db,
    transition_journal::{Operation, TransitionJournalBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    DatabaseEvidence, UsrRollbackCandidatePreserveApplyAuthority, UsrRollbackCandidatePreserveAuthority,
    UsrRollbackCandidatePreserveAuthorityError, UsrRollbackCandidatePreserveAuthorityErrorKind,
    UsrRollbackCandidatePreserveFinishAuthority, UsrRollbackCandidatePreserveTopology,
    candidate_preserve_plan_is_exact, inspect_current_database, require_exact_database,
};
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::{
        UsrRollbackArchivedCandidatePreserveAlreadySatisfiedNamespace,
        UsrRollbackArchivedCandidatePreserveAppliedNamespace, UsrRollbackArchivedCandidatePreserveDurableNamespace,
        UsrRollbackArchivedCandidatePreserveNamespaceApplyReconciliation,
        UsrRollbackArchivedCandidatePreserveNamespaceEffectEvidence,
    },
};

pub(in crate::client) struct UsrRollbackArchivedCandidatePreserveEffectSeal {
    _private: (),
}

impl UsrRollbackArchivedCandidatePreserveEffectSeal {
    pub(in crate::client) fn new_for_test() -> Self {
        Self { _private: () }
    }
}

struct ArchivedEffect<'reservation, Namespace> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: Namespace,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[must_use = "test-sealed archived candidate move lease must be reconciled"]
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

#[must_use = "durable archived candidate authority remains test-sealed"]
pub(in crate::client) struct UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'reservation> {
    effect: ArchivedEffect<'reservation, UsrRollbackArchivedCandidatePreserveDurableNamespace>,
}

#[must_use = "test-sealed archived candidate reconciliation must be handled"]
pub(in crate::client) enum UsrRollbackArchivedCandidatePreserveApplyReconciliation<'reservation> {
    Applied(UsrRollbackArchivedCandidatePreserveAppliedEffectAuthority<'reservation>),
    NotApplied,
    Ambiguous,
}

impl<'reservation> UsrRollbackCandidatePreserveApplyAuthority<'reservation> {
    pub(in crate::client) fn into_archived_effect_for_test(
        self,
        _seal: &UsrRollbackArchivedCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackArchivedCandidatePreserveEffectLease<'reservation>, UsrRollbackCandidatePreserveAuthorityError>
    {
        self.evidence.require_journal_binding(journal)?;
        let topology = self.evidence.namespace.topology();
        if topology != UsrRollbackCandidatePreserveTopology::ArchivedStagedWithCanonicalSlot {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into());
        }
        self.evidence.revalidate_after_binding(journal, topology)?;
        let UsrRollbackCandidatePreserveAuthority {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_binding,
            _active_state_reservation,
        } = self.evidence;
        let namespace = namespace.into_archived_apply_effect_evidence(&record)?;
        Ok(UsrRollbackArchivedCandidatePreserveEffectLease {
            effect: ArchivedEffect {
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
    pub(in crate::client) fn into_archived_finish_for_test(
        self,
        _seal: &UsrRollbackArchivedCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        self.evidence.require_journal_binding(journal)?;
        let topology = self.evidence.namespace.topology();
        if topology != UsrRollbackCandidatePreserveTopology::ArchivedPreserved {
            return Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into());
        }
        self.evidence.revalidate_after_binding(journal, topology)?;
        let UsrRollbackCandidatePreserveAuthority {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_binding,
            _active_state_reservation,
        } = self.evidence;
        require_archived_pre_effect_evidence(&installation, &state_db, &record, &database, journal)?;
        let namespace = namespace
            .into_archived_finish_effect_evidence(&record)?
            .reconcile_finish(&installation, &record)?;
        require_archived_post_effect_evidence(&installation, &state_db, &record, &database, journal)?;
        Ok(UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority {
            effect: ArchivedEffect {
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

impl<'reservation> UsrRollbackArchivedCandidatePreserveEffectLease<'reservation> {
    pub(in crate::client) fn reconcile(
        self,
        _seal: &UsrRollbackArchivedCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackArchivedCandidatePreserveApplyReconciliation<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        require_binding(&self.effect.journal_binding, journal)?;
        let ArchivedEffect {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_binding,
            _active_state_reservation,
        } = self.effect;
        require_archived_pre_effect_evidence(&installation, &state_db, &record, &database, journal)?;
        let prepared = namespace.prepare_move(&installation, &record);
        if prepared.is_err() {
            let _ = require_archived_post_effect_evidence(&installation, &state_db, &record, &database, journal);
        }
        let prepared = prepared?;
        require_binding(&journal_binding, journal)?;
        require_archived_pre_effect_evidence(&installation, &state_db, &record, &database, journal)?;
        let namespace_result = prepared.reconcile_move(&installation, &record);
        let trailing = require_archived_post_effect_evidence(&installation, &state_db, &record, &database, journal);
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
                            journal_binding,
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
        _seal: &UsrRollbackArchivedCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        complete_post_move(self.effect, journal)
    }
}

impl<'reservation> UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation> {
    pub(in crate::client) fn complete_post_move_durability(
        self,
        _seal: &UsrRollbackArchivedCandidatePreserveEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        complete_post_move(self.effect, journal)
    }
}

fn complete_post_move<'reservation, Namespace>(
    effect: ArchivedEffect<'reservation, Namespace>,
    journal: &TransitionJournalStore,
) -> Result<
    UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'reservation>,
    UsrRollbackCandidatePreserveAuthorityError,
>
where
    Namespace: CompleteArchivedPostMove,
{
    require_binding(&effect.journal_binding, journal)?;
    require_archived_pre_effect_evidence(
        &effect.installation,
        &effect.state_db,
        &effect.record,
        &effect.database,
        journal,
    )?;
    let ArchivedEffect {
        installation,
        state_db,
        record,
        database,
        namespace,
        journal_binding,
        _active_state_reservation,
    } = effect;
    let namespace_result = namespace.complete(&installation, &record);
    let trailing = require_binding(&journal_binding, journal)
        .and_then(|()| require_archived_post_effect_evidence(&installation, &state_db, &record, &database, journal));
    let namespace = namespace_result?;
    trailing?;
    Ok(UsrRollbackArchivedCandidatePreserveDurableEffectAuthority {
        effect: ArchivedEffect {
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

impl UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'_> {
    pub(in crate::client) fn revalidate_for_test(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
        require_binding(&self.effect.journal_binding, journal)?;
        require_archived_post_effect_evidence(
            &self.effect.installation,
            &self.effect.state_db,
            &self.effect.record,
            &self.effect.database,
            journal,
        )?;
        self.effect
            .namespace
            .revalidate(&self.effect.installation, &self.effect.record)?;
        require_archived_post_effect_evidence(
            &self.effect.installation,
            &self.effect.state_db,
            &self.effect.record,
            &self.effect.database,
            journal,
        )
    }
}

fn require_binding(
    expected: &TransitionJournalBinding,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    if journal.has_binding(expected) {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveAuthorityErrorKind::JournalBindingMismatch.into())
    }
}

fn require_archived_pre_effect_evidence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    expected_database: &DatabaseEvidence,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    installation.revalidate_mutable_namespace()?;
    require_exact_database(expected_database, inspect_current_database(record, state_db)?)?;
    require_exact_archived_journal(journal, record)?;
    require_exact_archived_plan(record)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn require_archived_post_effect_evidence(
    installation: &Installation,
    state_db: &db::state::Database,
    record: &TransitionRecord,
    expected_database: &DatabaseEvidence,
    journal: &TransitionJournalStore,
) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    installation.revalidate_mutable_namespace()?;
    require_exact_archived_journal(journal, record)?;
    require_exact_archived_plan(record)?;
    require_exact_database(expected_database, inspect_current_database(record, state_db)?)?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn require_exact_archived_journal(
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

fn require_exact_archived_plan(record: &TransitionRecord) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
    if record.operation == Operation::ActivateArchived && candidate_preserve_plan_is_exact(record) {
        Ok(())
    } else {
        Err(UsrRollbackCandidatePreserveAuthorityErrorKind::EvidenceMismatch.into())
    }
}
