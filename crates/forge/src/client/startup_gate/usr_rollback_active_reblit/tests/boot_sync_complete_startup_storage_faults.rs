//! Every conditional journal-update fault through the production startup edge.

use crate::{
    client::{
        startup_gate::{self, active_reblit_boot_sync_complete},
        startup_recovery::{
            ActiveReblitBootSyncCommitDecisionPersistenceError,
            DurableActiveReblitBootSyncCommitDecisionRecord,
        },
    },
    transition_journal::{
        Phase, arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed,
        assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    boot_sync_complete_support::{
        BootSyncCompleteReadOnlySnapshot, boot_sync_complete_fixture, exact_commit_decided,
    },
    support::{
        Epoch, assert_complete_route_journal_only, assert_pending_phase, enter_boot,
        reset_complete_route_effect_observers,
    },
};

#[derive(Clone, Copy)]
struct JournalFault {
    arm: fn(),
    assert_consumed: fn(),
    durable: DurableActiveReblitBootSyncCommitDecisionRecord,
}

const JOURNAL_FAULTS: [JournalFault; 5] = [
    JournalFault {
        arm: arm_next_temporary_sync_fault,
        assert_consumed: assert_temporary_sync_fault_consumed,
        durable: DurableActiveReblitBootSyncCommitDecisionRecord::BootSyncComplete,
    },
    JournalFault {
        arm: arm_next_update_exchange_fault,
        assert_consumed: assert_update_exchange_fault_consumed,
        durable: DurableActiveReblitBootSyncCommitDecisionRecord::BootSyncComplete,
    },
    JournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        assert_consumed: assert_update_first_directory_sync_fault_consumed,
        durable: DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided,
    },
    JournalFault {
        arm: arm_next_displaced_unlink_fault,
        assert_consumed: assert_displaced_unlink_fault_consumed,
        durable: DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided,
    },
    JournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        assert_consumed: assert_update_final_directory_sync_fault_consumed,
        durable: DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided,
    },
];

#[test]
fn startup_boot_sync_complete_all_five_journal_faults_classify_and_converge_without_false_success() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for fault in JOURNAL_FAULTS {
            let fixture = boot_sync_complete_fixture(epoch, true);
            let source = fixture.fixture.source.clone();
            let successor = exact_commit_decided(&fixture);
            let cleanup_complete = successor.forward_successor(None).unwrap();
            assert_eq!(cleanup_complete.phase, Phase::CommitCleanupComplete);
            let first_read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
            reset_complete_route_effect_observers();
            (fault.arm)();

            let first = enter_boot(&fixture);

            (fault.assert_consumed)();
            assert_persistence_advance(&first, fault.durable);
            assert_eq!(
                fixture.fixture.canonical_record(),
                match fault.durable {
                    DurableActiveReblitBootSyncCommitDecisionRecord::BootSyncComplete => source,
                    DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided => successor.clone(),
                }
            );
            first_read_only.assert_unchanged(&fixture);
            assert_complete_route_journal_only();

            let second_read_only = match fault.durable {
                DurableActiveReblitBootSyncCommitDecisionRecord::BootSyncComplete => {
                    Some(BootSyncCompleteReadOnlySnapshot::capture(&fixture))
                }
                DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided => None,
            };
            let second = enter_boot(&fixture);

            let second_successor = match fault.durable {
                DurableActiveReblitBootSyncCommitDecisionRecord::BootSyncComplete => successor,
                DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided => cleanup_complete,
            };
            assert_pending_phase(&second, second_successor.phase);
            assert_eq!(fixture.fixture.canonical_record(), second_successor);
            if let Some(second_read_only) = second_read_only {
                second_read_only.assert_unchanged(&fixture);
            }
            assert_complete_route_journal_only();
            cases += 1;
        }
    }
    assert_eq!(cases, 10);
}

fn assert_persistence_advance(
    error: &startup_gate::Error,
    expected: DurableActiveReblitBootSyncCommitDecisionRecord,
) {
    assert!(
        matches!(
            error,
            startup_gate::Error::ActiveReblitBootSyncCompleteDispatch(
                active_reblit_boot_sync_complete::Error::Persistence(
                    ActiveReblitBootSyncCommitDecisionPersistenceError::Advance { durable, .. }
                )
            ) if *durable == expected
        ),
        "expected durable {expected:?} boot-sync commit-decision advance failure, got {error:?}"
    );
}
