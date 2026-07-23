//! Focused startup persistence contracts for promoted `BootSyncStarted`.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            ActiveReblitBootSyncStartedCompletionPersistenceError,
            ActiveReblitBootSyncStartedCompletionValidationStage,
            DurableActiveReblitBootSyncStartedCompletionRecord,
            arm_after_active_reblit_boot_sync_started_completion_old_binding_validation,
            arm_after_active_reblit_boot_sync_started_completion_same_store_check_before_reopen,
            arm_before_active_reblit_boot_sync_started_completion_final_revalidation,
            arm_before_active_reblit_boot_sync_started_completion_fresh_binding_validation,
            arm_before_active_reblit_boot_sync_started_completion_reopened_validation,
            arm_before_active_reblit_boot_sync_started_completion_same_store_validation,
            persist_active_reblit_boot_sync_started_completion_and_reopen,
        },
    },
    transition_journal::{
        TransitionRecord, arm_next_displaced_unlink_fault,
        arm_next_temporary_sync_fault, arm_next_update_exchange_fault,
        arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault,
        assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed,
        assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed,
        assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    boot_sync_complete_support::{
        BootSyncCompleteReadOnlySnapshot, boot_sync_started_fixture,
        capture_boot_sync_started_ready, open_boot_sync_complete_journal,
        same_byte_different_inode_hook,
    },
    support::{
        Epoch, assert_complete_route_journal_only,
        reset_complete_route_effect_observers,
    },
};

#[derive(Clone, Copy)]
struct JournalFault {
    arm: fn(),
    assert_consumed: fn(),
    durable: DurableActiveReblitBootSyncStartedCompletionRecord,
}

const JOURNAL_FAULTS: [JournalFault; 5] = [
    JournalFault {
        arm: arm_next_temporary_sync_fault,
        assert_consumed: assert_temporary_sync_fault_consumed,
        durable: DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncStarted,
    },
    JournalFault {
        arm: arm_next_update_exchange_fault,
        assert_consumed: assert_update_exchange_fault_consumed,
        durable: DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncStarted,
    },
    JournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        assert_consumed: assert_update_first_directory_sync_fault_consumed,
        durable: DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncComplete,
    },
    JournalFault {
        arm: arm_next_displaced_unlink_fault,
        assert_consumed: assert_displaced_unlink_fault_consumed,
        durable: DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncComplete,
    },
    JournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        assert_consumed: assert_update_final_directory_sync_fault_consumed,
        durable: DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncComplete,
    },
];

#[derive(Clone, Copy, Debug)]
enum ValidationHook {
    FinalAuthority,
    SameStore,
    BeforeReopen,
    ReopenedOldBinding,
    OldBindingBeforeFreshCapture,
    ReopenedFreshBinding,
}

impl ValidationHook {
    const ALL: [Self; 6] = [
        Self::FinalAuthority,
        Self::SameStore,
        Self::BeforeReopen,
        Self::ReopenedOldBinding,
        Self::OldBindingBeforeFreshCapture,
        Self::ReopenedFreshBinding,
    ];

    fn expected_stage(
        self,
    ) -> Option<ActiveReblitBootSyncStartedCompletionValidationStage> {
        match self {
            Self::FinalAuthority => None,
            Self::SameStore => Some(
                ActiveReblitBootSyncStartedCompletionValidationStage::SameStore,
            ),
            Self::BeforeReopen | Self::ReopenedOldBinding => Some(
                ActiveReblitBootSyncStartedCompletionValidationStage::ReopenedOldBinding,
            ),
            Self::OldBindingBeforeFreshCapture => Some(
                ActiveReblitBootSyncStartedCompletionValidationStage::ReopenedOldBindingAfterFreshCapture,
            ),
            Self::ReopenedFreshBinding => Some(
                ActiveReblitBootSyncStartedCompletionValidationStage::ReopenedFreshBinding,
            ),
        }
    }
}

#[test]
fn promoted_boot_sync_started_persists_exact_completion_and_returns_reopened_store() {
    for epoch in Epoch::ALL {
        let fixture = boot_sync_started_fixture(epoch, true);
        let successor = exact_boot_sync_complete(&fixture.fixture.source);
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority =
            capture_boot_sync_started_ready(&fixture, &journal, &reservation);
        let read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
        reset_complete_route_effect_observers();

        let (reopened, record) =
            persist_active_reblit_boot_sync_started_completion_and_reopen(
                journal, authority,
            )
            .unwrap();

        assert_eq!(record, successor);
        assert_eq!(reopened.load().unwrap(), Some(successor.clone()));
        assert_eq!(fixture.fixture.canonical_record(), successor);
        read_only.assert_unchanged(&fixture);
        assert_complete_route_journal_only();
    }
}

#[test]
fn promoted_boot_sync_started_all_five_update_faults_reconcile_source_or_successor() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for fault in JOURNAL_FAULTS {
            let fixture = boot_sync_started_fixture(epoch, true);
            let source = fixture.fixture.source.clone();
            let successor = exact_boot_sync_complete(&source);
            let journal = open_boot_sync_complete_journal(&fixture);
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = capture_boot_sync_started_ready(
                &fixture,
                &journal,
                &reservation,
            );
            let read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
            reset_complete_route_effect_observers();
            (fault.arm)();

            let error = match
                persist_active_reblit_boot_sync_started_completion_and_reopen(
                    journal, authority,
                )
            {
                Ok(_) => panic!(
                    "faulted BootSyncStarted completion returned a journal store"
                ),
                Err(error) => error,
            };

            (fault.assert_consumed)();
            assert!(
                matches!(
                    error,
                    ActiveReblitBootSyncStartedCompletionPersistenceError::Advance {
                        durable,
                        ..
                    } if durable == fault.durable
                ),
                "unexpected durable record for {epoch:?}: {error:?}"
            );
            assert_eq!(
                fixture.fixture.canonical_record(),
                match fault.durable {
                    DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncStarted => {
                        source
                    }
                    DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncComplete => {
                        successor
                    }
                }
            );
            read_only.assert_unchanged(&fixture);
            assert_complete_route_journal_only();
            cases += 1;
        }
    }
    assert_eq!(cases, 10);
}

#[test]
fn promoted_boot_sync_started_all_six_binding_checks_reject_same_bytes_on_new_inode() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for hook in ValidationHook::ALL {
            let fixture = boot_sync_started_fixture(epoch, true);
            let source = fixture.fixture.source.clone();
            let successor = exact_boot_sync_complete(&source);
            let journal = open_boot_sync_complete_journal(&fixture);
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = capture_boot_sync_started_ready(
                &fixture,
                &journal,
                &reservation,
            );
            let read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
            reset_complete_route_effect_observers();
            let replacement = same_byte_different_inode_hook(
                &fixture,
                &format!("boot-sync-started-{epoch:?}-{hook:?}"),
            );
            match hook {
                ValidationHook::FinalAuthority => {
                    arm_before_active_reblit_boot_sync_started_completion_final_revalidation(
                        replacement,
                    )
                }
                ValidationHook::SameStore => {
                    arm_before_active_reblit_boot_sync_started_completion_same_store_validation(
                        replacement,
                    )
                }
                ValidationHook::BeforeReopen => {
                    arm_after_active_reblit_boot_sync_started_completion_same_store_check_before_reopen(
                        replacement,
                    )
                }
                ValidationHook::ReopenedOldBinding => {
                    arm_before_active_reblit_boot_sync_started_completion_reopened_validation(
                        replacement,
                    )
                }
                ValidationHook::OldBindingBeforeFreshCapture => {
                    arm_after_active_reblit_boot_sync_started_completion_old_binding_validation(
                        replacement,
                    )
                }
                ValidationHook::ReopenedFreshBinding => {
                    arm_before_active_reblit_boot_sync_started_completion_fresh_binding_validation(
                        replacement,
                    )
                }
            }

            let error = match
                persist_active_reblit_boot_sync_started_completion_and_reopen(
                    journal, authority,
                )
            {
                Ok(_) => panic!(
                    "same-byte journal inode substitution returned a store at {hook:?}"
                ),
                Err(error) => error,
            };

            match hook.expected_stage() {
                None => assert!(
                    matches!(
                        error,
                        ActiveReblitBootSyncStartedCompletionPersistenceError::Authority(_)
                    ),
                    "unexpected {hook:?} failure: {error:?}"
                ),
                Some(expected_stage) => assert!(
                    matches!(
                        error,
                        ActiveReblitBootSyncStartedCompletionPersistenceError::PostAdvanceValidation {
                            durable: DurableActiveReblitBootSyncStartedCompletionRecord::BootSyncComplete,
                            stage,
                            ..
                        } if stage == expected_stage
                    ),
                    "unexpected {hook:?} failure: {error:?}"
                ),
            }
            assert_eq!(
                fixture.fixture.canonical_record(),
                if matches!(hook, ValidationHook::FinalAuthority) {
                    source
                } else {
                    successor
                }
            );
            read_only.assert_unchanged(&fixture);
            assert_complete_route_journal_only();
            cases += 1;
        }
    }
    assert_eq!(cases, 12);
}

fn exact_boot_sync_complete(source: &TransitionRecord) -> TransitionRecord {
    let pair = source
        .boot_publication_receipt_correlation()
        .unwrap()
        .expect("promoted BootSyncStarted fixture carries an exact receipt pair");
    source.boot_sync_complete_successor(pair).unwrap()
}
