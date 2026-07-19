use super::*;

#[test]
fn recovery_classifier_rolls_back_every_reversible_forward_phase() {
    let phases = [
        Phase::Preparing,
        Phase::FreshStateAllocating,
        Phase::FreshStateAllocated,
        Phase::CandidatePrepareStarted,
        Phase::CandidatePrepared,
        Phase::TransactionTriggersStarted,
        Phase::TransactionTriggersComplete,
        Phase::UsrExchangeIntent,
        Phase::UsrExchanged,
        Phase::RootLinksComplete,
        Phase::SystemTriggersStarted,
        Phase::SystemTriggersComplete,
        Phase::PreviousArchiveIntent,
        Phase::PreviousArchived,
        Phase::BootSyncStarted,
    ];

    for phase in phases {
        assert_eq!(
            phase.recovery_disposition(),
            RecoveryDisposition::BeginRollback {
                source: phase.forward().unwrap()
            }
        );
    }
}

#[test]
fn recovery_classifier_rolls_forward_after_verified_boot_sync_or_commit() {
    for phase in [
        Phase::BootSyncComplete,
        Phase::CommitDecided,
        Phase::CommitCleanupComplete,
        Phase::Complete,
    ] {
        assert_eq!(
            phase.recovery_disposition(),
            RecoveryDisposition::RollForward {
                phase: phase.forward().unwrap()
            }
        );
    }
}

#[test]
fn recovery_classifier_resumes_or_finalizes_persisted_rollback_without_guessing() {
    for phase in [
        Phase::RollbackDecided,
        Phase::PreviousRestoreIntent,
        Phase::PreviousRestoredToStaging,
        Phase::ReverseExchangeIntent,
        Phase::UsrRestored,
        Phase::CandidatePreserveIntent,
        Phase::CandidatePreserved,
        Phase::FreshDbInvalidationIntent,
        Phase::FreshDbInvalidated,
        Phase::BootRepairRequired,
        Phase::BootRepairStarted,
        Phase::BootRepairComplete,
    ] {
        assert_eq!(
            phase.recovery_disposition(),
            RecoveryDisposition::ResumeRollback { phase }
        );
    }
    assert_eq!(
        Phase::BootRepairUnverified.recovery_disposition(),
        RecoveryDisposition::ManualBootRepair
    );
    assert_eq!(
        Phase::RollbackComplete.recovery_disposition(),
        RecoveryDisposition::FinalizeRollback
    );
}
