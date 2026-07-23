use std::{ffi::OsString, fs};

use crate::{
    client::{
        startup_gate,
        startup_recovery::{
            DurableUsrRollbackReverseRecord, UsrRollbackReverseDispatchError, UsrRollbackReversePersistenceError,
        },
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{
        RollbackActionOutcome, arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{
    Fixture, OperationKind, ReverseLayout, assert_candidate_preserve_intent_pending, assert_layout_reversed,
    assert_layout_unchanged, assert_usr_restored_pending, enter, expected_candidate_preserve_intent,
    expected_usr_restored, namespace_snapshot, usr_layout,
};

#[derive(Clone, Copy)]
struct JournalFault {
    arm: fn(),
    assert_consumed: fn(),
    durable: DurableUsrRollbackReverseRecord,
}

const JOURNAL_FAULTS: [JournalFault; 5] = [
    JournalFault {
        arm: arm_next_temporary_sync_fault,
        assert_consumed: assert_temporary_sync_fault_consumed,
        durable: DurableUsrRollbackReverseRecord::Source,
    },
    JournalFault {
        arm: arm_next_update_exchange_fault,
        assert_consumed: assert_update_exchange_fault_consumed,
        durable: DurableUsrRollbackReverseRecord::Source,
    },
    JournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        assert_consumed: assert_update_first_directory_sync_fault_consumed,
        durable: DurableUsrRollbackReverseRecord::UsrRestored,
    },
    JournalFault {
        arm: arm_next_displaced_unlink_fault,
        assert_consumed: assert_displaced_unlink_fault_consumed,
        durable: DurableUsrRollbackReverseRecord::UsrRestored,
    },
    JournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        assert_consumed: assert_update_final_directory_sync_fault_consumed,
        durable: DurableUsrRollbackReverseRecord::UsrRestored,
    },
];

#[test]
fn startup_usr_rollback_reverse_dispatch_journal_faults_restart_to_exact_source_or_usr_restored() {
    for kind in OperationKind::ALL {
        for layout in [ReverseLayout::Post, ReverseLayout::Pre] {
            for fault in JOURNAL_FAULTS {
                let fixture = Fixture::for_effect(kind, layout);
                let source = fixture.record.clone();
                let initial_outcome = match layout {
                    ReverseLayout::Post => RollbackActionOutcome::Applied,
                    ReverseLayout::Pre => RollbackActionOutcome::AlreadySatisfied,
                };
                let initial_successor = expected_usr_restored(&fixture, initial_outcome);
                let database_before = fixture.fixture.database_snapshot();
                let namespace_before = namespace_snapshot(&fixture);
                let layout_before = usr_layout(&fixture);
                let root_abi_before = fixture.root_abi_snapshot();
                reset_retained_exchange_syscall_count();
                (fault.arm)();

                let first_error = enter(&fixture);

                (fault.assert_consumed)();
                assert_persistence_fault(&first_error, fault.durable);
                let expected_exchange_count = usize::from(layout == ReverseLayout::Post);
                assert_eq!(
                    retained_exchange_syscall_count(),
                    expected_exchange_count,
                    "{kind:?} {layout:?} {:?}",
                    fault.durable
                );
                assert_eq!(
                    fixture.fixture.database_snapshot(),
                    database_before,
                    "{kind:?} {layout:?} {:?}",
                    fault.durable
                );
                match fault.durable {
                    DurableUsrRollbackReverseRecord::Source => {
                        assert_eq!(fixture.fixture.canonical_record(), source, "{kind:?} {layout:?}")
                    }
                    DurableUsrRollbackReverseRecord::UsrRestored => assert_eq!(
                        fixture.fixture.canonical_record(),
                        initial_successor,
                        "{kind:?} {layout:?}"
                    ),
                }
                match layout {
                    ReverseLayout::Post => assert_layout_reversed(layout_before, usr_layout(&fixture)),
                    ReverseLayout::Pre => assert_layout_unchanged(layout_before, usr_layout(&fixture)),
                }
                fixture.assert_root_abi_unchanged(&root_abi_before);
                assert_clean_journal_directory(&fixture);
                let namespace_after_fault = namespace_snapshot(&fixture);
                let layout_after_fault = usr_layout(&fixture);
                if layout == ReverseLayout::Pre {
                    assert_eq!(namespace_after_fault, namespace_before, "{kind:?} {:?}", fault.durable);
                }

                // The failed entry owns no reusable reservation, diagnostic,
                // journal store, or effect authority across this boundary.
                drop(first_error);

                let restart = enter(&fixture);
                let expected_after_restart = match fault.durable {
                    DurableUsrRollbackReverseRecord::Source => {
                        expected_usr_restored(&fixture, RollbackActionOutcome::AlreadySatisfied)
                    }
                    DurableUsrRollbackReverseRecord::UsrRestored => {
                        expected_candidate_preserve_intent(&initial_successor)
                    }
                };

                match fault.durable {
                    DurableUsrRollbackReverseRecord::Source => assert_usr_restored_pending(&restart),
                    DurableUsrRollbackReverseRecord::UsrRestored => assert_candidate_preserve_intent_pending(&restart),
                }
                assert_eq!(
                    fixture.fixture.canonical_record(),
                    expected_after_restart,
                    "{kind:?} {layout:?} {:?}",
                    fault.durable
                );
                assert_eq!(
                    retained_exchange_syscall_count(),
                    expected_exchange_count,
                    "restart exchanged twice for {kind:?} {layout:?} {:?}",
                    fault.durable
                );
                assert_eq!(
                    fixture.fixture.database_snapshot(),
                    database_before,
                    "restart changed database for {kind:?} {layout:?} {:?}",
                    fault.durable
                );
                assert_eq!(
                    namespace_snapshot(&fixture),
                    namespace_after_fault,
                    "restart changed non-journal namespace for {kind:?} {layout:?} {:?}",
                    fault.durable
                );
                assert_layout_unchanged(layout_after_fault, usr_layout(&fixture));
                fixture.assert_root_abi_unchanged(&root_abi_before);
                assert_clean_journal_directory(&fixture);
                drop(restart);
            }
        }
    }
}

fn assert_persistence_fault(error: &startup_gate::Error, expected: DurableUsrRollbackReverseRecord) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackReverseDispatch(
                UsrRollbackReverseDispatchError::Persistence(
                    UsrRollbackReversePersistenceError::Advance { durable, .. }
                )
            ) if *durable == expected
        ),
        "expected typed persistence fault classified as {expected:?}, got {error:?}"
    );
}

fn assert_clean_journal_directory(fixture: &Fixture) {
    let mut names = fs::read_dir(fixture.fixture.installation.root.join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        [
            OsString::from("state-transition"),
            OsString::from("state-transition.lock")
        ],
        "stale journal update residue survived exact reopen"
    );
}
