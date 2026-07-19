use super::{
    codec::{CodecError, PAYLOAD_FORMAT, PAYLOAD_VERSION, PAYLOAD_VERSION_V1},
    model::{
        AbortDisposition, BootRollback, ForwardPhase, MountNamespaceIdentity, Operation, Phase, PreviousOrigin,
        RollbackAction, RollbackPlan, RuntimeEpoch, RuntimeTreeIdentity, TransitionRecord,
    },
};

impl ForwardPhase {
    pub(super) fn ordinal(self) -> u8 {
        match self {
            Self::Preparing => 0,
            Self::FreshStateAllocating => 1,
            Self::FreshStateAllocated => 2,
            Self::CandidatePrepareStarted => 3,
            Self::CandidatePrepared => 4,
            Self::TransactionTriggersStarted => 5,
            Self::TransactionTriggersComplete => 6,
            Self::UsrExchangeIntent => 7,
            Self::UsrExchanged => 8,
            Self::RootLinksComplete => 9,
            Self::SystemTriggersStarted => 10,
            Self::SystemTriggersComplete => 11,
            Self::PreviousArchiveIntent => 12,
            Self::PreviousArchived => 13,
            Self::BootSyncStarted => 14,
            Self::BootSyncComplete => 15,
            Self::CommitDecided => 16,
            Self::CommitCleanupComplete => 17,
            Self::Complete => 18,
        }
    }
}

impl From<ForwardPhase> for Phase {
    fn from(value: ForwardPhase) -> Self {
        match value {
            ForwardPhase::Preparing => Self::Preparing,
            ForwardPhase::FreshStateAllocating => Self::FreshStateAllocating,
            ForwardPhase::FreshStateAllocated => Self::FreshStateAllocated,
            ForwardPhase::CandidatePrepareStarted => Self::CandidatePrepareStarted,
            ForwardPhase::CandidatePrepared => Self::CandidatePrepared,
            ForwardPhase::TransactionTriggersStarted => Self::TransactionTriggersStarted,
            ForwardPhase::TransactionTriggersComplete => Self::TransactionTriggersComplete,
            ForwardPhase::UsrExchangeIntent => Self::UsrExchangeIntent,
            ForwardPhase::UsrExchanged => Self::UsrExchanged,
            ForwardPhase::RootLinksComplete => Self::RootLinksComplete,
            ForwardPhase::SystemTriggersStarted => Self::SystemTriggersStarted,
            ForwardPhase::SystemTriggersComplete => Self::SystemTriggersComplete,
            ForwardPhase::PreviousArchiveIntent => Self::PreviousArchiveIntent,
            ForwardPhase::PreviousArchived => Self::PreviousArchived,
            ForwardPhase::BootSyncStarted => Self::BootSyncStarted,
            ForwardPhase::BootSyncComplete => Self::BootSyncComplete,
            ForwardPhase::CommitDecided => Self::CommitDecided,
            ForwardPhase::CommitCleanupComplete => Self::CommitCleanupComplete,
            ForwardPhase::Complete => Self::Complete,
        }
    }
}

impl Phase {
    pub(super) fn forward(self) -> Option<ForwardPhase> {
        Some(match self {
            Self::Preparing => ForwardPhase::Preparing,
            Self::FreshStateAllocating => ForwardPhase::FreshStateAllocating,
            Self::FreshStateAllocated => ForwardPhase::FreshStateAllocated,
            Self::CandidatePrepareStarted => ForwardPhase::CandidatePrepareStarted,
            Self::CandidatePrepared => ForwardPhase::CandidatePrepared,
            Self::TransactionTriggersStarted => ForwardPhase::TransactionTriggersStarted,
            Self::TransactionTriggersComplete => ForwardPhase::TransactionTriggersComplete,
            Self::UsrExchangeIntent => ForwardPhase::UsrExchangeIntent,
            Self::UsrExchanged => ForwardPhase::UsrExchanged,
            Self::RootLinksComplete => ForwardPhase::RootLinksComplete,
            Self::SystemTriggersStarted => ForwardPhase::SystemTriggersStarted,
            Self::SystemTriggersComplete => ForwardPhase::SystemTriggersComplete,
            Self::PreviousArchiveIntent => ForwardPhase::PreviousArchiveIntent,
            Self::PreviousArchived => ForwardPhase::PreviousArchived,
            Self::BootSyncStarted => ForwardPhase::BootSyncStarted,
            Self::BootSyncComplete => ForwardPhase::BootSyncComplete,
            Self::CommitDecided => ForwardPhase::CommitDecided,
            Self::CommitCleanupComplete => ForwardPhase::CommitCleanupComplete,
            Self::Complete => ForwardPhase::Complete,
            Self::RollbackDecided
            | Self::PreviousRestoreIntent
            | Self::PreviousRestoredToStaging
            | Self::ReverseExchangeIntent
            | Self::UsrRestored
            | Self::CandidatePreserveIntent
            | Self::CandidatePreserved
            | Self::FreshDbInvalidationIntent
            | Self::FreshDbInvalidated
            | Self::BootRepairRequired
            | Self::BootRepairStarted
            | Self::BootRepairComplete
            | Self::BootRepairUnverified
            | Self::RollbackComplete => return None,
        })
    }

    pub(super) fn blocks_advance(self) -> bool {
        matches!(
            self,
            Self::Complete | Self::BootRepairUnverified | Self::RollbackComplete
        )
    }

    pub(super) fn deletable(self) -> bool {
        matches!(self, Self::Complete | Self::RollbackComplete)
    }
}

impl RollbackAction {
    fn required(self) -> bool {
        self != Self::NotRequired
    }

    fn resolved(self) -> bool {
        matches!(self, Self::Applied | Self::AlreadySatisfied)
    }
}

impl TransitionRecord {
    pub(super) fn validate(&self) -> Result<(), CodecError> {
        if self.format != PAYLOAD_FORMAT {
            return Err(CodecError::UnsupportedPayloadFormat(self.format.clone()));
        }
        if !matches!(self.version, PAYLOAD_VERSION_V1 | PAYLOAD_VERSION) {
            return Err(CodecError::UnsupportedPayloadVersion(self.version));
        }
        if self.version == PAYLOAD_VERSION_V1 {
            if self.phase == Phase::BootRepairComplete {
                return Err(CodecError::PayloadVersionPhaseMismatch {
                    version: self.version,
                    phase: self.phase,
                });
            }
            if let Some(status @ (BootRollback::Applied | BootRollback::AlreadySatisfied)) =
                self.rollback.as_ref().map(|rollback| rollback.boot)
            {
                return Err(CodecError::PayloadVersionBootRollbackMismatch {
                    version: self.version,
                    status,
                });
            }
        }
        if self.generation == 0 {
            return Err(CodecError::ZeroGeneration);
        }
        self.creation_epoch.validate()?;
        self.previous.tree_token.validate()?;
        self.candidate.tree_token.validate()?;
        self.previous.usr_runtime_identity.validate()?;
        self.candidate.usr_runtime_identity.validate()?;
        for id in [self.candidate.id, self.previous.id].into_iter().flatten() {
            if id <= 0 {
                return Err(CodecError::InvalidStateId(id));
            }
        }

        let expected_origin = match self.operation {
            Operation::NewState => super::model::CandidateOrigin::Fresh,
            Operation::ActivateArchived => super::model::CandidateOrigin::Archived,
            Operation::ActiveReblit => super::model::CandidateOrigin::ActiveReblit,
        };
        if self.candidate.origin != expected_origin {
            return Err(CodecError::OperationOriginMismatch {
                operation: self.operation,
                origin: self.candidate.origin,
            });
        }

        let layout_phase = match (self.phase.forward(), self.rollback.as_ref()) {
            (Some(phase), None) => phase,
            (Some(_), Some(_)) => return Err(CodecError::RollbackPlanOnForwardPhase),
            (None, Some(rollback)) if rollback_allowed(self, rollback.source) => rollback.source,
            (None, Some(rollback)) => {
                return Err(CodecError::InvalidRollbackSource(rollback.source));
            }
            (None, None) => return Err(CodecError::MissingRollbackPlan),
        };
        self.validate_option_reachability(layout_phase)?;
        self.validate_candidate_layout(layout_phase)?;
        self.validate_relationships()?;
        if let Some(rollback) = &self.rollback {
            self.validate_rollback_plan(rollback)?;
            validate_rollback_phase(self.phase, rollback)?;
        }
        Ok(())
    }

    fn validate_option_reachability(&self, phase: ForwardPhase) -> Result<(), CodecError> {
        if matches!(
            phase,
            ForwardPhase::FreshStateAllocating | ForwardPhase::FreshStateAllocated
        ) && !matches!(self.operation, Operation::NewState)
        {
            return Err(CodecError::FreshPhaseForExistingCandidate);
        }
        if matches!(
            phase,
            ForwardPhase::TransactionTriggersStarted | ForwardPhase::TransactionTriggersComplete
        ) && !self.runs_transaction_triggers()
        {
            return Err(CodecError::DisabledPhase(Phase::from(phase)));
        }
        if matches!(
            phase,
            ForwardPhase::SystemTriggersStarted | ForwardPhase::SystemTriggersComplete
        ) && !self.options.run_system_triggers
        {
            return Err(CodecError::DisabledPhase(Phase::from(phase)));
        }
        if matches!(
            phase,
            ForwardPhase::PreviousArchiveIntent | ForwardPhase::PreviousArchived
        ) && !self.options.archive_previous
        {
            return Err(CodecError::DisabledPhase(Phase::from(phase)));
        }
        if matches!(phase, ForwardPhase::BootSyncStarted | ForwardPhase::BootSyncComplete)
            && !self.options.run_boot_sync
        {
            return Err(CodecError::DisabledPhase(Phase::from(phase)));
        }
        Ok(())
    }

    fn validate_candidate_layout(&self, phase: ForwardPhase) -> Result<(), CodecError> {
        match self.operation {
            Operation::NewState => {
                let id_required = match phase {
                    ForwardPhase::Preparing => false,
                    ForwardPhase::FreshStateAllocating => {
                        let Some(rollback) = self.rollback.as_ref() else {
                            return require_option_presence(self.candidate.id, false, CodecError::CandidateStateLayout);
                        };
                        match rollback.fresh_db {
                            RollbackAction::Pending | RollbackAction::Applied => true,
                            // An absent allocation is already satisfied with no
                            // ID, while a concurrently removed observed row
                            // retains its immutable ID as recovery evidence.
                            RollbackAction::AlreadySatisfied => return Ok(()),
                            RollbackAction::NotRequired => false,
                        }
                    }
                    ForwardPhase::FreshStateAllocated
                    | ForwardPhase::CandidatePrepareStarted
                    | ForwardPhase::CandidatePrepared
                    | ForwardPhase::TransactionTriggersStarted
                    | ForwardPhase::TransactionTriggersComplete
                    | ForwardPhase::UsrExchangeIntent
                    | ForwardPhase::UsrExchanged
                    | ForwardPhase::RootLinksComplete
                    | ForwardPhase::SystemTriggersStarted
                    | ForwardPhase::SystemTriggersComplete
                    | ForwardPhase::PreviousArchiveIntent
                    | ForwardPhase::PreviousArchived
                    | ForwardPhase::BootSyncStarted
                    | ForwardPhase::BootSyncComplete
                    | ForwardPhase::CommitDecided
                    | ForwardPhase::CommitCleanupComplete
                    | ForwardPhase::Complete => true,
                };
                require_option_presence(self.candidate.id, id_required, CodecError::CandidateStateLayout)?;
            }
            Operation::ActivateArchived | Operation::ActiveReblit => {
                if self.candidate.id.is_none() {
                    return Err(CodecError::ExistingCandidateStateMissing);
                }
            }
        }
        Ok(())
    }

    fn validate_relationships(&self) -> Result<(), CodecError> {
        if self.candidate.tree_token == self.previous.tree_token {
            return Err(CodecError::CandidatePreviousTreeTokenCollision);
        }

        let candidate = self.candidate.usr_runtime_identity;
        let previous = self.previous.usr_runtime_identity;
        if candidate.same_object(previous) {
            return Err(CodecError::CandidatePreviousObjectCollision);
        }
        if candidate.st_dev != previous.st_dev {
            return Err(CodecError::CandidatePreviousFilesystemMismatch {
                candidate: candidate.st_dev,
                previous: previous.st_dev,
            });
        }
        if candidate.mount_id != previous.mount_id {
            return Err(CodecError::CandidatePreviousMountMismatch {
                candidate: candidate.mount_id,
                previous: previous.mount_id,
            });
        }

        let archive_previous = matches!(self.previous.origin, PreviousOrigin::ActiveState);
        if self.options.archive_previous != archive_previous {
            return Err(CodecError::ArchiveOptionMismatch {
                origin: self.previous.origin,
                archive_previous: self.options.archive_previous,
            });
        }

        match self.operation {
            Operation::ActiveReblit => {
                if self.previous.origin != PreviousOrigin::ActiveReblitCorrupt
                    || self.previous.id.is_none()
                    || self.candidate.id != self.previous.id
                {
                    return Err(CodecError::ActiveReblitStateMismatch);
                }
            }
            Operation::ActivateArchived => {
                if self.previous.origin != PreviousOrigin::ActiveState
                    || self.previous.id.is_none()
                    || self.candidate.id.is_none()
                {
                    return Err(CodecError::ArchivedActivationStateMismatch);
                }
                if self.candidate.id.is_some() && self.candidate.id == self.previous.id {
                    return Err(CodecError::CandidatePreviousStateCollision);
                }
            }
            Operation::NewState => {
                match self.previous.origin {
                    PreviousOrigin::ActiveState if self.previous.id.is_none() => {
                        return Err(CodecError::PreviousOriginStateMismatch {
                            origin: self.previous.origin,
                            state_id: self.previous.id,
                        });
                    }
                    PreviousOrigin::SynthesizedEmpty | PreviousOrigin::Unmanaged if self.previous.id.is_some() => {
                        return Err(CodecError::PreviousOriginStateMismatch {
                            origin: self.previous.origin,
                            state_id: self.previous.id,
                        });
                    }
                    PreviousOrigin::ActiveReblitCorrupt => {
                        return Err(CodecError::PreviousOriginOperationMismatch {
                            operation: self.operation,
                            origin: self.previous.origin,
                        });
                    }
                    _ => {}
                }
                if self.candidate.id.is_some() && self.candidate.id == self.previous.id {
                    return Err(CodecError::CandidatePreviousStateCollision);
                }
            }
        }
        Ok(())
    }

    pub(super) fn runs_transaction_triggers(&self) -> bool {
        matches!(self.operation, Operation::NewState | Operation::ActiveReblit)
    }

    pub(super) fn candidate_disposition_for(&self, source: ForwardPhase) -> AbortDisposition {
        match self.operation {
            Operation::NewState | Operation::ActiveReblit => AbortDisposition::Quarantine,
            Operation::ActivateArchived if source == ForwardPhase::SystemTriggersStarted => {
                AbortDisposition::Quarantine
            }
            Operation::ActivateArchived => AbortDisposition::Rearchive,
        }
    }

    fn validate_rollback_plan(&self, rollback: &RollbackPlan) -> Result<(), CodecError> {
        validate_rollback_requirement(
            "previous-archive",
            rollback.previous_archive,
            self.options.archive_previous && rollback.source.ordinal() >= ForwardPhase::PreviousArchiveIntent.ordinal(),
        )?;
        validate_rollback_requirement(
            "usr-exchange",
            rollback.usr_exchange,
            rollback.source.ordinal() >= ForwardPhase::UsrExchangeIntent.ordinal(),
        )?;
        validate_rollback_requirement("candidate", rollback.candidate.action, true)?;
        validate_rollback_requirement(
            "fresh-db",
            rollback.fresh_db,
            matches!(self.operation, Operation::NewState)
                && rollback.source.ordinal() >= ForwardPhase::FreshStateAllocating.ordinal(),
        )?;

        let boot_possible = rollback.source == ForwardPhase::BootSyncStarted;
        match (boot_possible, rollback.boot) {
            (false, BootRollback::NotRequired)
            | (
                true,
                BootRollback::PendingUnverifiable
                | BootRollback::Applied
                | BootRollback::AlreadySatisfied
                | BootRollback::Unverified,
            ) => {}
            _ => return Err(CodecError::InvalidBootRollbackRequirement),
        }

        let expected_disposition = self.candidate_disposition_for(rollback.source);
        if rollback.candidate.disposition != expected_disposition {
            return Err(CodecError::InvalidCandidateDisposition {
                operation: self.operation,
                rollback_source: rollback.source,
                expected: expected_disposition,
                actual: rollback.candidate.disposition,
            });
        }

        let external_effects_may_remain = (self.runs_transaction_triggers()
            && rollback.source.ordinal() >= ForwardPhase::TransactionTriggersStarted.ordinal())
            || (self.options.run_system_triggers
                && rollback.source.ordinal() >= ForwardPhase::SystemTriggersStarted.ordinal())
            || rollback.source == ForwardPhase::BootSyncStarted;
        if rollback.external_effects_may_remain != external_effects_may_remain {
            return Err(CodecError::InvalidExternalEffectsEvidence {
                expected: external_effects_may_remain,
                actual: rollback.external_effects_may_remain,
            });
        }
        Ok(())
    }
}

impl MountNamespaceIdentity {
    fn validate(self) -> Result<(), CodecError> {
        if self.st_dev == 0 || self.inode == 0 {
            return Err(CodecError::ZeroMountNamespaceIdentity);
        }
        Ok(())
    }
}

impl RuntimeEpoch {
    fn validate(&self) -> Result<(), CodecError> {
        self.boot_id.validate()?;
        self.mount_namespace.validate()
    }
}

impl RuntimeTreeIdentity {
    fn validate(self) -> Result<(), CodecError> {
        if self.st_dev == 0 || self.inode == 0 || self.mount_id == 0 {
            return Err(CodecError::ZeroRuntimeTreeIdentity);
        }
        Ok(())
    }

    fn same_object(self, other: Self) -> bool {
        self.st_dev == other.st_dev && self.inode == other.inode
    }
}

fn require_option_presence<T>(value: Option<T>, required: bool, error: CodecError) -> Result<(), CodecError> {
    if value.is_some() != required {
        return Err(error);
    }
    Ok(())
}

pub(super) fn next_forward_phase(record: &TransitionRecord, current: ForwardPhase) -> Option<ForwardPhase> {
    let after_system = || {
        if record.options.archive_previous {
            ForwardPhase::PreviousArchiveIntent
        } else if record.options.run_boot_sync {
            ForwardPhase::BootSyncStarted
        } else {
            ForwardPhase::CommitDecided
        }
    };
    let after_archive = || {
        if record.options.run_boot_sync {
            ForwardPhase::BootSyncStarted
        } else {
            ForwardPhase::CommitDecided
        }
    };
    Some(match current {
        ForwardPhase::Preparing if matches!(record.operation, Operation::NewState) => {
            ForwardPhase::FreshStateAllocating
        }
        ForwardPhase::Preparing => ForwardPhase::CandidatePrepareStarted,
        ForwardPhase::FreshStateAllocating => ForwardPhase::FreshStateAllocated,
        ForwardPhase::FreshStateAllocated => ForwardPhase::CandidatePrepareStarted,
        ForwardPhase::CandidatePrepareStarted => ForwardPhase::CandidatePrepared,
        ForwardPhase::CandidatePrepared if record.runs_transaction_triggers() => {
            ForwardPhase::TransactionTriggersStarted
        }
        ForwardPhase::CandidatePrepared => ForwardPhase::UsrExchangeIntent,
        ForwardPhase::TransactionTriggersStarted => ForwardPhase::TransactionTriggersComplete,
        ForwardPhase::TransactionTriggersComplete => ForwardPhase::UsrExchangeIntent,
        ForwardPhase::UsrExchangeIntent => ForwardPhase::UsrExchanged,
        ForwardPhase::UsrExchanged => ForwardPhase::RootLinksComplete,
        ForwardPhase::RootLinksComplete if record.options.run_system_triggers => ForwardPhase::SystemTriggersStarted,
        ForwardPhase::RootLinksComplete => after_system(),
        ForwardPhase::SystemTriggersStarted => ForwardPhase::SystemTriggersComplete,
        ForwardPhase::SystemTriggersComplete => after_system(),
        ForwardPhase::PreviousArchiveIntent => ForwardPhase::PreviousArchived,
        ForwardPhase::PreviousArchived => after_archive(),
        ForwardPhase::BootSyncStarted => ForwardPhase::BootSyncComplete,
        ForwardPhase::BootSyncComplete => ForwardPhase::CommitDecided,
        ForwardPhase::CommitDecided => ForwardPhase::CommitCleanupComplete,
        ForwardPhase::CommitCleanupComplete => ForwardPhase::Complete,
        ForwardPhase::Complete => return None,
    })
}

pub(super) fn rollback_allowed(_record: &TransitionRecord, source: ForwardPhase) -> bool {
    source.ordinal() < ForwardPhase::CommitDecided.ordinal() && source != ForwardPhase::BootSyncComplete
}

fn validate_rollback_requirement(
    action: &'static str,
    status: RollbackAction,
    possible: bool,
) -> Result<(), CodecError> {
    if possible == status.required() {
        Ok(())
    } else {
        Err(CodecError::InvalidRollbackRequirement {
            action,
            status,
            possible,
        })
    }
}

fn rollback_actions(plan: &RollbackPlan) -> [RollbackAction; 4] {
    [
        plan.previous_archive,
        plan.usr_exchange,
        plan.candidate.action,
        plan.fresh_db,
    ]
}

fn ordinary_actions_resolved(plan: &RollbackPlan) -> bool {
    rollback_actions(plan)
        .into_iter()
        .all(|action| action == RollbackAction::NotRequired || action.resolved())
}

pub(super) fn rollback_action_phase(phase: Phase) -> Option<(usize, bool)> {
    Some(match phase {
        Phase::PreviousRestoreIntent => (0, false),
        Phase::PreviousRestoredToStaging => (0, true),
        Phase::ReverseExchangeIntent => (1, false),
        Phase::UsrRestored => (1, true),
        Phase::CandidatePreserveIntent => (2, false),
        Phase::CandidatePreserved => (2, true),
        Phase::FreshDbInvalidationIntent => (3, false),
        Phase::FreshDbInvalidated => (3, true),
        _ => return None,
    })
}

fn validate_rollback_phase(phase: Phase, plan: &RollbackPlan) -> Result<(), CodecError> {
    let actions = rollback_actions(plan);
    let matches_phase = match phase {
        Phase::RollbackDecided => {
            actions.into_iter().all(|action| action != RollbackAction::Applied)
                && matches!(plan.boot, BootRollback::NotRequired | BootRollback::PendingUnverifiable)
        }
        Phase::BootRepairRequired | Phase::BootRepairStarted => {
            ordinary_actions_resolved(plan) && plan.boot == BootRollback::PendingUnverifiable
        }
        Phase::BootRepairComplete => {
            ordinary_actions_resolved(plan)
                && matches!(plan.boot, BootRollback::Applied | BootRollback::AlreadySatisfied)
        }
        Phase::BootRepairUnverified => ordinary_actions_resolved(plan) && plan.boot == BootRollback::Unverified,
        Phase::RollbackComplete => {
            ordinary_actions_resolved(plan)
                && matches!(
                    plan.boot,
                    BootRollback::NotRequired | BootRollback::Applied | BootRollback::AlreadySatisfied
                )
        }
        _ => {
            let Some((current, completed)) = rollback_action_phase(phase) else {
                return Err(CodecError::RollbackPlanOnForwardPhase);
            };
            let prior_resolved = actions[..current]
                .iter()
                .all(|action| *action == RollbackAction::NotRequired || action.resolved());
            let current_matches = if completed {
                actions[current].resolved()
            } else {
                actions[current] == RollbackAction::Pending
            };
            let later_unapplied = actions[current + 1..]
                .iter()
                .all(|action| *action != RollbackAction::Applied);
            prior_resolved
                && current_matches
                && later_unapplied
                && matches!(plan.boot, BootRollback::NotRequired | BootRollback::PendingUnverifiable)
        }
    };
    if matches_phase {
        Ok(())
    } else {
        Err(CodecError::RollbackPlanPhaseMismatch { phase })
    }
}

pub(super) fn next_rollback_phase(plan: &RollbackPlan, current: Phase) -> Option<Phase> {
    match current {
        Phase::PreviousRestoreIntent => return Some(Phase::PreviousRestoredToStaging),
        Phase::ReverseExchangeIntent => return Some(Phase::UsrRestored),
        Phase::CandidatePreserveIntent => return Some(Phase::CandidatePreserved),
        Phase::FreshDbInvalidationIntent => return Some(Phase::FreshDbInvalidated),
        Phase::BootRepairRequired => return Some(Phase::BootRepairStarted),
        Phase::BootRepairStarted => return Some(Phase::BootRepairUnverified),
        Phase::BootRepairComplete => return Some(Phase::RollbackComplete),
        Phase::BootRepairUnverified | Phase::RollbackComplete => return None,
        Phase::RollbackDecided
        | Phase::PreviousRestoredToStaging
        | Phase::UsrRestored
        | Phase::CandidatePreserved
        | Phase::FreshDbInvalidated => {}
        _ => return None,
    }

    for (action, intent) in rollback_actions(plan).into_iter().zip([
        Phase::PreviousRestoreIntent,
        Phase::ReverseExchangeIntent,
        Phase::CandidatePreserveIntent,
        Phase::FreshDbInvalidationIntent,
    ]) {
        if action == RollbackAction::Pending {
            return Some(intent);
        }
    }
    match plan.boot {
        BootRollback::PendingUnverifiable => Some(Phase::BootRepairRequired),
        BootRollback::NotRequired => Some(Phase::RollbackComplete),
        BootRollback::Applied | BootRollback::AlreadySatisfied | BootRollback::Unverified => None,
    }
}

fn validate_rollback_plan_advance(
    expected: &RollbackPlan,
    next: &RollbackPlan,
    current_phase: Phase,
    next_phase: Phase,
) -> Result<(), CodecError> {
    if expected.source != next.source
        || expected.candidate.disposition != next.candidate.disposition
        || expected.external_effects_may_remain != next.external_effects_may_remain
    {
        return Err(CodecError::RollbackPlanChangedIllegally);
    }

    let completed_action = match (current_phase, next_phase) {
        (Phase::PreviousRestoreIntent, Phase::PreviousRestoredToStaging) => Some(0),
        (Phase::ReverseExchangeIntent, Phase::UsrRestored) => Some(1),
        (Phase::CandidatePreserveIntent, Phase::CandidatePreserved) => Some(2),
        (Phase::FreshDbInvalidationIntent, Phase::FreshDbInvalidated) => Some(3),
        _ => None,
    };
    for (index, (before, after)) in rollback_actions(expected)
        .into_iter()
        .zip(rollback_actions(next))
        .enumerate()
    {
        if Some(index) == completed_action {
            if before != RollbackAction::Pending || !after.resolved() {
                return Err(CodecError::RollbackPlanChangedIllegally);
            }
        } else if before != after {
            return Err(CodecError::RollbackPlanChangedIllegally);
        }
    }

    match (current_phase, next_phase) {
        (Phase::BootRepairStarted, Phase::BootRepairUnverified)
            if expected.boot == BootRollback::PendingUnverifiable && next.boot == BootRollback::Unverified => {}
        (Phase::BootRepairStarted, Phase::BootRepairComplete)
            if expected.boot == BootRollback::PendingUnverifiable
                && matches!(next.boot, BootRollback::Applied | BootRollback::AlreadySatisfied) => {}
        _ if expected.boot == next.boot => {}
        _ => return Err(CodecError::RollbackPlanChangedIllegally),
    }
    Ok(())
}

pub(super) fn validate_advance(expected: &TransitionRecord, next: &TransitionRecord) -> Result<(), CodecError> {
    expected.validate()?;
    next.validate()?;

    if expected.phase.blocks_advance() {
        return Err(CodecError::TerminalPhaseAdvance);
    }

    let expected_generation = expected
        .generation
        .checked_add(1)
        .ok_or(CodecError::GenerationExhausted)?;
    if next.generation != expected_generation {
        return Err(CodecError::GenerationMismatch {
            expected: expected_generation,
            actual: next.generation,
        });
    }
    if expected.transition_id != next.transition_id {
        return Err(CodecError::TransitionChanged);
    }
    if expected.format != next.format
        || expected.version != next.version
        || expected.creation_epoch != next.creation_epoch
        || expected.operation != next.operation
        || expected.previous != next.previous
        || expected.options != next.options
        || expected.quarantine_name != next.quarantine_name
        || expected.candidate.origin != next.candidate.origin
        || expected.candidate.tree_token != next.candidate.tree_token
        || expected.candidate.usr_runtime_identity != next.candidate.usr_runtime_identity
    {
        return Err(CodecError::ImmutableTransitionDataChanged);
    }

    match (expected.phase.forward(), next.phase.forward()) {
        (Some(current), Some(actual)) => {
            let wanted = next_forward_phase(expected, current).ok_or(CodecError::TerminalPhaseAdvance)?;
            if actual != wanted {
                return Err(CodecError::IllegalPhaseAdvance {
                    current: expected.phase,
                    next: next.phase,
                });
            }
        }
        (Some(current), None) => {
            let rollback_source = next.rollback.as_ref().map(|rollback| rollback.source);
            if next.phase != Phase::RollbackDecided
                || !rollback_allowed(expected, current)
                || rollback_source != Some(current)
            {
                return Err(CodecError::IllegalPhaseAdvance {
                    current: expected.phase,
                    next: next.phase,
                });
            }
        }
        (None, Some(_)) => {
            return Err(CodecError::IllegalPhaseAdvance {
                current: expected.phase,
                next: next.phase,
            });
        }
        (None, None) => {
            let expected_plan = expected.rollback.as_ref().expect("validated rollback plan");
            let next_plan = next.rollback.as_ref().expect("validated rollback plan");
            let boot_repair_completed =
                (expected.phase, next.phase) == (Phase::BootRepairStarted, Phase::BootRepairComplete);
            if !boot_repair_completed && next_rollback_phase(expected_plan, expected.phase) != Some(next.phase) {
                return Err(CodecError::IllegalPhaseAdvance {
                    current: expected.phase,
                    next: next.phase,
                });
            }
            validate_rollback_plan_advance(expected_plan, next_plan, expected.phase, next.phase)?;
        }
    }

    let allocation_completed = expected.phase == Phase::FreshStateAllocating
        && next.phase == Phase::FreshStateAllocated
        && expected.candidate.id.is_none()
        && next.candidate.id.is_some();
    let allocation_observed_during_rollback = expected.phase == Phase::FreshStateAllocating
        && next.phase == Phase::RollbackDecided
        && expected.candidate.id.is_none()
        && next.candidate.id.is_some()
        && next
            .rollback
            .as_ref()
            .is_some_and(|rollback| rollback.fresh_db == RollbackAction::Pending);
    if expected.candidate.id != next.candidate.id && !allocation_completed && !allocation_observed_during_rollback {
        return Err(CodecError::CandidateStateChangedIllegally);
    }
    Ok(())
}
