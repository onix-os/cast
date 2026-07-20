//! Authority convergence for ActiveReblit POST durability.

mod persistence;

#[cfg(test)]
pub(in crate::client) use persistence::arm_before_active_reblit_candidate_preserve_persistence_durable_trailing_evidence;
pub(in crate::client) use persistence::UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError;

#[cfg(test)]
use crate::transition_journal::RollbackActionOutcome;
use crate::{
    Installation, db,
    transition_journal::{TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord},
};

use super::{
    ReconciledActiveReblitCandidatePreserveEffect,
    UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority,
    UsrRollbackActiveReblitCandidatePreserveAppliedEffectAuthority, require_active_reblit_post_effect_evidence,
    require_active_reblit_pre_effect_evidence,
};
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::{
        DatabaseEvidence, UsrRollbackCandidatePreserveAuthorityError, UsrRollbackCandidatePreserveNamespaceError,
        activation_namespace::UsrRollbackActiveReblitCandidatePreserveDurableNamespace,
        usr_rollback_candidate_preserve_authority::effect_evidence::require_effect_binding,
    },
    startup_recovery::UsrRollbackActiveReblitCandidatePreserveDurabilitySeal,
};

/// Opaque authority retained only after every namespace and non-namespace
/// POST check has completed.
#[must_use = "durable ActiveReblit candidate-preservation authority must remain sealed"]
pub(in crate::client) struct UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority<'reservation> {
    _effect: DurableActiveReblitCandidatePreserveEffect<'reservation>,
    origin: ActiveReblitDurabilityOrigin,
}

struct DurableActiveReblitCandidatePreserveEffect<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackActiveReblitCandidatePreserveDurableNamespace,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveReblitDurabilityOrigin {
    Applied,
    AlreadySatisfied,
}

impl<'reservation> UsrRollbackActiveReblitCandidatePreserveAppliedEffectAuthority<'reservation> {
    /// Consume freshly applied evidence through the shared suffix. The origin
    /// is fixed here and cannot be supplied by the caller.
    pub(in crate::client) fn complete_post_exchange_durability(
        self,
        _seal: &UsrRollbackActiveReblitCandidatePreserveDurabilitySeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        require_effect_binding(
            &self._effect._installation,
            &self._effect._journal_record_binding,
            &self._effect._record,
            journal,
        )?;
        let effect = complete_after_binding(self._effect, journal, |namespace, installation, record| {
            namespace.complete_post_exchange_durability(installation, record)
        })?;
        Ok(UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority {
            _effect: effect,
            origin: ActiveReblitDurabilityOrigin::Applied,
        })
    }
}

impl<'reservation> UsrRollbackActiveReblitCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation> {
    /// Consume independently admitted Finish evidence through the identical
    /// suffix, with a separately fixed private origin.
    pub(in crate::client) fn complete_post_exchange_durability(
        self,
        _seal: &UsrRollbackActiveReblitCandidatePreserveDurabilitySeal,
        journal: &TransitionJournalStore,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority<'reservation>,
        UsrRollbackCandidatePreserveAuthorityError,
    > {
        require_effect_binding(
            &self._effect._installation,
            &self._effect._journal_record_binding,
            &self._effect._record,
            journal,
        )?;
        let effect = complete_after_binding(self._effect, journal, |namespace, installation, record| {
            namespace.complete_post_exchange_durability(installation, record)
        })?;
        Ok(UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority {
            _effect: effect,
            origin: ActiveReblitDurabilityOrigin::AlreadySatisfied,
        })
    }
}

fn complete_after_binding<'reservation, Namespace>(
    effect: ReconciledActiveReblitCandidatePreserveEffect<'reservation, Namespace>,
    journal: &TransitionJournalStore,
    complete_namespace: impl FnOnce(
        Namespace,
        &Installation,
        &TransitionRecord,
    ) -> Result<
        UsrRollbackActiveReblitCandidatePreserveDurableNamespace,
        UsrRollbackCandidatePreserveNamespaceError,
    >,
) -> Result<DurableActiveReblitCandidatePreserveEffect<'reservation>, UsrRollbackCandidatePreserveAuthorityError> {
    let ReconciledActiveReblitCandidatePreserveEffect {
        _installation: installation,
        _state_db: state_db,
        _record: record,
        _database: database,
        _namespace: namespace,
        _journal_record_binding: journal_record_binding,
        _active_state_reservation,
    } = effect;

    require_active_reblit_pre_effect_evidence(
        &installation,
        &state_db,
        &record,
        &database,
        &journal_record_binding,
        journal,
    )?;
    let namespace_result = complete_namespace(namespace, &installation, &record);
    run_before_durable_trailing_evidence();
    let trailing_evidence = require_effect_binding(&installation, &journal_record_binding, &record, journal)
        .and_then(|()| {
            require_active_reblit_post_effect_evidence(
                &installation,
                &state_db,
                &record,
                &database,
                &journal_record_binding,
                journal,
            )
        });
    let namespace = namespace_result?;
    trailing_evidence?;

    Ok(DurableActiveReblitCandidatePreserveEffect {
        installation,
        state_db,
        record,
        database,
        namespace,
        journal_record_binding,
        _active_state_reservation,
    })
}

#[cfg(test)]
impl UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority<'_> {
    pub(in crate::client) fn origin_for_test(&self) -> RollbackActionOutcome {
        match self.origin {
            ActiveReblitDurabilityOrigin::Applied => RollbackActionOutcome::Applied,
            ActiveReblitDurabilityOrigin::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        }
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_DURABLE_TRAILING_EVIDENCE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_active_reblit_candidate_preserve_durable_trailing_evidence(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_DURABLE_TRAILING_EVIDENCE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_before_durable_trailing_evidence() {
    BEFORE_DURABLE_TRAILING_EVIDENCE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_durable_trailing_evidence() {}
