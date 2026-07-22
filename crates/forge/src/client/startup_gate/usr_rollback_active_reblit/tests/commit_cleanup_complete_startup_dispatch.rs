//! Focused receipt retirement and `CommitCleanupComplete` dispatch contracts.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{
            self, ActiveReblitCommitCleanupCompleteSeal,
            active_reblit_commit_cleanup_complete,
        },
        startup_reconciliation::{
            ActiveReblitCommitCleanupCompleteAdmission,
            ActiveReblitCommitCleanupCompleteAuthority,
            ActiveReblitCommitCleanupCompleteRetiredAuthority,
            arm_between_active_reblit_commit_cleanup_complete_database_captures,
        },
        startup_recovery::{
            ActiveReblitCommitCleanupCompletePersistenceError,
            ActiveReblitCommitCleanupCompleteValidationStage,
            DurableActiveReblitCommitCleanupCompleteRecord,
            arm_after_active_reblit_commit_cleanup_complete_old_binding_validation,
            arm_after_active_reblit_commit_cleanup_complete_same_store_before_reopen,
            arm_before_active_reblit_commit_cleanup_complete_final_revalidation,
            arm_before_active_reblit_commit_cleanup_complete_fresh_binding_validation,
            arm_before_active_reblit_commit_cleanup_complete_reopened_validation,
            arm_before_active_reblit_commit_cleanup_complete_same_store_validation,
            persist_active_reblit_commit_cleanup_complete_to_complete_and_reopen,
        },
    },
    db,
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
    boot_sync_complete_support::{open_boot_sync_complete_journal, same_byte_different_inode_hook},
    commit_cleanup_effect::{CleanupLayout, commit_decided_fixture},
    support::{
        BootRepairFixture, Epoch, assert_canonical_absent, assert_pending_phase,
        enter_boot, enter_clean_boot,
    },
};

#[derive(Clone, Copy)]
enum ReceiptRoute {
    Apply,
    Finish,
}

#[test]
fn completed_cleanup_current_and_historical_apply_and_finish_reaches_complete_once() {
    for epoch in Epoch::ALL {
        for route in [ReceiptRoute::Apply, ReceiptRoute::Finish] {
            let mut fixture = commit_cleanup_complete_fixture(epoch);
            let source = fixture.fixture.source.clone();
            let successor = exact_complete(&source);
            let state_before = NonReceiptDatabaseSnapshot::capture(&fixture);
            if matches!(route, ReceiptRoute::Finish) {
                retire_receipt(&fixture);
            }

            let first = enter_boot(&fixture);

            assert_pending_phase(&first, Phase::Complete);
            assert_eq!(fixture.fixture.canonical_record(), successor);
            state_before.assert_unchanged(&fixture);
            assert_receipt_retired(&fixture);
            fixture.fixture.source = successor.clone();

            let clean = enter_clean_boot(&fixture);

            assert_canonical_absent(&fixture.fixture.installation.root);
            state_before.assert_unchanged(&fixture);
            assert_receipt_retired(&fixture);
            drop(clean);

            let clean_again = enter_clean_boot(&fixture);
            assert_canonical_absent(&fixture.fixture.installation.root);
            state_before.assert_unchanged(&fixture);
            assert_receipt_retired(&fixture);
            drop(clean_again);
        }
    }
}

#[test]
fn committed_retirement_report_error_reenters_finish_and_completes() {
    let mut fixture = commit_cleanup_complete_fixture(Epoch::Current);
    let source = fixture.fixture.source.clone();
    let successor = exact_complete(&source);
    db::state::arm_boot_publication_receipt_retirement_after_commit_error(
        db::Error::RowNotFound,
    );

    let first = enter_boot(&fixture);

    assert_pending_phase(&first, Phase::CommitCleanupComplete);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_receipt_retired(&fixture);

    let second = enter_boot(&fixture);

    assert_pending_phase(&second, Phase::Complete);
    assert_eq!(fixture.fixture.canonical_record(), successor);
    assert_receipt_retired(&fixture);
    fixture.fixture.source = successor;
}

#[derive(Clone, Copy)]
struct JournalFault {
    arm: fn(),
    consumed: fn(),
    durable: DurableActiveReblitCommitCleanupCompleteRecord,
}

const JOURNAL_FAULTS: [JournalFault; 5] = [
    JournalFault {
        arm: arm_next_temporary_sync_fault,
        consumed: assert_temporary_sync_fault_consumed,
        durable: DurableActiveReblitCommitCleanupCompleteRecord::CommitCleanupComplete,
    },
    JournalFault {
        arm: arm_next_update_exchange_fault,
        consumed: assert_update_exchange_fault_consumed,
        durable: DurableActiveReblitCommitCleanupCompleteRecord::CommitCleanupComplete,
    },
    JournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        consumed: assert_update_first_directory_sync_fault_consumed,
        durable: DurableActiveReblitCommitCleanupCompleteRecord::Complete,
    },
    JournalFault {
        arm: arm_next_displaced_unlink_fault,
        consumed: assert_displaced_unlink_fault_consumed,
        durable: DurableActiveReblitCommitCleanupCompleteRecord::Complete,
    },
    JournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        consumed: assert_update_final_directory_sync_fault_consumed,
        durable: DurableActiveReblitCommitCleanupCompleteRecord::Complete,
    },
];

#[test]
fn completed_cleanup_all_five_journal_faults_classify_and_converge() {
    for fault in JOURNAL_FAULTS {
        let mut fixture = commit_cleanup_complete_fixture(Epoch::Current);
        let source = fixture.fixture.source.clone();
        let successor = exact_complete(&source);
        (fault.arm)();

        let first = enter_boot(&fixture);

        (fault.consumed)();
        assert_advance_failure(&first, fault.durable);
        assert_eq!(
            fixture.fixture.canonical_record(),
            match fault.durable {
                DurableActiveReblitCommitCleanupCompleteRecord::CommitCleanupComplete => {
                    source
                }
                DurableActiveReblitCommitCleanupCompleteRecord::Complete => successor.clone(),
            },
        );
        assert_receipt_retired(&fixture);

        if fault.durable == DurableActiveReblitCommitCleanupCompleteRecord::CommitCleanupComplete {
            let second = enter_boot(&fixture);
            assert_pending_phase(&second, Phase::Complete);
            assert_eq!(fixture.fixture.canonical_record(), successor);
            assert_receipt_retired(&fixture);
        }
        fixture.fixture.source = successor;

        let clean = enter_clean_boot(&fixture);
        assert_canonical_absent(&fixture.fixture.installation.root);
        assert_receipt_retired(&fixture);
        drop(clean);

        let clean_again = enter_clean_boot(&fixture);
        assert_canonical_absent(&fixture.fixture.installation.root);
        assert_receipt_retired(&fixture);
        drop(clean_again);
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

    fn expected_stage(self) -> Option<ActiveReblitCommitCleanupCompleteValidationStage> {
        match self {
            Self::FinalAuthority => None,
            Self::SameStore => Some(ActiveReblitCommitCleanupCompleteValidationStage::SameStore),
            Self::BeforeReopen | Self::ReopenedOldBinding => Some(
                ActiveReblitCommitCleanupCompleteValidationStage::ReopenedOldBinding,
            ),
            Self::OldBindingBeforeFreshCapture => Some(
                ActiveReblitCommitCleanupCompleteValidationStage::ReopenedOldBindingAfterFreshCapture,
            ),
            Self::ReopenedFreshBinding => Some(
                ActiveReblitCommitCleanupCompleteValidationStage::ReopenedFreshBinding,
            ),
        }
    }
}

#[test]
fn complete_persistence_all_binding_windows_reject_same_bytes_on_a_new_inode() {
    for hook in BindingHook::ALL {
        let fixture = commit_cleanup_complete_fixture(Epoch::Current);
        let source = fixture.fixture.source.clone();
        let successor = exact_complete(&source);
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        let retired = capture_retired(&fixture, &journal, &reservation);
        let replacement = same_byte_different_inode_hook(
            &fixture,
            &format!("cleanup-complete-persistence-{hook:?}"),
        );
        match hook {
            BindingHook::FinalAuthority => {
                arm_before_active_reblit_commit_cleanup_complete_final_revalidation(replacement)
            }
            BindingHook::SameStore => {
                arm_before_active_reblit_commit_cleanup_complete_same_store_validation(replacement)
            }
            BindingHook::BeforeReopen => {
                arm_after_active_reblit_commit_cleanup_complete_same_store_before_reopen(replacement)
            }
            BindingHook::ReopenedOldBinding => {
                arm_before_active_reblit_commit_cleanup_complete_reopened_validation(replacement)
            }
            BindingHook::OldBindingBeforeFreshCapture => {
                arm_after_active_reblit_commit_cleanup_complete_old_binding_validation(replacement)
            }
            BindingHook::ReopenedFreshBinding => {
                arm_before_active_reblit_commit_cleanup_complete_fresh_binding_validation(replacement)
            }
        }

        let error = persist_active_reblit_commit_cleanup_complete_to_complete_and_reopen(
            journal,
            retired,
        )
        .expect_err("same-byte journal inode substitution returned Complete authority");

        match hook.expected_stage() {
            None => assert!(matches!(
                error,
                ActiveReblitCommitCleanupCompletePersistenceError::Authority(_)
            )),
            Some(stage) => assert!(matches!(
                error,
                ActiveReblitCommitCleanupCompletePersistenceError::PostAdvanceValidation {
                    durable: DurableActiveReblitCommitCleanupCompleteRecord::Complete,
                    stage: actual,
                    ..
                } if actual == stage
            )),
        }
        assert_eq!(
            fixture.fixture.canonical_record(),
            if matches!(hook, BindingHook::FinalAuthority) {
                source
            } else {
                successor
            },
        );
        assert_receipt_retired(&fixture);
    }
}

#[test]
fn completed_cleanup_database_and_namespace_races_fail_closed() {
    let fixture = commit_cleanup_complete_fixture(Epoch::Current);
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    arm_between_active_reblit_commit_cleanup_complete_database_captures(move || {
        database
            .change_summary_for_test(candidate, Some("completed cleanup database race"))
            .unwrap();
    });

    let error = enter_boot(&fixture);

    assert!(matches!(
        error,
        startup_gate::Error::ActiveReblitCommitCleanupCompleteDispatch(
            active_reblit_commit_cleanup_complete::Error::Authority(_)
        )
    ));
    assert_eq!(fixture.fixture.canonical_record(), fixture.fixture.source);

    let fixture = commit_cleanup_complete_fixture(Epoch::Current);
    let successor = exact_complete(&fixture.fixture.source);
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let retired = capture_retired(&fixture, &journal, &reservation);
    let staging = fixture.fixture.installation.root.join(".cast/root/staging");
    let mode = fs::metadata(&staging).unwrap().permissions().mode() & 0o7777;
    let changed_mode = if mode == 0o700 { 0o755 } else { 0o700 };
    arm_before_active_reblit_commit_cleanup_complete_same_store_validation(move || {
        fs::set_permissions(staging, fs::Permissions::from_mode(changed_mode)).unwrap();
    });

    let error = persist_active_reblit_commit_cleanup_complete_to_complete_and_reopen(
        journal,
        retired,
    )
    .expect_err("completed cleanup namespace race returned Complete authority");

    assert!(matches!(
        error,
        ActiveReblitCommitCleanupCompletePersistenceError::PostAdvanceValidation {
            durable: DurableActiveReblitCommitCleanupCompleteRecord::Complete,
            stage: ActiveReblitCommitCleanupCompleteValidationStage::SameStore,
            ..
        }
    ));
    assert_eq!(fixture.fixture.canonical_record(), successor);
    assert_receipt_retired(&fixture);
}

pub(super) fn commit_cleanup_complete_fixture(epoch: Epoch) -> BootRepairFixture {
    let mut fixture = commit_decided_fixture(epoch, CleanupLayout::Finish);
    let successor = fixture.fixture.source.forward_successor(None).unwrap();
    assert_eq!(successor.phase, Phase::CommitCleanupComplete);

    let first = enter_boot(&fixture);

    assert_pending_phase(&first, Phase::CommitCleanupComplete);
    assert_eq!(fixture.fixture.canonical_record(), successor);
    fixture.fixture.source = successor;
    fixture
}

#[derive(Debug, Eq, PartialEq)]
struct NonReceiptDatabaseSnapshot {
    states: Vec<crate::State>,
    in_flight: Option<db::state::InFlightTransition>,
    candidate_ownership: db::state::TransitionOwnership,
    candidate_provenance: Option<db::state::MetadataProvenance>,
    previous_ownership: db::state::TransitionOwnership,
    previous_provenance: Option<db::state::MetadataProvenance>,
}

impl NonReceiptDatabaseSnapshot {
    fn capture(fixture: &BootRepairFixture) -> Self {
        Self {
            states: fixture.fixture.database.all().unwrap(),
            in_flight: fixture.fixture.database.audit_in_flight_transition().unwrap(),
            candidate_ownership: fixture
                .fixture
                .database
                .transition_ownership(
                    fixture.fixture.candidate_state,
                    &fixture.fixture.source.transition_id,
                )
                .unwrap(),
            candidate_provenance: fixture
                .fixture
                .database
                .metadata_provenance(fixture.fixture.candidate_state)
                .unwrap(),
            previous_ownership: fixture
                .fixture
                .database
                .transition_ownership(
                    fixture.fixture.previous_state,
                    &fixture.fixture.source.transition_id,
                )
                .unwrap(),
            previous_provenance: fixture
                .fixture
                .database
                .metadata_provenance(fixture.fixture.previous_state)
                .unwrap(),
        }
    }

    fn assert_unchanged(&self, fixture: &BootRepairFixture) {
        assert_eq!(&Self::capture(fixture), self);
    }
}

fn capture_retired<'reservation>(
    fixture: &BootRepairFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> ActiveReblitCommitCleanupCompleteRetiredAuthority<'reservation> {
    let seal = ActiveReblitCommitCleanupCompleteSeal::new_for_test();
    match ActiveReblitCommitCleanupCompleteAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        journal,
        &fixture.fixture.database,
        reservation,
        &fixture.fixture.source,
    )
    .unwrap()
    {
        ActiveReblitCommitCleanupCompleteAdmission::Apply(authority) => {
            authority.retire(journal).unwrap()
        }
        _ => panic!("exact promoted CommitCleanupComplete evidence did not admit Apply"),
    }
}

fn retire_receipt(fixture: &BootRepairFixture) {
    let pair = receipt_pair(fixture);
    fixture
        .fixture
        .database
        .retire_promoted_boot_publication_receipt_head(
            &fixture.fixture.source.transition_id,
            &pair,
        )
        .unwrap();
    assert_receipt_retired(fixture);
}

pub(super) fn assert_receipt_retired(fixture: &BootRepairFixture) {
    let pair = receipt_pair(fixture);
    assert_eq!(
        fixture
            .fixture
            .database
            .inspect_exact_boot_publication_receipt_retirement_state(
                &fixture.fixture.source.transition_id,
                &pair,
            )
            .unwrap(),
        db::state::BootPublicationReceiptRetirementDurableState::Retired,
    );
    let state = fixture
        .fixture
        .database
        .boot_publication_receipt_state()
        .unwrap();
    assert_eq!(state.head().committed(), None);
    assert!(state.head().pending().is_none());
    assert!(state.committed().is_none());
    assert!(state.pending().is_none());
}

fn receipt_pair(
    fixture: &BootRepairFixture,
) -> crate::boot_publication::BootPublicationReceiptPair {
    fixture
        .fixture
        .source
        .boot_publication_receipt_correlation()
        .unwrap()
        .unwrap()
}

fn exact_complete(source: &TransitionRecord) -> TransitionRecord {
    let successor = source.forward_successor(None).unwrap();
    assert_eq!(successor.phase, Phase::Complete);
    successor
}

fn assert_advance_failure(
    error: &startup_gate::Error,
    expected: DurableActiveReblitCommitCleanupCompleteRecord,
) {
    assert!(matches!(
        error,
        startup_gate::Error::ActiveReblitCommitCleanupCompleteDispatch(
            active_reblit_commit_cleanup_complete::Error::Persistence(
                ActiveReblitCommitCleanupCompletePersistenceError::Advance { durable, .. }
            )
        ) if *durable == expected
    ));
}
