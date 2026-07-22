//! Consuming authority from terminal immutable publication into receipt promotion.
//!
//! The scalar leaf evidence retained by the publication attempt is historical,
//! not ongoing namespace authority. Promotion therefore repeats complete
//! read-only publication preflight under the original deadline, authenticates
//! the exact staged journal and database state before requesting the sole
//! database mutation, and repeats both sides after a successful database
//! return. The promoted terminal value can then be consumed by this module's
//! completion child to persist the exact receipt-bound `BootSyncComplete`
//! successor. Neither boundary can replace or delete a boot file, decide the
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
    linux_fs::{
        descriptor_boot_namespace::BootNamespaceDestinationState,
        mount_namespace::{
            RetainedBootFilePublicationOutcome,
            ValidatedRetainedBootFilePublication,
        },
    },
};

use super::{
    StagedExactActiveReblitBootPublication,
    super::ActiveReblitBootPublicationPreflightError,
};

/// Terminal exact publication whose canonical receipt is now committed.
///
/// The original terminal token remains owned rather than being projected into
/// detached receipt data. This value is non-`Clone` and is the sole input to
/// the exact `BootSyncComplete` persistence boundary.
#[must_use = "promoted boot-publication authority must be durably completed or deliberately discarded"]
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

    pub(in crate::client) fn evidence(&self) -> &[ValidatedRetainedBootFilePublication] {
        self.terminal.evidence()
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

/// Failure while proving that historical terminal evidence is still exact.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootTerminalEvidenceValidationError {
    #[error("the terminal promotion deadline {deadline:?} expired at {checkpoint}")]
    DeadlineExceeded {
        checkpoint: &'static str,
        deadline: Instant,
    },
    #[error("prepare a fresh read-only publication preflight at {checkpoint}")]
    Preflight {
        checkpoint: &'static str,
        #[source]
        source: ActiveReblitBootPublicationPreflightError,
    },
    #[error("terminal publication count at {checkpoint} is {actual}, expected {expected}")]
    PublicationCountMismatch {
        checkpoint: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("terminal publication counters overflowed at {checkpoint}")]
    PublicationCounterOverflow { checkpoint: &'static str },
    #[error(
        "terminal publication outcomes at {checkpoint} are published={published}, already-exact={already_exact}; retained counters are published={retained_published}, already-exact={retained_already_exact}"
    )]
    PublicationOutcomeMismatch {
        checkpoint: &'static str,
        published: usize,
        already_exact: usize,
        retained_published: usize,
        retained_already_exact: usize,
    },
    #[error("terminal scalar evidence for output {plan_index} differs from its retained plan at {checkpoint}")]
    EvidenceMismatch {
        checkpoint: &'static str,
        plan_index: usize,
    },
    #[error("fresh output {plan_index} is {state:?}, not Exact, at {checkpoint}")]
    DestinationNotExact {
        checkpoint: &'static str,
        plan_index: usize,
        state: BootNamespaceDestinationState,
    },
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
            self.publication_count,
            self.published_count,
            self.already_exact_count,
            &self.evidence,
            checkpoint,
        )
    }
}

fn validate_exact_terminal_evidence_snapshot<
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    plan: &BoundActiveReblitBlsPublicationPlan<
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
    evidence: &[ValidatedRetainedBootFilePublication],
    checkpoint: &'static str,
) -> Result<(), ActiveReblitBootTerminalEvidenceValidationError> {
    require_deadline(checkpoint, plan.input_deadline())?;
    let expected = plan.publication_count();
    if publication_count != expected {
        return Err(
            ActiveReblitBootTerminalEvidenceValidationError::PublicationCountMismatch {
                checkpoint,
                expected,
                actual: publication_count,
            },
        );
    }
    if evidence.len() != expected {
        return Err(
            ActiveReblitBootTerminalEvidenceValidationError::PublicationCountMismatch {
                checkpoint,
                expected,
                actual: evidence.len(),
            },
        );
    }

    let mut published = 0usize;
    let mut already_exact = 0usize;
    for (plan_index, (retained, output)) in
        evidence.iter().copied().zip(plan.outputs()).enumerate()
    {
        if retained.length() != output.expected_length()
            || retained.xxh3() != output.expected_digest()
            || retained.sha256() != *output.expected_content_identity().as_bytes()
        {
            return Err(
                ActiveReblitBootTerminalEvidenceValidationError::EvidenceMismatch {
                    checkpoint,
                    plan_index,
                },
            );
        }
        match retained.outcome() {
            RetainedBootFilePublicationOutcome::Published => {
                published = published.checked_add(1).ok_or(
                    ActiveReblitBootTerminalEvidenceValidationError::PublicationCounterOverflow {
                        checkpoint,
                    },
                )?;
            }
            RetainedBootFilePublicationOutcome::AlreadyExact => {
                already_exact = already_exact.checked_add(1).ok_or(
                    ActiveReblitBootTerminalEvidenceValidationError::PublicationCounterOverflow {
                        checkpoint,
                    },
                )?;
            }
        }
    }
    if published != published_count || already_exact != already_exact_count {
        return Err(
            ActiveReblitBootTerminalEvidenceValidationError::PublicationOutcomeMismatch {
                checkpoint,
                published,
                already_exact,
                retained_published: published_count,
                retained_already_exact: already_exact_count,
            },
        );
    }
    let accounted = published.checked_add(already_exact).ok_or(
        ActiveReblitBootTerminalEvidenceValidationError::PublicationCounterOverflow {
            checkpoint,
        },
    )?;
    if accounted != expected {
        return Err(
            ActiveReblitBootTerminalEvidenceValidationError::PublicationCountMismatch {
                checkpoint,
                expected,
                actual: accounted,
            },
        );
    }

    let preflight = plan
        .prepare_boot_publication_preflight()
        .map_err(|source| ActiveReblitBootTerminalEvidenceValidationError::Preflight {
            checkpoint,
            source,
        })?;
    if preflight.publication_count() != expected {
        return Err(
            ActiveReblitBootTerminalEvidenceValidationError::PublicationCountMismatch {
                checkpoint,
                expected,
                actual: preflight.publication_count(),
            },
        );
    }
    for (plan_index, state) in preflight.initial_states().iter().copied().enumerate() {
        if state != BootNamespaceDestinationState::Exact {
            return Err(
                ActiveReblitBootTerminalEvidenceValidationError::DestinationNotExact {
                    checkpoint,
                    plan_index,
                    state,
                },
            );
        }
    }
    require_deadline(checkpoint, plan.input_deadline())
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
