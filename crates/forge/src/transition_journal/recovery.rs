use super::{ForwardPhase, Phase, TransitionRecord};

/// The only safe direction in which startup may resume a durable transition.
///
/// This is deliberately a policy classification, not a recovery executor.
/// A caller must still authenticate the exact database, tree, and runtime
/// evidence required by the selected action before it mutates anything.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // consumed by startup reconciliation in the next saved increment
pub(crate) enum RecoveryDisposition {
    /// Persist a rollback decision derived from this exact forward phase.
    BeginRollback { source: ForwardPhase },
    /// Continue the rollback plan already persisted in the journal.
    ResumeRollback { phase: Phase },
    /// Continue toward committed cleanup from this exact forward phase.
    RollForward { phase: ForwardPhase },
    /// The rollback effects are complete; only terminal journal cleanup remains.
    FinalizeRollback,
    /// Automated recovery must stop and retain the journal for an operator.
    ManualBootRepair,
}

impl Phase {
    fn recovery_disposition(self) -> RecoveryDisposition {
        match self {
            Self::Preparing
            | Self::FreshStateAllocating
            | Self::FreshStateAllocated
            | Self::CandidatePrepareStarted
            | Self::CandidatePrepared
            | Self::TransactionTriggersStarted
            | Self::TransactionTriggersComplete
            | Self::UsrExchangeIntent
            | Self::UsrExchanged
            | Self::RootLinksComplete
            | Self::SystemTriggersStarted
            | Self::SystemTriggersComplete
            | Self::PreviousArchiveIntent
            | Self::PreviousArchived
            | Self::BootSyncStarted => RecoveryDisposition::BeginRollback {
                source: self
                    .forward()
                    .expect("every pre-commit recovery phase is a forward phase"),
            },
            Self::BootSyncComplete | Self::CommitDecided | Self::CommitCleanupComplete | Self::Complete => {
                RecoveryDisposition::RollForward {
                    phase: self
                        .forward()
                        .expect("every committed recovery phase is a forward phase"),
                }
            }
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
            | Self::BootRepairStarted => RecoveryDisposition::ResumeRollback { phase: self },
            Self::BootRepairUnverified => RecoveryDisposition::ManualBootRepair,
            Self::RollbackComplete => RecoveryDisposition::FinalizeRollback,
        }
    }
}

impl TransitionRecord {
    /// Classify the already-validated durable record without consulting paths
    /// or reinterpreting runtime witnesses from another boot or namespace.
    #[allow(dead_code)] // consumed by startup reconciliation in the next saved increment
    pub(crate) fn recovery_disposition(&self) -> RecoveryDisposition {
        self.phase.recovery_disposition()
    }
}

#[cfg(test)]
mod tests;
