//! Consuming promoted terminal evidence into durable `BootSyncComplete`.
//!
//! This is the only bridge which may mint the completion seal, and its entry
//! point exists only on the cleaned-promoted typestate. It repeats the promoted
//! cross-store and terminal-output evidence before handing the owned staging
//! authority to the journal state owner, then repeats completed cross-store and
//! terminal evidence before returning a non-cloneable result. Every error
//! consumes all authority. This slice performs no later commit,
//! installed-receipt mutation, boot-file replacement, or transition cleanup.

use thiserror::Error;

use crate::{
    boot_publication::BootPublicationReceiptFingerprint,
    client::{
        Client,
        active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_boot_sync_staging::{
            ActiveReblitBootSyncCompletePersistenceError,
            ActiveReblitBootSyncCompleteValidationError,
            ActiveReblitBootSyncCompletionReconciliationError,
            CompletedStagedActiveReblitBootSync,
            DurableActiveReblitBootSyncCompletionRecord,
        },
    },
    db::state::BootPublicationReceiptPromotionOutcome,
    transition_journal::TransitionRecord,
};

use super::{
    ActiveReblitBootPostPromotionValidationError,
    ActiveReblitBootTerminalEvidenceValidationError,
    CleanedPromotedExactActiveReblitBootPublication,
    PromotedExactActiveReblitBootPublication,
    require_deadline,
    validate_exact_terminal_evidence_snapshot,
};
use super::super::{
    ActiveReblitBootSyncCompletionSeal,
    StagedExactActiveReblitBootPublication,
    ValidatedActiveReblitBootPublicationEffect,
};

/// Exact terminal publication whose promoted receipt and journal are durably
/// correlated at `BootSyncComplete`.
///
/// This value deliberately exposes observations only. Its owned staging
/// completion remains private for the later commit-coordination slice.
#[must_use = "completed boot-publication authority must enter commit coordination or be deliberately discarded"]
pub(in crate::client) struct CompletedExactActiveReblitBootPublication<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
> {
    completed: CompletedStagedActiveReblitBootSync<
        'plan,
        'inventory,
        BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
    >,
    database_outcome: BootPublicationReceiptPromotionOutcome,
    publication_count: usize,
    published_count: usize,
    already_exact_count: usize,
    replaced_count: usize,
    evidence: Vec<ValidatedActiveReblitBootPublicationEffect>,
}

impl std::fmt::Debug
    for CompletedExactActiveReblitBootPublication<'_, '_, '_, '_, '_, '_, '_, '_>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompletedExactActiveReblitBootPublication")
            .field("receipt_fingerprint", &self.receipt_fingerprint())
            .field("database_outcome", &self.database_outcome)
            .field("publication_count", &self.publication_count)
            .field("published_count", &self.published_count)
            .field("already_exact_count", &self.already_exact_count)
            .field("replaced_count", &self.replaced_count)
            .field("durable_phase", &"BootSyncComplete")
            .finish_non_exhaustive()
    }
}

impl CompletedExactActiveReblitBootPublication<'_, '_, '_, '_, '_, '_, '_, '_> {
    pub(in crate::client) const fn record(&self) -> &TransitionRecord {
        self.completed.record()
    }

    pub(in crate::client) const fn receipt_fingerprint(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.completed.receipt_fingerprint()
    }

    pub(in crate::client) const fn database_outcome(
        &self,
    ) -> BootPublicationReceiptPromotionOutcome {
        self.database_outcome
    }

    pub(in crate::client) const fn publication_count(&self) -> usize {
        self.publication_count
    }

    pub(in crate::client) const fn published_count(&self) -> usize {
        self.published_count
    }

    pub(in crate::client) const fn already_exact_count(&self) -> usize {
        self.already_exact_count
    }

    pub(in crate::client) const fn replaced_count(&self) -> usize {
        self.replaced_count
    }

    pub(in crate::client) fn evidence(&self) -> &[ValidatedActiveReblitBootPublicationEffect] {
        &self.evidence
    }
}

/// Validation failure after the completion state owner returned an exact
/// `BootSyncComplete` authority.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootPostCompletionValidationError {
    #[error("revalidate completed journal, receipt, database, and installation at {checkpoint}")]
    CompletedStagedEvidence {
        checkpoint: &'static str,
        #[source]
        source: ActiveReblitBootSyncCompleteValidationError,
    },
    #[error("the completed staging authority returned a different retained plan at {checkpoint}")]
    PlanMismatch { checkpoint: &'static str },
    #[error("repeat exact terminal output and topology validation at {checkpoint}")]
    TerminalEvidence {
        checkpoint: &'static str,
        #[source]
        source: ActiveReblitBootTerminalEvidenceValidationError,
    },
}

/// Failure while consuming promoted terminal publication into durable journal
/// completion. No variant contains a seal, store, record binding, staging
/// token, promoted token, or completed authority.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSyncCompletionError {
    #[error("validate exact promoted terminal evidence at initial completion admission")]
    InitialHandoff(#[source] ActiveReblitBootPostPromotionValidationError),
    #[error("repeat exact promoted terminal evidence immediately before completion")]
    ImmediateHandoff(#[source] ActiveReblitBootPostPromotionValidationError),
    #[error("the inherited completion deadline expired immediately before persistence")]
    Deadline(#[source] ActiveReblitBootTerminalEvidenceValidationError),
    #[error("persist exact promoted BootSyncStarted evidence as BootSyncComplete")]
    Persistence(#[source] ActiveReblitBootSyncCompletePersistenceError),
    #[error("post-completion validation failed; durable journal is {durable:?}")]
    PostCompletion {
        durable: DurableActiveReblitBootSyncCompletionRecord,
        #[source]
        source: ActiveReblitBootPostCompletionValidationError,
    },
    #[error("post-completion validation and exact Started-or-Complete reconciliation both failed")]
    PostCompletionAndReconciliation {
        validation: ActiveReblitBootPostCompletionValidationError,
        #[source]
        reconciliation: ActiveReblitBootSyncCompletionReconciliationError,
    },
}

impl<
        'plan,
        'inventory,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
    CleanedPromotedExactActiveReblitBootPublication<
        'plan,
        'inventory,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
where
    'input: 'plan,
{
    /// Consume exact promoted terminal evidence into one deadline-bound
    /// `BootSyncComplete` persistence attempt.
    pub(in crate::client) fn persist_boot_sync_complete(
        self,
        client: &Client,
    ) -> Result<
        CompletedExactActiveReblitBootPublication<
            'plan,
            'inventory,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootSyncCompletionError,
    > {
        let retained_plan = self
            .validate_completion_handoff(client, None, "initial completion admission")
            .map_err(ActiveReblitBootSyncCompletionError::InitialHandoff)?;

        after_initial_completion_handoff();
        self.validate_completion_handoff(
            client,
            Some(retained_plan),
            "immediate pre-persistence",
        )
            .map_err(ActiveReblitBootSyncCompletionError::ImmediateHandoff)?;

        before_completion_deadline();
        require_deadline("immediate completion persistence", retained_plan.input_deadline())
            .map_err(ActiveReblitBootSyncCompletionError::Deadline)?;

        let CleanedPromotedExactActiveReblitBootPublication { promoted } = self;
        let PromotedExactActiveReblitBootPublication {
            terminal,
            database_outcome,
        } = promoted;
        let StagedExactActiveReblitBootPublication {
            staged,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            promoted_cleanup_required,
            evidence,
        } = terminal;
        debug_assert!(!promoted_cleanup_required);
        let seal = ActiveReblitBootSyncCompletionSeal { _private: () };
        let completed = staged
            .persist_boot_sync_complete(client, seal)
            .map_err(ActiveReblitBootSyncCompletionError::Persistence)?;

        after_boot_sync_complete_persistence();
        if let Err(validation) = validate_completed_terminal_sandwich(
            &completed,
            client,
            retained_plan,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            &evidence,
            "post-persistence",
        ) {
            return Err(reconcile_post_completion_failure(completed, validation));
        }

        before_final_completion_validation();
        if let Err(validation) = validate_completed_terminal_sandwich(
            &completed,
            client,
            retained_plan,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            &evidence,
            "final return",
        ) {
            return Err(reconcile_post_completion_failure(completed, validation));
        }

        Ok(CompletedExactActiveReblitBootPublication {
            completed,
            database_outcome,
            publication_count,
            published_count,
            already_exact_count,
            replaced_count,
            evidence,
        })
    }

    fn validate_completion_handoff(
        &self,
        client: &Client,
        expected_plan: Option<&'plan BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >>,
        checkpoint: &'static str,
    ) -> Result<
        &'plan BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootPostPromotionValidationError,
    > {
        let fresh = self
            .promoted
            .terminal
            .staged
            .revalidate_promoted_against(client)
            .map_err(|source| {
                ActiveReblitBootPostPromotionValidationError::PromotedStagedEvidence {
                    checkpoint,
                    source,
                }
            })?;
        let retained_plan = fresh.plan();
        if let Some(expected_plan) = expected_plan {
            if !std::ptr::eq(retained_plan, expected_plan) {
                return Err(ActiveReblitBootPostPromotionValidationError::PlanMismatch {
                    checkpoint,
                });
            }
        }
        validate_exact_terminal_evidence_snapshot(
            retained_plan,
            self.receipt_fingerprint(),
            self.promoted.terminal.publication_count,
            self.promoted.terminal.published_count,
            self.promoted.terminal.already_exact_count,
            self.promoted.terminal.replaced_count,
            &self.promoted.terminal.evidence,
            checkpoint,
        )
        .map_err(|source| {
            ActiveReblitBootPostPromotionValidationError::TerminalEvidence {
                checkpoint,
                source,
            }
        })?;
        let final_fresh = self
            .promoted
            .terminal
            .staged
            .revalidate_promoted_against(client)
            .map_err(|source| {
                ActiveReblitBootPostPromotionValidationError::PromotedStagedEvidence {
                    checkpoint,
                    source,
                }
            })?;
        if !std::ptr::eq(final_fresh.plan(), retained_plan) {
            return Err(ActiveReblitBootPostPromotionValidationError::PlanMismatch {
                checkpoint,
            });
        }
        Ok(retained_plan)
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_completed_terminal_sandwich<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    completed: &CompletedStagedActiveReblitBootSync<
        'plan,
        'inventory,
        BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
    >,
    client: &Client,
    retained_plan: &'plan BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    publication_count: usize,
    published_count: usize,
    already_exact_count: usize,
    replaced_count: usize,
    evidence: &[ValidatedActiveReblitBootPublicationEffect],
    checkpoint: &'static str,
) -> Result<(), ActiveReblitBootPostCompletionValidationError>
where
    'input: 'plan,
{
    let fresh = completed.revalidate_against(client).map_err(|source| {
        ActiveReblitBootPostCompletionValidationError::CompletedStagedEvidence {
            checkpoint,
            source,
        }
    })?;
    if !std::ptr::eq(fresh.plan(), retained_plan) {
        return Err(ActiveReblitBootPostCompletionValidationError::PlanMismatch {
            checkpoint,
        });
    }
    validate_exact_terminal_evidence_snapshot(
        retained_plan,
        completed.receipt_fingerprint(),
        publication_count,
        published_count,
        already_exact_count,
        replaced_count,
        evidence,
        checkpoint,
    )
    .map_err(|source| {
        ActiveReblitBootPostCompletionValidationError::TerminalEvidence {
            checkpoint,
            source,
        }
    })?;
    let final_fresh = completed.revalidate_against(client).map_err(|source| {
        ActiveReblitBootPostCompletionValidationError::CompletedStagedEvidence {
            checkpoint,
            source,
        }
    })?;
    if !std::ptr::eq(final_fresh.plan(), retained_plan) {
        return Err(ActiveReblitBootPostCompletionValidationError::PlanMismatch {
            checkpoint,
        });
    }
    require_deadline(checkpoint, retained_plan.input_deadline()).map_err(|source| {
        ActiveReblitBootPostCompletionValidationError::TerminalEvidence {
            checkpoint,
            source,
        }
    })
}

fn reconcile_post_completion_failure<
    'plan,
    'inventory,
    Plan,
>(
    completed: CompletedStagedActiveReblitBootSync<'plan, 'inventory, Plan>,
    validation: ActiveReblitBootPostCompletionValidationError,
) -> ActiveReblitBootSyncCompletionError {
    match completed.reconcile_after_completed_validation_failure() {
        Ok(durable) => ActiveReblitBootSyncCompletionError::PostCompletion {
            durable,
            source: validation,
        },
        Err(reconciliation) => {
            ActiveReblitBootSyncCompletionError::PostCompletionAndReconciliation {
                validation,
                reconciliation,
            }
        }
    }
}

#[cfg(test)]
std::thread_local! {
    static AFTER_INITIAL_HANDOFF: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_COMPLETION_DEADLINE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_COMPLETION_PERSISTENCE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_FINAL_COMPLETION_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_callback(
    slot: &'static std::thread::LocalKey<std::cell::RefCell<Option<Box<dyn FnOnce()>>>>,
    callback: impl FnOnce() + 'static,
) {
    slot.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(callback)).is_none());
    });
}

#[cfg(test)]
fn assert_callback_consumed(
    slot: &'static std::thread::LocalKey<std::cell::RefCell<Option<Box<dyn FnOnce()>>>>,
    name: &'static str,
) {
    slot.with(|slot| {
        assert!(slot.borrow().is_none(), "{name} completion hook was not consumed");
    });
}

#[cfg(test)]
pub(in crate::client) fn arm_after_initial_completion_handoff(
    callback: impl FnOnce() + 'static,
) {
    arm_callback(&AFTER_INITIAL_HANDOFF, callback);
}

#[cfg(test)]
pub(in crate::client) fn arm_before_completion_deadline(callback: impl FnOnce() + 'static) {
    arm_callback(&BEFORE_COMPLETION_DEADLINE, callback);
}

#[cfg(test)]
pub(in crate::client) fn arm_after_boot_sync_complete_persistence(
    callback: impl FnOnce() + 'static,
) {
    arm_callback(&AFTER_COMPLETION_PERSISTENCE, callback);
}

#[cfg(test)]
pub(in crate::client) fn arm_before_final_completion_validation(
    callback: impl FnOnce() + 'static,
) {
    arm_callback(&BEFORE_FINAL_COMPLETION_VALIDATION, callback);
}

#[cfg(test)]
pub(in crate::client) fn assert_after_initial_completion_handoff_hook_consumed() {
    assert_callback_consumed(
        &AFTER_INITIAL_HANDOFF,
        "after-initial-handoff",
    );
}

#[cfg(test)]
pub(in crate::client) fn assert_before_completion_deadline_hook_consumed() {
    assert_callback_consumed(
        &BEFORE_COMPLETION_DEADLINE,
        "before-deadline",
    );
}

#[cfg(test)]
pub(in crate::client) fn assert_after_boot_sync_complete_persistence_hook_consumed() {
    assert_callback_consumed(
        &AFTER_COMPLETION_PERSISTENCE,
        "after-persistence",
    );
}

#[cfg(test)]
pub(in crate::client) fn assert_before_final_completion_validation_hook_consumed() {
    assert_callback_consumed(
        &BEFORE_FINAL_COMPLETION_VALIDATION,
        "before-final-validation",
    );
}

#[cfg(test)]
fn run_callback(
    slot: &'static std::thread::LocalKey<std::cell::RefCell<Option<Box<dyn FnOnce()>>>>,
) {
    slot.with(|slot| {
        if let Some(callback) = slot.borrow_mut().take() {
            callback();
        }
    });
}

#[cfg(test)]
fn after_initial_completion_handoff() {
    run_callback(&AFTER_INITIAL_HANDOFF);
}

#[cfg(not(test))]
fn after_initial_completion_handoff() {}

#[cfg(test)]
fn before_completion_deadline() {
    run_callback(&BEFORE_COMPLETION_DEADLINE);
}

#[cfg(not(test))]
fn before_completion_deadline() {}

#[cfg(test)]
fn after_boot_sync_complete_persistence() {
    run_callback(&AFTER_COMPLETION_PERSISTENCE);
}

#[cfg(not(test))]
fn after_boot_sync_complete_persistence() {}

#[cfg(test)]
fn before_final_completion_validation() {
    run_callback(&BEFORE_FINAL_COMPLETION_VALIDATION);
}

#[cfg(not(test))]
fn before_final_completion_validation() {}

#[path = "boot_sync_completion/commit_decision.rs"]
mod commit_decision;
pub(in crate::client) use commit_decision::{
    ActiveReblitBootCompleteError,
    ActiveReblitBootCompleteHandoff,
    ActiveReblitBootCompletePostAdvanceError,
    ActiveReblitBootCommitCleanupCompleteHandoff,
    ActiveReblitBootFinalizationError,
    ActiveReblitBootFinalizedHandoff,
    ActiveReblitBootCommitCleanupError,
    ActiveReblitBootCommitCleanupPostAdvanceError,
    ActiveReblitBootCommitDecisionError,
    ActiveReblitBootCommitDecisionFinalValidation,
    ActiveReblitBootCommitDecisionHandoff,
};
#[cfg(test)]
pub(in crate::client) use commit_decision::{
    arm_after_active_reblit_commit_decision_terminal_validation,
    assert_after_active_reblit_commit_decision_terminal_validation_hook_consumed,
};
