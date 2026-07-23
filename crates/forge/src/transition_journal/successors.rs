use super::{
    BootRollback, CodecError, ForwardPhase, Operation, Phase, RollbackAction, RollbackPlan, TransitionRecord,
    codec::PAYLOAD_VERSION,
    model::CandidateRollback,
    validation::{next_forward_phase, next_rollback_phase, rollback_action_phase, rollback_allowed, validate_advance},
};

/// Initial classification of one rollback effect at the durable decision
/// boundary. `Pending` means the coordinator must perform or reconcile the
/// effect; `AlreadySatisfied` records exact evidence that no mutation remains.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InitialRollbackAction {
    Pending,
    AlreadySatisfied,
}

impl From<InitialRollbackAction> for RollbackAction {
    fn from(value: InitialRollbackAction) -> Self {
        match value {
            InitialRollbackAction::Pending => Self::Pending,
            InitialRollbackAction::AlreadySatisfied => Self::AlreadySatisfied,
        }
    }
}

/// Exact namespace/database observations used to persist `RollbackDecided`.
/// Optional actions must be `Some` exactly when the source phase makes that
/// recovery effect possible; the candidate is always classified.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RollbackObservations {
    pub(crate) allocated_candidate_id: Option<i32>,
    pub(crate) previous_archive: Option<InitialRollbackAction>,
    pub(crate) usr_exchange: Option<InitialRollbackAction>,
    pub(crate) candidate: InitialRollbackAction,
    pub(crate) fresh_db: Option<InitialRollbackAction>,
}

/// Provenance for the reconciliation invocation which completes one persisted
/// rollback intent.
///
/// This records what the completing invocation did, not which earlier
/// invocation may have produced the namespace or database state it observes.
/// `Applied` is valid if and only if this invocation performed the action.
/// `AlreadySatisfied` is valid if and only if the desired state was exact at
/// admission and this invocation performed no action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RollbackActionOutcome {
    /// This reconciliation invocation performed the action.
    Applied,
    /// The desired state was exact at admission, so this reconciliation
    /// invocation performed no action.
    AlreadySatisfied,
}

impl From<RollbackActionOutcome> for RollbackAction {
    fn from(value: RollbackActionOutcome) -> Self {
        match value {
            RollbackActionOutcome::Applied => Self::Applied,
            RollbackActionOutcome::AlreadySatisfied => Self::AlreadySatisfied,
        }
    }
}

/// Exact result of a separately authorized boot-repair invocation.
///
/// As with ordinary rollback outcomes, this records what the completing
/// invocation did. It does not infer success from the historical journal
/// phase or from a best-effort inspection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootRepairOutcome {
    /// This reconciliation invocation applied the authorized repair.
    Applied,
    /// Exact admission evidence proved that the repair was already complete,
    /// so this reconciliation invocation performed no action.
    AlreadySatisfied,
}

impl From<BootRepairOutcome> for BootRollback {
    fn from(value: BootRepairOutcome) -> Self {
        match value {
            BootRepairOutcome::Applied => Self::Applied,
            BootRepairOutcome::AlreadySatisfied => Self::AlreadySatisfied,
        }
    }
}

impl TransitionRecord {
    /// Construct an ordinary legal forward successor without exposing mutable
    /// record fields to the coordinator. A candidate ID is accepted only for
    /// the durable `FreshStateAllocating -> FreshStateAllocated` boundary;
    /// boot-publication entry and completion use their receipt-bound
    /// constructors instead.
    pub(crate) fn forward_successor(&self, allocated_candidate_id: Option<i32>) -> Result<Self, CodecError> {
        self.validate()?;
        let current = self.phase.forward().ok_or(CodecError::IllegalPhaseAdvance {
            current: self.phase,
            next: self.phase,
        })?;
        let next_phase = next_forward_phase(self, current).ok_or(CodecError::TerminalPhaseAdvance)?;
        if next_phase == ForwardPhase::BootSyncStarted {
            return Err(CodecError::ExplicitBootSyncStartedSuccessorRequired);
        }
        if next_phase == ForwardPhase::BootSyncComplete {
            return Err(CodecError::ExplicitBootSyncCompleteSuccessorRequired);
        }
        let allocation_completed =
            (current, next_phase) == (ForwardPhase::FreshStateAllocating, ForwardPhase::FreshStateAllocated);
        if !allocation_completed && allocated_candidate_id.is_some() {
            return Err(CodecError::CandidateStateChangedIllegally);
        }

        let mut next = self.clone();
        next.generation = self.generation.checked_add(1).ok_or(CodecError::GenerationExhausted)?;
        next.phase = next_phase.into();
        if allocation_completed {
            next.candidate.id = allocated_candidate_id;
        }
        validate_advance(self, &next)?;
        Ok(next)
    }

    /// Enter boot publication while durably binding the exact committed and
    /// pending receipt fingerprints. This is the sole v3 `None -> Some`
    /// receipt-correlation edge; generic forward advancement cannot enter it.
    pub(crate) fn boot_sync_started_successor(
        &self,
        receipts: crate::boot_publication::BootPublicationReceiptPair,
    ) -> Result<Self, CodecError> {
        self.validate()?;
        if self.version != PAYLOAD_VERSION {
            return Err(CodecError::PayloadVersionBootPublicationReceiptsMismatch(
                self.version,
            ));
        }
        let current = self.phase.forward().ok_or(CodecError::IllegalPhaseAdvance {
            current: self.phase,
            next: Phase::BootSyncStarted,
        })?;
        if next_forward_phase(self, current) != Some(ForwardPhase::BootSyncStarted) {
            return Err(CodecError::IllegalPhaseAdvance {
                current: self.phase,
                next: Phase::BootSyncStarted,
            });
        }

        let mut next = self.clone();
        next.generation = self.generation.checked_add(1).ok_or(CodecError::GenerationExhausted)?;
        next.phase = Phase::BootSyncStarted;
        next.boot_publication_receipts = Some(receipts);
        validate_advance(self, &next)?;
        Ok(next)
    }

    /// Complete boot publication only for the exact receipt pair which the
    /// validated v3 `BootSyncStarted` record already binds. Generic forward
    /// advancement cannot cross this evidence-bearing boundary.
    pub(crate) fn boot_sync_complete_successor(
        &self,
        expected_pair: crate::boot_publication::BootPublicationReceiptPair,
    ) -> Result<Self, CodecError> {
        self.validate()?;
        if self.version != PAYLOAD_VERSION {
            return Err(CodecError::PayloadVersionBootPublicationReceiptsMismatch(
                self.version,
            ));
        }
        if self.phase != Phase::BootSyncStarted {
            return Err(CodecError::IllegalPhaseAdvance {
                current: self.phase,
                next: Phase::BootSyncComplete,
            });
        }
        if self.boot_publication_receipts != Some(expected_pair) {
            return Err(CodecError::BootPublicationReceiptsChangedIllegally);
        }

        let mut next = self.clone();
        next.generation = self.generation.checked_add(1).ok_or(CodecError::GenerationExhausted)?;
        next.phase = Phase::BootSyncComplete;
        validate_advance(self, &next)?;
        Ok(next)
    }

    /// Derive and validate the durable rollback plan from exact observations.
    /// Requirements and disposition are computed from the persisted forward
    /// phase; callers cannot disable a required recovery action.
    pub(crate) fn rollback_decision(&self, observations: RollbackObservations) -> Result<Self, CodecError> {
        self.validate()?;
        let source = self.phase.forward().ok_or(CodecError::IllegalPhaseAdvance {
            current: self.phase,
            next: Phase::RollbackDecided,
        })?;
        if !rollback_allowed(self, source) {
            return Err(CodecError::InvalidRollbackSource(source));
        }

        let previous_possible =
            self.options.archive_previous && source.ordinal() >= ForwardPhase::PreviousArchiveIntent.ordinal();
        let usr_possible = source.ordinal() >= ForwardPhase::UsrExchangeIntent.ordinal();
        let fresh_possible = matches!(self.operation, Operation::NewState)
            && source.ordinal() >= ForwardPhase::FreshStateAllocating.ordinal();
        let boot_possible = source == ForwardPhase::BootSyncStarted;

        let mut next = self.clone();
        next.generation = self.generation.checked_add(1).ok_or(CodecError::GenerationExhausted)?;
        next.phase = Phase::RollbackDecided;
        if next.candidate.id.is_none() {
            next.candidate.id = observations.allocated_candidate_id;
        } else if observations.allocated_candidate_id.is_some() {
            return Err(CodecError::CandidateStateChangedIllegally);
        }
        next.rollback = Some(RollbackPlan {
            source,
            previous_archive: observed_initial_action(
                "previous-archive",
                previous_possible,
                observations.previous_archive,
            )?,
            usr_exchange: observed_initial_action("usr-exchange", usr_possible, observations.usr_exchange)?,
            candidate: CandidateRollback {
                action: observations.candidate.into(),
                disposition: self.candidate_disposition_for(source),
            },
            fresh_db: observed_initial_action("fresh-db", fresh_possible, observations.fresh_db)?,
            boot: if boot_possible {
                BootRollback::PendingUnverifiable
            } else {
                BootRollback::NotRequired
            },
            external_effects_may_remain: (self.runs_transaction_triggers()
                && source.ordinal() >= ForwardPhase::TransactionTriggersStarted.ordinal())
                || (self.options.run_system_triggers
                    && source.ordinal() >= ForwardPhase::SystemTriggersStarted.ordinal())
                || boot_possible,
        });
        validate_advance(self, &next)?;
        Ok(next)
    }

    /// Construct the legal ordinary rollback successor. Exactly one explicit
    /// outcome is required when completing an ordinary persisted intent;
    /// ordinary routing phases accept none. Every boot-repair edge is excluded
    /// and must use its exact typed constructor.
    pub(crate) fn rollback_successor(&self, outcome: Option<RollbackActionOutcome>) -> Result<Self, CodecError> {
        self.validate()?;
        if matches!(
            self.phase,
            Phase::BootRepairRequired | Phase::BootRepairStarted | Phase::BootRepairComplete
        ) {
            return Err(CodecError::ExplicitBootRepairSuccessorRequired(self.phase));
        }
        let plan = self.rollback.as_ref().ok_or(CodecError::MissingRollbackPlan)?;
        let next_phase = next_rollback_phase(plan, self.phase).ok_or_else(|| {
            if self.phase.blocks_advance() {
                CodecError::TerminalPhaseAdvance
            } else {
                CodecError::IllegalPhaseAdvance {
                    current: self.phase,
                    next: self.phase,
                }
            }
        })?;
        let ordinary_completion = rollback_action_phase(self.phase).is_some_and(|(_, completed)| !completed)
            && rollback_action_phase(next_phase).is_some_and(|(_, completed)| completed);
        if ordinary_completion != outcome.is_some() {
            return Err(CodecError::RollbackActionOutcomeMismatch);
        }

        let mut next = self.clone();
        next.generation = self.generation.checked_add(1).ok_or(CodecError::GenerationExhausted)?;
        next.phase = next_phase;
        let next_plan = next.rollback.as_mut().expect("validated rollback plan");
        if let Some(outcome) = outcome {
            let status = outcome.into();
            match (self.phase, next_phase) {
                (Phase::PreviousRestoreIntent, Phase::PreviousRestoredToStaging) => {
                    next_plan.previous_archive = status;
                }
                (Phase::ReverseExchangeIntent, Phase::UsrRestored) => next_plan.usr_exchange = status,
                (Phase::CandidatePreserveIntent, Phase::CandidatePreserved) => next_plan.candidate.action = status,
                (Phase::FreshDbInvalidationIntent, Phase::FreshDbInvalidated) => next_plan.fresh_db = status,
                _ => return Err(CodecError::RollbackActionOutcomeMismatch),
            }
        }
        validate_advance(self, &next)?;
        Ok(next)
    }

    /// Persist the exact routing edge into the boot-repair intent.
    pub(crate) fn boot_repair_started_successor(&self) -> Result<Self, CodecError> {
        self.validate()?;
        if self.phase != Phase::BootRepairRequired {
            return Err(CodecError::IllegalPhaseAdvance {
                current: self.phase,
                next: Phase::BootRepairStarted,
            });
        }

        let mut next = self.clone();
        next.generation = self.generation.checked_add(1).ok_or(CodecError::GenerationExhausted)?;
        next.phase = Phase::BootRepairStarted;
        validate_advance(self, &next)?;
        Ok(next)
    }

    /// Persist a verified boot-repair result from the exact started intent.
    ///
    /// This successor belongs only to the v2 payload domain. A v1 record is
    /// kept immutable and fails validation rather than being rewritten or
    /// silently upgraded.
    pub(crate) fn boot_repair_complete_successor(&self, outcome: BootRepairOutcome) -> Result<Self, CodecError> {
        self.validate()?;
        if self.phase != Phase::BootRepairStarted {
            return Err(CodecError::IllegalPhaseAdvance {
                current: self.phase,
                next: Phase::BootRepairComplete,
            });
        }

        let mut next = self.clone();
        next.generation = self.generation.checked_add(1).ok_or(CodecError::GenerationExhausted)?;
        next.phase = Phase::BootRepairComplete;
        next.rollback.as_mut().ok_or(CodecError::MissingRollbackPlan)?.boot = outcome.into();
        validate_advance(self, &next)?;
        Ok(next)
    }

    /// Persist that a started boot repair cannot be verified automatically.
    /// This is the conservative successor for both supported payload versions.
    pub(crate) fn boot_repair_unverified_successor(&self) -> Result<Self, CodecError> {
        self.validate()?;
        if self.phase != Phase::BootRepairStarted {
            return Err(CodecError::IllegalPhaseAdvance {
                current: self.phase,
                next: Phase::BootRepairUnverified,
            });
        }

        let mut next = self.clone();
        next.generation = self.generation.checked_add(1).ok_or(CodecError::GenerationExhausted)?;
        next.phase = Phase::BootRepairUnverified;
        next.rollback.as_mut().ok_or(CodecError::MissingRollbackPlan)?.boot = BootRollback::Unverified;
        validate_advance(self, &next)?;
        Ok(next)
    }

    /// Persist terminal rollback completion after a verified boot repair.
    pub(crate) fn boot_repair_rollback_complete_successor(&self) -> Result<Self, CodecError> {
        self.validate()?;
        if self.phase != Phase::BootRepairComplete {
            return Err(CodecError::IllegalPhaseAdvance {
                current: self.phase,
                next: Phase::RollbackComplete,
            });
        }

        let mut next = self.clone();
        next.generation = self.generation.checked_add(1).ok_or(CodecError::GenerationExhausted)?;
        next.phase = Phase::RollbackComplete;
        validate_advance(self, &next)?;
        Ok(next)
    }
}

fn observed_initial_action(
    action: &'static str,
    possible: bool,
    observation: Option<InitialRollbackAction>,
) -> Result<RollbackAction, CodecError> {
    match (possible, observation) {
        (true, Some(observation)) => Ok(observation.into()),
        (false, None) => Ok(RollbackAction::NotRequired),
        (possible, observation) => Err(CodecError::InvalidRollbackRequirement {
            action,
            status: observation.map_or(RollbackAction::NotRequired, Into::into),
            possible,
        }),
    }
}
