//! Consuming authority from terminal immutable publication into receipt promotion.
//!
//! The scalar leaf evidence retained by the publication attempt is historical,
//! not ongoing namespace authority. Promotion therefore repeats complete
//! read-only publication preflight under the original deadline, authenticates
//! the exact staged journal and database state before requesting the sole
//! database mutation, and repeats both sides after a successful database
//! return. A promoted terminal value can enter the completion child only after
//! cleanup authority has been discharged into the distinct cleaned-promoted
//! typestate. Neither boundary can replace or delete a boot file, decide the
//! commit, or clean old receipts.

use std::time::Instant;

use thiserror::Error;

use crate::{
    boot_publication::BootPublicationReceiptFingerprint,
    client::{
        Client,
        active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_boot_sync_staging::{
            ActiveReblitBootSyncFreshValidationError,
            ActiveReblitBootSyncPromotedValidationError,
        },
    },
    db::state::{
        BootPublicationReceiptPromotionDurableState,
        BootPublicationReceiptPromotionError,
        BootPublicationReceiptPromotionOutcome,
    },
};

use super::{
    StagedExactActiveReblitBootPublication,
    ValidatedActiveReblitBootPublicationEffect,
};
use replacement_pair_validation::validate_applied_replacement_pairs;
use terminal_evidence::validate_exact_terminal_evidence_snapshot;

/// Terminal exact publication whose canonical receipt is now committed.
///
/// The original terminal token remains owned rather than being projected into
/// detached receipt data. This value is non-`Clone`; it must first be cleaned
/// (or prove that no cleanup is required) before completion is available.
#[must_use = "promoted boot-publication authority must be cleaned or deliberately discarded"]
pub(in crate::client) struct PromotedExactActiveReblitBootPublication<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
> {
    terminal: StagedExactActiveReblitBootPublication<
        'plan,
        'inventory,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    database_outcome: BootPublicationReceiptPromotionOutcome,
}

/// Promoted exact publication whose replacement and stale-output cleanup
/// authority has been fully discharged.
///
/// This non-`Clone` wrapper is the only production typestate from which
/// `BootSyncComplete` can be persisted. The private promoted token remains
/// intact inside it so no counters, evidence, receipt binding, or database
/// outcome are reduced to forgeable scalar claims at the cleanup boundary.
#[must_use = "cleaned promoted boot-publication authority must be durably completed or deliberately discarded"]
pub(in crate::client) struct CleanedPromotedExactActiveReblitBootPublication<
    'plan,
    'inventory,
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
> {
    promoted: PromotedExactActiveReblitBootPublication<
        'plan,
        'inventory,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
}

impl std::fmt::Debug
    for PromotedExactActiveReblitBootPublication<'_, '_, '_, '_, '_, '_, '_, '_>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PromotedExactActiveReblitBootPublication")
            .field("receipt_fingerprint", &self.receipt_fingerprint())
            .field("database_outcome", &self.database_outcome)
            .field("publication_count", &self.publication_count())
            .field("published_count", &self.published_count())
            .field("already_exact_count", &self.already_exact_count())
            .field("replaced_count", &self.replaced_count())
            .field("durable_phase", &"BootSyncStarted")
            .finish_non_exhaustive()
    }
}

impl PromotedExactActiveReblitBootPublication<'_, '_, '_, '_, '_, '_, '_, '_> {
    pub(in crate::client) const fn receipt_fingerprint(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.terminal.receipt_fingerprint()
    }

    pub(in crate::client) const fn database_outcome(
        &self,
    ) -> BootPublicationReceiptPromotionOutcome {
        self.database_outcome
    }

    pub(in crate::client) const fn publication_count(&self) -> usize {
        self.terminal.publication_count()
    }

    pub(in crate::client) const fn published_count(&self) -> usize {
        self.terminal.published_count()
    }

    pub(in crate::client) const fn already_exact_count(&self) -> usize {
        self.terminal.already_exact_count()
    }

    pub(in crate::client) const fn replaced_count(&self) -> usize {
        self.terminal.replaced_count()
    }

    pub(in crate::client) const fn promoted_cleanup_required(&self) -> bool {
        self.terminal.promoted_cleanup_required()
    }

    pub(in crate::client) fn evidence(&self) -> &[ValidatedActiveReblitBootPublicationEffect] {
        self.terminal.evidence()
    }
}

impl std::fmt::Debug
    for CleanedPromotedExactActiveReblitBootPublication<'_, '_, '_, '_, '_, '_, '_, '_>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CleanedPromotedExactActiveReblitBootPublication")
            .field("receipt_fingerprint", &self.receipt_fingerprint())
            .field("database_outcome", &self.database_outcome())
            .field("publication_count", &self.publication_count())
            .field("published_count", &self.published_count())
            .field("already_exact_count", &self.already_exact_count())
            .field("replaced_count", &self.replaced_count())
            .field("durable_phase", &"BootSyncStarted")
            .field("cleanup", &"discharged")
            .finish_non_exhaustive()
    }
}

impl CleanedPromotedExactActiveReblitBootPublication<'_, '_, '_, '_, '_, '_, '_, '_> {
    pub(in crate::client) const fn receipt_fingerprint(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.promoted.receipt_fingerprint()
    }

    pub(in crate::client) const fn database_outcome(
        &self,
    ) -> BootPublicationReceiptPromotionOutcome {
        self.promoted.database_outcome()
    }

    pub(in crate::client) const fn publication_count(&self) -> usize {
        self.promoted.publication_count()
    }

    pub(in crate::client) const fn published_count(&self) -> usize {
        self.promoted.published_count()
    }

    pub(in crate::client) const fn already_exact_count(&self) -> usize {
        self.promoted.already_exact_count()
    }

    pub(in crate::client) const fn replaced_count(&self) -> usize {
        self.promoted.replaced_count()
    }

    pub(in crate::client) fn evidence(&self) -> &[ValidatedActiveReblitBootPublicationEffect] {
        self.promoted.evidence()
    }
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
    PromotedExactActiveReblitBootPublication<
        'plan,
        'inventory,
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
{
    /// Preserve the complete promoted authority while proving that no
    /// replacement or owned-stale cleanup remains.
    ///
    /// A publication which still owns cleanup authority is returned intact in
    /// `Err`; callers cannot lose that authority merely by probing readiness.
    pub(in crate::client) fn try_into_cleaned(
        self,
    ) -> Result<
        CleanedPromotedExactActiveReblitBootPublication<
            'plan,
            'inventory,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        Self,
    > {
        if self.promoted_cleanup_required() {
            Err(self)
        } else {
            Ok(CleanedPromotedExactActiveReblitBootPublication { promoted: self })
        }
    }
}

/// Failure after the database reported a definite promotion outcome.
///
/// The outcome is retained in the enclosing error while neither authority
/// token is returned. Recovery must continue from durable evidence instead of
/// retrying the consumed terminal token.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootPostPromotionValidationError {
    #[error("repeat exact terminal output and topology validation at {checkpoint}")]
    TerminalEvidence {
        checkpoint: &'static str,
        #[source]
        source: ActiveReblitBootTerminalEvidenceValidationError,
    },
    #[error("revalidate the exact promoted journal, database, and installation at {checkpoint}")]
    PromotedStagedEvidence {
        checkpoint: &'static str,
        #[source]
        source: ActiveReblitBootSyncPromotedValidationError,
    },
    #[error("promoted validation returned a different retained plan at {checkpoint}")]
    PlanMismatch { checkpoint: &'static str },
}

/// Failure while consuming terminal publication evidence into DB promotion.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootReceiptPromotionError {
    #[error("admit the retained BootSyncStarted evidence as neither exact pending nor exact promoted state")]
    InitialAdmission {
        pending: ActiveReblitBootSyncFreshValidationError,
        promoted: ActiveReblitBootSyncPromotedValidationError,
    },
    #[error("freshly validate terminal output and topology evidence before receipt promotion")]
    TerminalEvidence(#[source] ActiveReblitBootTerminalEvidenceValidationError),
    #[error("revalidate exact pending BootSyncStarted evidence immediately before promotion")]
    PrePromotionPending(#[source] ActiveReblitBootSyncFreshValidationError),
    #[error("revalidate exact already-promoted BootSyncStarted evidence immediately before retry")]
    PrePromotionAlreadyPromoted(#[source] ActiveReblitBootSyncPromotedValidationError),
    #[error("the retained publication plan changed at {checkpoint}")]
    PlanMismatch { checkpoint: &'static str },
    #[error("atomically promote the exact terminal boot-publication receipt")]
    DatabasePromotion(#[source] BootPublicationReceiptPromotionError),
    #[error("receipt promotion returned {outcome:?}, but post-promotion validation failed")]
    PostPromotion {
        outcome: BootPublicationReceiptPromotionOutcome,
        #[source]
        source: ActiveReblitBootPostPromotionValidationError,
    },
}

impl ActiveReblitBootReceiptPromotionError {
    /// Return the exact successful database-call outcome retained by a later
    /// post-promotion validation failure.
    ///
    /// A database-call error can instead report only the reconciled durable
    /// receipt state; use [`Self::durable_receipt_state`] for that distinction.
    pub(in crate::client) const fn durable_promotion_outcome(
        &self,
    ) -> Option<BootPublicationReceiptPromotionOutcome> {
        match self {
            Self::PostPromotion { outcome, .. } => Some(*outcome),
            _ => None,
        }
    }

    /// Return a durable receipt classification when one was proved despite
    /// the overall promotion operation failing.
    ///
    /// This deliberately does not turn an ambiguous transaction report into a
    /// successful invocation outcome. Both an ordinary successful DB return
    /// followed by validation failure and a reconciled commit-report error may
    /// prove only that the exact receipt is durably promoted.
    pub(in crate::client) const fn durable_receipt_state(
        &self,
    ) -> Option<BootPublicationReceiptPromotionDurableState> {
        match self {
            Self::PostPromotion { .. } => {
                Some(BootPublicationReceiptPromotionDurableState::Promoted)
            }
            Self::DatabasePromotion(
                BootPublicationReceiptPromotionError::PostCommitDurableState { durable }
                | BootPublicationReceiptPromotionError::CommitReport { durable, .. },
            ) => Some(*durable),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
enum InitiallyAdmittedReceiptState {
    Pending,
    Promoted,
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
    StagedExactActiveReblitBootPublication<
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
    /// Consume terminal aggregate evidence into one exact receipt promotion.
    ///
    /// Exact retry is admitted only when the same receipt is already the
    /// committed head and the original `BootSyncStarted` binding still holds.
    /// Every result consumes `self`. A failure after a successful database
    /// return records that exact outcome but returns no replayable authority;
    /// an error returned by the database call retains only whatever durable
    /// classification that nested error could reconcile.
    pub(in crate::client) fn promote_terminal_receipt(
        self,
        client: &Client,
    ) -> Result<
        PromotedExactActiveReblitBootPublication<
            'plan,
            'inventory,
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        ActiveReblitBootReceiptPromotionError,
    > {
        let (initial_state, plan) = match self.staged.revalidate_against(client) {
            Ok(fresh) => (InitiallyAdmittedReceiptState::Pending, fresh.plan()),
            Err(pending) => match self.staged.revalidate_promoted_against(client) {
                Ok(fresh) => (InitiallyAdmittedReceiptState::Promoted, fresh.plan()),
                Err(promoted) => {
                    return Err(ActiveReblitBootReceiptPromotionError::InitialAdmission {
                        pending,
                        promoted,
                    });
                }
            },
        };

        before_fresh_admission();
        self.validate_exact_terminal_evidence(plan, "initial terminal admission")
            .map_err(ActiveReblitBootReceiptPromotionError::TerminalEvidence)?;

        before_immediate_pre_promotion_terminal_check();
        self.validate_exact_terminal_evidence(plan, "immediate pre-promotion")
            .map_err(ActiveReblitBootReceiptPromotionError::TerminalEvidence)?;
        match initial_state {
            InitiallyAdmittedReceiptState::Pending => {
                let fresh = self
                    .staged
                    .revalidate_against(client)
                    .map_err(ActiveReblitBootReceiptPromotionError::PrePromotionPending)?;
                if !std::ptr::eq(fresh.plan(), plan) {
                    return Err(ActiveReblitBootReceiptPromotionError::PlanMismatch {
                        checkpoint: "immediate pending receipt revalidation",
                    });
                }
            }
            InitiallyAdmittedReceiptState::Promoted => {
                let fresh = self
                    .staged
                    .revalidate_promoted_against(client)
                    .map_err(
                        ActiveReblitBootReceiptPromotionError::PrePromotionAlreadyPromoted,
                    )?;
                if !std::ptr::eq(fresh.plan(), plan) {
                    return Err(ActiveReblitBootReceiptPromotionError::PlanMismatch {
                        checkpoint: "immediate promoted receipt revalidation",
                    });
                }
            }
        }

        // The staged cross-store proof above can block. Recheck the inherited
        // deadline after the last caller-visible race seam; the database also
        // enforces that same deadline after acquiring its exclusive
        // transaction and immediately before changing the head.
        after_pre_promotion_revalidation();
        require_deadline("immediate database promotion", plan.input_deadline())
            .map_err(ActiveReblitBootReceiptPromotionError::TerminalEvidence)?;
        let database_outcome = client
            .state_db
            .promote_boot_publication_receipt(self.staged.receipt(), plan.input_deadline())
            .map_err(ActiveReblitBootReceiptPromotionError::DatabasePromotion)?;

        after_database_promotion();
        self.validate_after_promotion(client, plan, database_outcome, "post-promotion")?;

        before_final_promoted_validation();
        self.validate_after_promotion(client, plan, database_outcome, "final return")?;

        Ok(PromotedExactActiveReblitBootPublication {
            terminal: self,
            database_outcome,
        })
    }

    fn validate_after_promotion(
        &self,
        client: &Client,
        plan: &'plan BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        outcome: BootPublicationReceiptPromotionOutcome,
        checkpoint: &'static str,
    ) -> Result<(), ActiveReblitBootReceiptPromotionError> {
        self.validate_exact_terminal_evidence(plan, checkpoint)
            .map_err(|source| ActiveReblitBootReceiptPromotionError::PostPromotion {
                outcome,
                source: ActiveReblitBootPostPromotionValidationError::TerminalEvidence {
                    checkpoint,
                    source,
                },
            })?;
        let fresh = self
            .staged
            .revalidate_promoted_against(client)
            .map_err(|source| ActiveReblitBootReceiptPromotionError::PostPromotion {
                outcome,
                source:
                    ActiveReblitBootPostPromotionValidationError::PromotedStagedEvidence {
                        checkpoint,
                        source,
                    },
            })?;
        if !std::ptr::eq(fresh.plan(), plan) {
            return Err(ActiveReblitBootReceiptPromotionError::PostPromotion {
                outcome,
                source: ActiveReblitBootPostPromotionValidationError::PlanMismatch {
                    checkpoint,
                },
            });
        }
        Ok(())
    }

    fn validate_exact_terminal_evidence(
        &self,
        plan: &'plan BoundActiveReblitBlsPublicationPlan<
            'input,
            'topology_view,
            'topology_authority,
            'attempt,
            'stone,
            'roots,
        >,
        checkpoint: &'static str,
    ) -> Result<(), ActiveReblitBootTerminalEvidenceValidationError> {
        validate_exact_terminal_evidence_snapshot(
            plan,
            self.receipt_fingerprint(),
            self.publication_count,
            self.published_count,
            self.already_exact_count,
            self.replaced_count,
            &self.evidence,
            checkpoint,
        )?;
        validate_applied_replacement_pairs(plan, &self.evidence, checkpoint)
    }
}

fn require_deadline(
    checkpoint: &'static str,
    deadline: Instant,
) -> Result<(), ActiveReblitBootTerminalEvidenceValidationError> {
    #[cfg(test)]
    if FORCE_EXPIRED_DEADLINE.with(|forced| forced.replace(false)) {
        return Err(ActiveReblitBootTerminalEvidenceValidationError::DeadlineExceeded {
            checkpoint,
            deadline,
        });
    }
    if Instant::now() > deadline {
        Err(ActiveReblitBootTerminalEvidenceValidationError::DeadlineExceeded {
            checkpoint,
            deadline,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_FRESH_ADMISSION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_IMMEDIATE_PRE_PROMOTION_TERMINAL_CHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_PRE_PROMOTION_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_DATABASE_PROMOTION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_FINAL_PROMOTED_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static FORCE_EXPIRED_DEADLINE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
pub(super) fn arm_before_fresh_admission(callback: impl FnOnce() + 'static) {
    arm_callback(&BEFORE_FRESH_ADMISSION, callback);
}

#[cfg(test)]
pub(super) fn arm_before_immediate_pre_promotion_terminal_check(
    callback: impl FnOnce() + 'static,
) {
    arm_callback(&BEFORE_IMMEDIATE_PRE_PROMOTION_TERMINAL_CHECK, callback);
}

#[cfg(test)]
pub(super) fn arm_after_pre_promotion_revalidation(callback: impl FnOnce() + 'static) {
    arm_callback(&AFTER_PRE_PROMOTION_REVALIDATION, callback);
}

#[cfg(test)]
pub(super) fn arm_after_database_promotion(callback: impl FnOnce() + 'static) {
    arm_callback(&AFTER_DATABASE_PROMOTION, callback);
}

#[cfg(test)]
pub(super) fn arm_before_final_promoted_validation(callback: impl FnOnce() + 'static) {
    arm_callback(&BEFORE_FINAL_PROMOTED_VALIDATION, callback);
}

#[cfg(test)]
pub(super) fn arm_expired_deadline() {
    FORCE_EXPIRED_DEADLINE.with(|forced| {
        assert!(!forced.replace(true));
    });
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
fn before_fresh_admission() {
    run_callback(&BEFORE_FRESH_ADMISSION);
}

#[cfg(not(test))]
fn before_fresh_admission() {}

#[cfg(test)]
fn before_immediate_pre_promotion_terminal_check() {
    run_callback(&BEFORE_IMMEDIATE_PRE_PROMOTION_TERMINAL_CHECK);
}

#[cfg(not(test))]
fn before_immediate_pre_promotion_terminal_check() {}

#[cfg(test)]
fn after_pre_promotion_revalidation() {
    run_callback(&AFTER_PRE_PROMOTION_REVALIDATION);
}

#[cfg(not(test))]
fn after_pre_promotion_revalidation() {}

#[cfg(test)]
fn after_database_promotion() {
    run_callback(&AFTER_DATABASE_PROMOTION);
}

#[cfg(not(test))]
fn after_database_promotion() {}

#[cfg(test)]
fn before_final_promoted_validation() {
    run_callback(&BEFORE_FINAL_PROMOTED_VALIDATION);
}

#[cfg(not(test))]
fn before_final_promoted_validation() {}

#[path = "receipt_promotion/terminal_evidence.rs"]
mod terminal_evidence;
pub(in crate::client) use terminal_evidence::ActiveReblitBootTerminalEvidenceValidationError;
#[path = "receipt_promotion/replacement_pair_validation.rs"]
mod replacement_pair_validation;

#[path = "receipt_promotion/promoted_cleanup.rs"]
mod promoted_cleanup;
pub(in crate::client) use promoted_cleanup::ActiveReblitBootPromotedCleanupError;

#[path = "receipt_promotion/boot_sync_completion.rs"]
mod boot_sync_completion;
pub(in crate::client) use boot_sync_completion::{
    ActiveReblitBootSyncCompletionError,
    CompletedExactActiveReblitBootPublication,
};
#[cfg(test)]
pub(super) use boot_sync_completion::{
    ActiveReblitBootPostCompletionValidationError,
    arm_after_boot_sync_complete_persistence,
    arm_after_initial_completion_handoff,
    arm_before_completion_deadline,
    arm_before_final_completion_validation,
    assert_after_boot_sync_complete_persistence_hook_consumed,
    assert_after_initial_completion_handoff_hook_consumed,
    assert_before_completion_deadline_hook_consumed,
    assert_before_final_completion_validation_hook_consumed,
};
