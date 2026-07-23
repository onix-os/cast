//! Focused persistence and one-entry production cleanup dispatch contracts.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{self, active_reblit_commit_cleanup},
        startup_reconciliation::{
            ActiveReblitCommitCleanupDurableAuthority,
            active_reblit_commit_cleanup_exchange_attempt_count,
            reset_active_reblit_commit_cleanup_exchange_attempt_count,
        },
        startup_recovery::{
            ActiveReblitCommitCleanupPersistenceError,
            ActiveReblitCommitCleanupValidationStage,
            DurableActiveReblitCommitCleanupRecord,
            arm_after_active_reblit_commit_cleanup_old_binding_validation,
            arm_after_active_reblit_commit_cleanup_same_store_check_before_reopen,
            arm_before_active_reblit_commit_cleanup_final_revalidation,
            arm_before_active_reblit_commit_cleanup_fresh_binding_validation,
            arm_before_active_reblit_commit_cleanup_reopened_validation,
            arm_before_active_reblit_commit_cleanup_same_store_validation,
            persist_active_reblit_commit_cleanup_complete_and_reopen,
        },
    },
    transition_journal::{
        Phase, TransitionJournalStore, TransitionRecord,
        arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed,
        assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    boot_sync_complete_support::{
        exact_promoted_receipt_state, open_boot_sync_complete_journal,
        same_byte_different_inode_hook,
    },
    commit_cleanup_effect::{
        CleanupLayout, capture_apply_pending, capture_finish_pending,
        commit_decided_fixture, no_boot_commit_decided_fixture,
    },
    support::{Epoch, assert_pending_phase, enter_boot},
};

#[test]
fn startup_cleanup_current_and_historical_apply_and_finish_persist_once() {
    for epoch in Epoch::ALL {
        for layout in [CleanupLayout::Apply, CleanupLayout::Finish] {
            for no_boot in [false, true] {
                let fixture = if no_boot {
                    no_boot_commit_decided_fixture(epoch, layout, true)
                } else {
                    commit_decided_fixture(epoch, layout)
                };
                let source = fixture.fixture.source.clone();
                let successor = exact_cleanup_complete(&source);
                let database_before = fixture.fixture.database_snapshot();
                let receipt_before = fixture
                    .fixture
                    .database
                    .load_current_exact_promoted_boot_publication_receipt_chain()
                    .unwrap();
                let root_abi_before = root_abi_snapshot(&fixture.fixture.installation.root);
                reset_active_reblit_commit_cleanup_exchange_attempt_count();

                let first = enter_boot(&fixture);

                assert_pending_phase(&first, Phase::CommitCleanupComplete);
                assert_eq!(fixture.fixture.canonical_record(), successor);
                assert_eq!(successor.generation, source.generation + 1);
                assert_eq!(successor.rollback, None);
                if no_boot {
                    assert_eq!(source.generation, 11);
                    assert_eq!(successor.generation, 12);
                    assert_eq!(successor.boot_publication_receipts, None);
                }
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_eq!(
                    fixture
                        .fixture
                        .database
                        .load_current_exact_promoted_boot_publication_receipt_chain()
                        .unwrap(),
                    receipt_before
                );
                assert_eq!(root_abi_snapshot(&fixture.fixture.installation.root), root_abi_before);
                let expected_attempts = usize::from(matches!(layout, CleanupLayout::Apply));
                assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), expected_attempts);

                let second = dispatch_cleanup_once(&fixture);

                assert_eq!(second, successor);
                assert_eq!(fixture.fixture.canonical_record(), successor);
                assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), expected_attempts);
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_eq!(
                    fixture
                        .fixture
                        .database
                        .load_current_exact_promoted_boot_publication_receipt_chain()
                        .unwrap(),
                    receipt_before
                );
                assert_eq!(root_abi_snapshot(&fixture.fixture.installation.root), root_abi_before);
            }
        }
    }
}

#[derive(Clone, Copy)]
struct JournalFault {
    arm: fn(),
    consumed: fn(),
    durable: DurableActiveReblitCommitCleanupRecord,
}

const JOURNAL_FAULTS: [JournalFault; 5] = [
    JournalFault {
        arm: arm_next_temporary_sync_fault,
        consumed: assert_temporary_sync_fault_consumed,
        durable: DurableActiveReblitCommitCleanupRecord::CommitDecided,
    },
    JournalFault {
        arm: arm_next_update_exchange_fault,
        consumed: assert_update_exchange_fault_consumed,
        durable: DurableActiveReblitCommitCleanupRecord::CommitDecided,
    },
    JournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        consumed: assert_update_first_directory_sync_fault_consumed,
        durable: DurableActiveReblitCommitCleanupRecord::CommitCleanupComplete,
    },
    JournalFault {
        arm: arm_next_displaced_unlink_fault,
        consumed: assert_displaced_unlink_fault_consumed,
        durable: DurableActiveReblitCommitCleanupRecord::CommitCleanupComplete,
    },
    JournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        consumed: assert_update_final_directory_sync_fault_consumed,
        durable: DurableActiveReblitCommitCleanupRecord::CommitCleanupComplete,
    },
];

#[test]
fn startup_cleanup_all_five_journal_faults_classify_and_converge_without_second_exchange() {
    for fault in JOURNAL_FAULTS {
        let fixture = commit_decided_fixture(Epoch::Current, CleanupLayout::Apply);
        let source = fixture.fixture.source.clone();
        let successor = exact_cleanup_complete(&source);
        let database_before = fixture.fixture.database_snapshot();
        let receipt_before = exact_promoted_receipt_state(&fixture);
        reset_active_reblit_commit_cleanup_exchange_attempt_count();
        (fault.arm)();

        let first = enter_boot(&fixture);

        (fault.consumed)();
        assert_advance_failure(&first, fault.durable);
        assert_eq!(
            fixture.fixture.canonical_record(),
            match fault.durable {
                DurableActiveReblitCommitCleanupRecord::CommitDecided => source,
                DurableActiveReblitCommitCleanupRecord::CommitCleanupComplete => successor.clone(),
            }
        );
        assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);

        let second = dispatch_cleanup_once(&fixture);

        assert_eq!(second, successor);
        assert_eq!(fixture.fixture.canonical_record(), successor);
        assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(exact_promoted_receipt_state(&fixture), receipt_before);
    }
}

#[derive(Clone, Copy, Debug)]
enum BindingHook {
    FinalAuthority,
    SameStore,
    BeforeReopen,
    ReopenedOldBinding,
    OldBindingBeforeFreshCapture,
    ReopenedFreshBinding,
}

impl BindingHook {
    const ALL: [Self; 6] = [
        Self::FinalAuthority,
        Self::SameStore,
        Self::BeforeReopen,
        Self::ReopenedOldBinding,
        Self::OldBindingBeforeFreshCapture,
        Self::ReopenedFreshBinding,
    ];

    fn expected_stage(self) -> Option<ActiveReblitCommitCleanupValidationStage> {
        match self {
            Self::FinalAuthority => None,
            Self::SameStore => Some(ActiveReblitCommitCleanupValidationStage::SameStore),
            Self::BeforeReopen | Self::ReopenedOldBinding => {
                Some(ActiveReblitCommitCleanupValidationStage::ReopenedOldBinding)
            }
            Self::OldBindingBeforeFreshCapture => Some(
                ActiveReblitCommitCleanupValidationStage::ReopenedOldBindingAfterFreshCapture,
            ),
            Self::ReopenedFreshBinding => {
                Some(ActiveReblitCommitCleanupValidationStage::ReopenedFreshBinding)
            }
        }
    }
}

#[test]
fn cleanup_persistence_all_binding_windows_reject_same_bytes_on_a_new_inode() {
    for hook in BindingHook::ALL {
        let fixture = commit_decided_fixture(Epoch::Current, CleanupLayout::Apply);
        let source = fixture.fixture.source.clone();
        let successor = exact_cleanup_complete(&source);
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_active_reblit_commit_cleanup_exchange_attempt_count();
        let durable = durable_cleanup(&fixture, &journal, &reservation, CleanupLayout::Apply);
        let replacement = same_byte_different_inode_hook(
            &fixture,
            &format!("cleanup-persistence-{hook:?}"),
        );
        match hook {
            BindingHook::FinalAuthority => {
                arm_before_active_reblit_commit_cleanup_final_revalidation(replacement)
            }
            BindingHook::SameStore => {
                arm_before_active_reblit_commit_cleanup_same_store_validation(replacement)
            }
            BindingHook::BeforeReopen => {
                arm_after_active_reblit_commit_cleanup_same_store_check_before_reopen(replacement)
            }
            BindingHook::ReopenedOldBinding => {
                arm_before_active_reblit_commit_cleanup_reopened_validation(replacement)
            }
            BindingHook::OldBindingBeforeFreshCapture => {
                arm_after_active_reblit_commit_cleanup_old_binding_validation(replacement)
            }
            BindingHook::ReopenedFreshBinding => {
                arm_before_active_reblit_commit_cleanup_fresh_binding_validation(replacement)
            }
        }

        let error = persist_active_reblit_commit_cleanup_complete_and_reopen(journal, durable)
            .expect_err("same-byte journal inode substitution returned success authority");

        match hook.expected_stage() {
            None => assert!(matches!(error, ActiveReblitCommitCleanupPersistenceError::Authority(_))),
            Some(stage) => assert!(matches!(
                error,
                ActiveReblitCommitCleanupPersistenceError::PostAdvanceValidation {
                    durable: DurableActiveReblitCommitCleanupRecord::CommitCleanupComplete,
                    stage: actual,
                    ..
                } if actual == stage
            )),
        }
        assert_eq!(
            fixture.fixture.canonical_record(),
            if matches!(hook, BindingHook::FinalAuthority) { source } else { successor }
        );
        assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
    }
}

#[test]
fn cleanup_persistence_rejects_database_and_namespace_changes_in_new_windows() {
    let fixture = commit_decided_fixture(Epoch::Current, CleanupLayout::Apply);
    let successor = exact_cleanup_complete(&fixture.fixture.source);
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let durable = durable_cleanup(&fixture, &journal, &reservation, CleanupLayout::Apply);
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    arm_before_active_reblit_commit_cleanup_same_store_validation(move || {
        database
            .change_summary_for_test(candidate, Some("cleanup persistence database race"))
            .unwrap();
    });
    let error = persist_active_reblit_commit_cleanup_complete_and_reopen(journal, durable)
        .expect_err("post-advance database change returned success authority");
    assert!(matches!(
        error,
        ActiveReblitCommitCleanupPersistenceError::PostAdvanceValidation {
            durable: DurableActiveReblitCommitCleanupRecord::CommitCleanupComplete,
            stage: ActiveReblitCommitCleanupValidationStage::SameStore,
            ..
        }
    ));
    assert_eq!(fixture.fixture.canonical_record(), successor);
    drop(reservation);

    let fixture = commit_decided_fixture(Epoch::Current, CleanupLayout::Finish);
    let successor = exact_cleanup_complete(&fixture.fixture.source);
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let durable = durable_cleanup(&fixture, &journal, &reservation, CleanupLayout::Finish);
    let staging = fixture.fixture.installation.root.join(".cast/root/staging");
    let mode = fs::metadata(&staging).unwrap().permissions().mode() & 0o7777;
    let changed_mode = if mode == 0o700 { 0o755 } else { 0o700 };
    arm_before_active_reblit_commit_cleanup_fresh_binding_validation(move || {
        fs::set_permissions(staging, fs::Permissions::from_mode(changed_mode)).unwrap();
    });
    let error = persist_active_reblit_commit_cleanup_complete_and_reopen(journal, durable)
        .expect_err("post-reopen namespace change returned success authority");
    assert!(matches!(
        error,
        ActiveReblitCommitCleanupPersistenceError::PostAdvanceValidation {
            durable: DurableActiveReblitCommitCleanupRecord::CommitCleanupComplete,
            stage: ActiveReblitCommitCleanupValidationStage::ReopenedFreshBinding,
            ..
        }
    ));
    assert_eq!(fixture.fixture.canonical_record(), successor);
}

fn durable_cleanup<'reservation>(
    fixture: &super::support::BootRepairFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    layout: CleanupLayout,
) -> ActiveReblitCommitCleanupDurableAuthority<'reservation> {
    let pending = match layout {
        CleanupLayout::Apply => capture_apply_pending(fixture, journal, reservation),
        CleanupLayout::Finish => capture_finish_pending(fixture, journal, reservation),
    };
    pending.complete(journal).unwrap()
}

fn dispatch_cleanup_once(
    fixture: &super::support::BootRepairFixture,
) -> TransitionRecord {
    let journal = open_boot_sync_complete_journal(fixture);
    let record = journal
        .load()
        .unwrap()
        .expect("cleanup retry requires a retained record");
    let reservation = ActiveStateReservation::acquire().unwrap();
    match active_reblit_commit_cleanup::dispatch(
        &fixture.fixture.installation,
        &fixture.fixture.database,
        &reservation,
        journal,
        record,
    )
    .unwrap()
    {
        active_reblit_commit_cleanup::Dispatch::Handled { journal, record }
        | active_reblit_commit_cleanup::Dispatch::Unhandled { journal, record } => {
            drop(journal);
            record
        }
    }
}

fn exact_cleanup_complete(source: &TransitionRecord) -> TransitionRecord {
    let successor = source.forward_successor(None).unwrap();
    assert_eq!(successor.phase, Phase::CommitCleanupComplete);
    successor
}

fn assert_advance_failure(
    error: &startup_gate::Error,
    expected: DurableActiveReblitCommitCleanupRecord,
) {
    assert!(matches!(
        error,
        startup_gate::Error::ActiveReblitCommitCleanupDispatch(
            active_reblit_commit_cleanup::Error::Persistence(
                ActiveReblitCommitCleanupPersistenceError::Advance { durable, .. }
            )
        ) if *durable == expected
    ));
}

fn root_abi_snapshot(root: &std::path::Path) -> Vec<(String, Option<std::path::PathBuf>)> {
    ["etc", "var", "home", "root", "srv"]
        .into_iter()
        .map(|name| {
            let path = root.join(name);
            let target = fs::read_link(&path).ok();
            (name.to_owned(), target)
        })
        .collect()
}
