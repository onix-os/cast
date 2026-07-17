//! Test-only authority convergence for ActiveReblit POST durability.
//!
//! This child is reachable only through the already test-gated ActiveReblit
//! effect module. It neither selects production work nor persists a journal.

use crate::{
    Installation, db,
    transition_journal::{RollbackActionOutcome, TransitionJournalBinding, TransitionJournalStore, TransitionRecord},
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
};

/// Seal dedicated to the test-only ActiveReblit durability checkpoint.
/// Production startup has no constructor, dispatch, or call site for it.
pub(in crate::client) struct UsrRollbackActiveReblitCandidatePreserveDurabilitySeal {
    _private: (),
}

impl UsrRollbackActiveReblitCandidatePreserveDurabilitySeal {
    pub(in crate::client) fn new_for_test() -> Self {
        Self { _private: () }
    }
}

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
    journal_binding: TransitionJournalBinding,
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
        require_effect_binding(&self._effect._journal_binding, journal)?;
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
        require_effect_binding(&self._effect._journal_binding, journal)?;
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
        _journal_binding: journal_binding,
        _active_state_reservation,
    } = effect;

    require_active_reblit_pre_effect_evidence(&installation, &state_db, &record, &database, journal)?;
    let namespace_result = complete_namespace(namespace, &installation, &record);
    run_before_durable_trailing_evidence();
    let trailing_evidence = require_effect_binding(&journal_binding, journal).and_then(|()| {
        require_active_reblit_post_effect_evidence(&installation, &state_db, &record, &database, journal)
    });
    let namespace = namespace_result?;
    trailing_evidence?;

    Ok(DurableActiveReblitCandidatePreserveEffect {
        installation,
        state_db,
        record,
        database,
        namespace,
        journal_binding,
        _active_state_reservation,
    })
}

impl UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority<'_> {
    pub(in crate::client) fn origin_for_test(&self) -> RollbackActionOutcome {
        match self.origin {
            ActiveReblitDurabilityOrigin::Applied => RollbackActionOutcome::Applied,
            ActiveReblitDurabilityOrigin::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        }
    }

    pub(in crate::client) fn revalidate_for_test(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackCandidatePreserveAuthorityError> {
        require_effect_binding(&self._effect.journal_binding, journal)?;
        require_active_reblit_post_effect_evidence(
            &self._effect.installation,
            &self._effect.state_db,
            &self._effect.record,
            &self._effect.database,
            journal,
        )?;
        self._effect
            .namespace
            .revalidate(&self._effect.installation, &self._effect.record)?;
        Ok(())
    }
}

std::thread_local! {
    static BEFORE_DURABLE_TRAILING_EVIDENCE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

pub(in crate::client) fn arm_before_active_reblit_candidate_preserve_durable_trailing_evidence(
    hook: impl FnOnce() + 'static,
) {
    BEFORE_DURABLE_TRAILING_EVIDENCE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

fn run_before_durable_trailing_evidence() {
    BEFORE_DURABLE_TRAILING_EVIDENCE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}
