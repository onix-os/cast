use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use crate::{
    client::active_state_snapshot::ActiveStateReservation,
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
    Fixture, OperationKind, ReverseLayout, SourceCase, durable_authority, expected_usr_restored,
    non_journal_namespace_snapshot,
};
use super::super::{
    DurableUsrRollbackReverseRecord, UsrRollbackReversePersistenceError,
    UsrRollbackReverseSuccessorBindingError,
    arm_after_usr_rollback_reverse_successor_binding_check_before_reopen,
    arm_before_usr_rollback_reverse_successor_binding_revalidation,
    persist_usr_rollback_reverse_and_reopen,
};

type StorageFault = (fn(), fn(), DurableUsrRollbackReverseRecord);

const STORAGE_FAULTS: [StorageFault; 5] = [
    (
        arm_next_temporary_sync_fault,
        assert_temporary_sync_fault_consumed,
        DurableUsrRollbackReverseRecord::Source,
    ),
    (
        arm_next_update_exchange_fault,
        assert_update_exchange_fault_consumed,
        DurableUsrRollbackReverseRecord::Source,
    ),
    (
        arm_next_update_first_directory_sync_fault,
        assert_update_first_directory_sync_fault_consumed,
        DurableUsrRollbackReverseRecord::UsrRestored,
    ),
    (
        arm_next_displaced_unlink_fault,
        assert_displaced_unlink_fault_consumed,
        DurableUsrRollbackReverseRecord::UsrRestored,
    ),
    (
        arm_next_update_final_directory_sync_fault,
        assert_update_final_directory_sync_fault_consumed,
        DurableUsrRollbackReverseRecord::UsrRestored,
    ),
];

fn fixture(
    kind: OperationKind,
    outcome: RollbackActionOutcome,
    historical: bool,
) -> Fixture {
    Fixture::for_effect_source(
        kind,
        SourceCase::RootLinksCompletePost,
        match outcome {
            RollbackActionOutcome::Applied => ReverseLayout::Post,
            RollbackActionOutcome::AlreadySatisfied => ReverseLayout::Pre,
        },
        historical,
    )
}

fn canonical_journal(fixture: &Fixture) -> PathBuf {
    fixture
        .fixture
        .installation
        .root
        .join(".cast/journal/state-transition")
}

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn same_byte_successor_replacement_hook(
    fixture: &Fixture,
    displaced_name: String,
) -> (PathBuf, impl FnOnce() + 'static) {
    let canonical = canonical_journal(fixture);
    let displaced = fixture.fixture.installation.root.join(displaced_name);
    let hook_displaced = displaced.clone();
    let hook = move || {
        let bytes = fs::read(&canonical).unwrap();
        fs::rename(&canonical, &hook_displaced).unwrap();
        fs::write(&canonical, bytes).unwrap();
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
    };
    (displaced, hook)
}

fn assert_successor_replacement_failure(
    error: UsrRollbackReversePersistenceError,
    fixture: &Fixture,
    displaced: &Path,
) {
    assert!(matches!(
        error,
        UsrRollbackReversePersistenceError::SuccessorRecordBinding {
            durable: DurableUsrRollbackReverseRecord::UsrRestored,
            source: UsrRollbackReverseSuccessorBindingError::Changed,
        }
    ));
    assert_eq!(fs::read(displaced).unwrap(), fixture.fixture.canonical_bytes());
    assert_ne!(inode_identity(displaced), inode_identity(&canonical_journal(fixture)));
}

#[test]
fn startup_root_links_reverse_all_bound_update_faults_reopen_exact_record_across_operations_and_epochs() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for (arm, assert_consumed, expected_durable) in STORAGE_FAULTS {
                    let fixture = fixture(kind, outcome, historical);
                    let case = format!("{kind:?} {outcome:?} historical={historical}");
                    let root_abi_before = fixture.root_abi_snapshot();
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_retained_exchange_syscall_count();
                    let authority = durable_authority(&fixture, &journal, &reservation, outcome);
                    let namespace_before = non_journal_namespace_snapshot(&fixture);
                    let restored = expected_usr_restored(&fixture, outcome);
                    arm();

                    let error = persist_usr_rollback_reverse_and_reopen(journal, authority).unwrap_err();

                    assert_consumed();
                    assert!(matches!(
                        &error,
                        UsrRollbackReversePersistenceError::Advance { durable, .. }
                            if *durable == expected_durable
                    ), "{case}: {error:?}");
                    match expected_durable {
                        DurableUsrRollbackReverseRecord::Source => {
                            assert_eq!(fixture.fixture.canonical_record(), fixture.record, "{case}")
                        }
                        DurableUsrRollbackReverseRecord::UsrRestored => {
                            assert_eq!(fixture.fixture.canonical_record(), restored, "{case}")
                        }
                    }
                    assert_eq!(
                        retained_exchange_syscall_count(),
                        usize::from(outcome == RollbackActionOutcome::Applied),
                        "{case}"
                    );
                    assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before, "{case}");
                    fixture.assert_root_abi_unchanged(&root_abi_before);
                }
            }
        }
    }
}

#[test]
fn startup_root_links_reverse_same_byte_successor_replacement_after_publication_fails_exact_binding() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                let fixture = fixture(kind, outcome, historical);
                let case = format!("{kind:?} {outcome:?} historical={historical}");
                let root_abi_before = fixture.root_abi_snapshot();
                let database_before = fixture.fixture.database_snapshot();
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_retained_exchange_syscall_count();
                let authority = durable_authority(&fixture, &journal, &reservation, outcome);
                let restored = expected_usr_restored(&fixture, outcome);
                let (displaced, hook) = same_byte_successor_replacement_hook(
                    &fixture,
                    format!("root-links-reverse-successor-{kind:?}-{outcome:?}-{historical}"),
                );
                arm_before_usr_rollback_reverse_successor_binding_revalidation(hook);

                let error = persist_usr_rollback_reverse_and_reopen(journal, authority).unwrap_err();

                assert_successor_replacement_failure(error, &fixture, &displaced);
                assert_eq!(fixture.fixture.canonical_record(), restored, "{case}");
                assert_eq!(fixture.fixture.database_snapshot(), database_before, "{case}");
                assert_eq!(
                    retained_exchange_syscall_count(),
                    usize::from(outcome == RollbackActionOutcome::Applied),
                    "{case}"
                );
                fixture.assert_root_abi_unchanged(&root_abi_before);
            }
        }
    }
}

#[test]
fn startup_root_links_reverse_same_byte_successor_replacement_after_binding_before_reopen_never_succeeds() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                let fixture = fixture(kind, outcome, historical);
                let case = format!("{kind:?} {outcome:?} historical={historical}");
                let root_abi_before = fixture.root_abi_snapshot();
                let database_before = fixture.fixture.database_snapshot();
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_retained_exchange_syscall_count();
                let authority = durable_authority(&fixture, &journal, &reservation, outcome);
                let restored = expected_usr_restored(&fixture, outcome);
                let (displaced, hook) = same_byte_successor_replacement_hook(
                    &fixture,
                    format!("root-links-reverse-reopen-{kind:?}-{outcome:?}-{historical}"),
                );
                arm_after_usr_rollback_reverse_successor_binding_check_before_reopen(hook);

                let error = persist_usr_rollback_reverse_and_reopen(journal, authority).unwrap_err();

                assert_successor_replacement_failure(error, &fixture, &displaced);
                assert_eq!(fixture.fixture.canonical_record(), restored, "{case}");
                assert_eq!(fixture.fixture.database_snapshot(), database_before, "{case}");
                assert_eq!(
                    retained_exchange_syscall_count(),
                    usize::from(outcome == RollbackActionOutcome::Applied),
                    "{case}"
                );
                fixture.assert_root_abi_unchanged(&root_abi_before);
            }
        }
    }
}
