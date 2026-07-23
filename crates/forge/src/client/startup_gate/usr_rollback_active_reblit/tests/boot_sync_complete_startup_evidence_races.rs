//! Authority and post-advance evidence races for forward boot completion.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_gate::{self, active_reblit_boot_sync_complete},
    startup_reconciliation::{
        arm_before_active_reblit_boot_sync_complete_fresh_namespace_capture,
        arm_between_active_reblit_boot_sync_complete_database_captures,
    },
    startup_recovery::{
        ActiveReblitBootSyncCommitDecisionPersistenceError,
        ActiveReblitBootSyncCommitDecisionValidationStage,
        DurableActiveReblitBootSyncCommitDecisionRecord,
        arm_after_active_reblit_boot_sync_commit_decision_old_binding_validation,
        arm_after_active_reblit_boot_sync_commit_decision_same_store_check_before_reopen,
        arm_before_active_reblit_boot_sync_commit_decision_final_revalidation,
        arm_before_active_reblit_boot_sync_commit_decision_fresh_binding_validation,
        arm_before_active_reblit_boot_sync_commit_decision_reopened_validation,
        arm_before_active_reblit_boot_sync_commit_decision_same_store_validation,
        persist_active_reblit_boot_sync_commit_decision_and_reopen,
    },
};

use super::{
    boot_sync_complete_support::{
        BootSyncCompleteReadOnlySnapshot, boot_sync_complete_fixture,
        capture_boot_sync_complete_ready, exact_commit_decided,
        open_boot_sync_complete_journal, same_byte_different_inode_hook,
    },
    support::{
        Epoch, assert_complete_route_journal_only, enter_boot,
        reset_complete_route_effect_observers,
    },
};

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

    fn expected_stage(self) -> Option<ActiveReblitBootSyncCommitDecisionValidationStage> {
        match self {
            Self::FinalAuthority => None,
            Self::SameStore => Some(ActiveReblitBootSyncCommitDecisionValidationStage::SameStore),
            Self::BeforeReopen | Self::ReopenedOldBinding => {
                Some(ActiveReblitBootSyncCommitDecisionValidationStage::ReopenedOldBinding)
            }
            Self::OldBindingBeforeFreshCapture => Some(
                ActiveReblitBootSyncCommitDecisionValidationStage::ReopenedOldBindingAfterFreshCapture,
            ),
            Self::ReopenedFreshBinding => {
                Some(ActiveReblitBootSyncCommitDecisionValidationStage::ReopenedFreshBinding)
            }
        }
    }
}

#[test]
fn boot_sync_commit_decision_all_six_validation_hooks_reject_same_bytes_on_a_new_inode() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for hook in ValidationHook::ALL {
            let fixture = boot_sync_complete_fixture(epoch, true);
            let source = fixture.fixture.source.clone();
            let successor = exact_commit_decided(&fixture);
            let journal = open_boot_sync_complete_journal(&fixture);
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = capture_boot_sync_complete_ready(&fixture, &journal, &reservation);
            let read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
            reset_complete_route_effect_observers();
            let replacement = same_byte_different_inode_hook(
                &fixture,
                &format!("boot-sync-{epoch:?}-{hook:?}"),
            );
            match hook {
                ValidationHook::FinalAuthority => {
                    arm_before_active_reblit_boot_sync_commit_decision_final_revalidation(replacement)
                }
                ValidationHook::SameStore => {
                    arm_before_active_reblit_boot_sync_commit_decision_same_store_validation(replacement)
                }
                ValidationHook::BeforeReopen => {
                    arm_after_active_reblit_boot_sync_commit_decision_same_store_check_before_reopen(
                        replacement,
                    )
                }
                ValidationHook::ReopenedOldBinding => {
                    arm_before_active_reblit_boot_sync_commit_decision_reopened_validation(replacement)
                }
                ValidationHook::OldBindingBeforeFreshCapture => {
                    arm_after_active_reblit_boot_sync_commit_decision_old_binding_validation(replacement)
                }
                ValidationHook::ReopenedFreshBinding => {
                    arm_before_active_reblit_boot_sync_commit_decision_fresh_binding_validation(replacement)
                }
            }

            let error = match persist_active_reblit_boot_sync_commit_decision_and_reopen(journal, authority) {
                Ok(_) => panic!("same-byte journal inode substitution returned a store at {hook:?}"),
                Err(error) => error,
            };

            match hook.expected_stage() {
                None => assert!(
                    matches!(error, ActiveReblitBootSyncCommitDecisionPersistenceError::Authority(_)),
                    "unexpected {hook:?} failure: {error:?}"
                ),
                Some(expected_stage) => assert!(
                    matches!(
                        error,
                        ActiveReblitBootSyncCommitDecisionPersistenceError::PostAdvanceValidation {
                            durable: DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided,
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

#[test]
fn boot_sync_commit_decision_same_store_rejects_full_state_change_after_advance() {
    for epoch in Epoch::ALL {
        let fixture = boot_sync_complete_fixture(epoch, true);
        let successor = exact_commit_decided(&fixture);
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = capture_boot_sync_complete_ready(&fixture, &journal, &reservation);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        let receipt_before = super::boot_sync_complete_support::exact_promoted_receipt_state(&fixture);
        let database = fixture.fixture.database.clone();
        let candidate = fixture.fixture.candidate_state;
        let changed_summary = format!("post-advance full-State race at {epoch:?}");
        let hook_summary = changed_summary.clone();
        reset_complete_route_effect_observers();
        arm_before_active_reblit_boot_sync_commit_decision_same_store_validation(move || {
            database
                .change_summary_for_test(candidate, Some(&hook_summary))
                .unwrap();
        });

        let error = match persist_active_reblit_boot_sync_commit_decision_and_reopen(journal, authority) {
            Ok(_) => panic!("post-advance full-State change returned a journal store at {epoch:?}"),
            Err(error) => error,
        };

        assert!(
            matches!(
                error,
                ActiveReblitBootSyncCommitDecisionPersistenceError::PostAdvanceValidation {
                    durable: DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided,
                    stage: ActiveReblitBootSyncCommitDecisionValidationStage::SameStore,
                    ..
                }
            ),
            "unexpected post-advance full-State failure at {epoch:?}: {error:?}"
        );
        assert_eq!(fixture.fixture.canonical_record(), successor);
        assert_ne!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(
            fixture.fixture.database.get(candidate).unwrap().summary.as_deref(),
            Some(changed_summary.as_str())
        );
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_eq!(
            super::boot_sync_complete_support::exact_promoted_receipt_state(&fixture),
            receipt_before
        );
        assert_complete_route_journal_only();
    }
}

#[test]
fn boot_sync_commit_decision_fresh_binding_rejects_namespace_change_after_reopen() {
    for epoch in Epoch::ALL {
        let fixture = boot_sync_complete_fixture(epoch, true);
        let successor = exact_commit_decided(&fixture);
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = capture_boot_sync_complete_ready(&fixture, &journal, &reservation);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        let receipt_before = super::boot_sync_complete_support::exact_promoted_receipt_state(&fixture);
        let changed = fixture
            .fixture
            .active_reblit_reservation
            .as_ref()
            .expect("ActiveReblit fixture retains the replacement wrapper")
            .clone();
        let original_mode = fs::metadata(&changed).unwrap().permissions().mode() & 0o7777;
        let changed_mode = if original_mode == 0o700 { 0o755 } else { 0o700 };
        let asserted_path = changed.clone();
        reset_complete_route_effect_observers();
        arm_before_active_reblit_boot_sync_commit_decision_fresh_binding_validation(move || {
            fs::set_permissions(changed, fs::Permissions::from_mode(changed_mode)).unwrap();
        });

        let error = match persist_active_reblit_boot_sync_commit_decision_and_reopen(journal, authority) {
            Ok(_) => panic!("post-reopen namespace change returned a journal store at {epoch:?}"),
            Err(error) => error,
        };

        assert!(
            matches!(
                error,
                ActiveReblitBootSyncCommitDecisionPersistenceError::PostAdvanceValidation {
                    durable: DurableActiveReblitBootSyncCommitDecisionRecord::CommitDecided,
                    stage: ActiveReblitBootSyncCommitDecisionValidationStage::ReopenedFreshBinding,
                    ..
                }
            ),
            "unexpected post-reopen namespace failure at {epoch:?}: {error:?}"
        );
        assert_eq!(fixture.fixture.canonical_record(), successor);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_ne!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_eq!(
            fs::metadata(asserted_path).unwrap().permissions().mode() & 0o7777,
            changed_mode
        );
        assert_eq!(
            super::boot_sync_complete_support::exact_promoted_receipt_state(&fixture),
            receipt_before
        );
        assert_complete_route_journal_only();
    }
}

#[test]
fn startup_boot_sync_complete_database_race_fails_stop_before_any_journal_advance() {
    let fixture = boot_sync_complete_fixture(Epoch::Current, true);
    let source = fixture.fixture.source.clone();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    reset_complete_route_effect_observers();
    arm_between_active_reblit_boot_sync_complete_database_captures(move || {
        database
            .change_summary_for_test(candidate, Some("changed during production startup admission"))
            .unwrap();
    });

    let error = enter_boot(&fixture);

    assert_capture_authority_error(&error);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_complete_route_journal_only();
}

#[test]
fn startup_boot_sync_complete_namespace_race_fails_stop_before_any_journal_advance() {
    let fixture = boot_sync_complete_fixture(Epoch::Current, true);
    let source = fixture.fixture.source.clone();
    let changed = fixture
        .fixture
        .active_reblit_reservation
        .as_ref()
        .expect("ActiveReblit fixture retains the replacement wrapper")
        .clone();
    let original_mode = fs::metadata(&changed).unwrap().permissions().mode() & 0o7777;
    let changed_mode = if original_mode == 0o700 { 0o755 } else { 0o700 };
    reset_complete_route_effect_observers();
    arm_before_active_reblit_boot_sync_complete_fresh_namespace_capture(move || {
        fs::set_permissions(changed, fs::Permissions::from_mode(changed_mode)).unwrap();
    });

    let error = enter_boot(&fixture);

    assert_persistence_authority_error(&error);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_complete_route_journal_only();
}

fn assert_capture_authority_error(error: &startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::ActiveReblitBootSyncCompleteDispatch(
                active_reblit_boot_sync_complete::Error::Authority(_)
            )
        ),
        "expected exact boot-completion startup authority failure, got {error:?}"
    );
}

fn assert_persistence_authority_error(error: &startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::ActiveReblitBootSyncCompleteDispatch(
                active_reblit_boot_sync_complete::Error::Persistence(
                    ActiveReblitBootSyncCommitDecisionPersistenceError::Authority(_)
                )
            )
        ),
        "expected exact pre-advance boot-completion persistence authority failure, got {error:?}"
    );
}
