//! Focused installed-receipt and `CommitCleanupComplete` dispatch contracts.

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    time::{Duration, Instant},
};

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
        Phase, TransitionJournalStore, TransitionRecord, encode,
        arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed,
        assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    super::test_fixture::stage_test_boot_publication_receipts,
    boot_sync_complete_support::{open_boot_sync_complete_journal, same_byte_different_inode_hook},
    commit_cleanup_effect::{
        CleanupLayout, commit_decided_fixture, no_boot_commit_decided_fixture,
    },
    support::{
        BootRepairFixture, Epoch, assert_pending_phase, enter_boot,
    },
};

#[test]
fn completed_cleanup_current_and_historical_reaches_complete_once() {
    for epoch in Epoch::ALL {
        let fixture = commit_cleanup_complete_fixture(epoch);
        let source = fixture.fixture.source.clone();
        let successor = exact_complete(&source);
        let database_before = NonReceiptDatabaseSnapshot::capture(&fixture);
        let receipt_before = installed_receipt_state(&fixture);

        let first = enter_boot(&fixture);

        assert_pending_phase(&first, Phase::Complete);
        assert_eq!(fixture.fixture.canonical_record(), successor);
        database_before.assert_unchanged(&fixture);
        assert_eq!(installed_receipt_state(&fixture), receipt_before);
        assert_installed_receipt_promoted(&fixture);
    }
}

#[test]
fn system_triggered_no_boot_cleanup_reaches_exact_complete_without_receipt_mutation() {
    for epoch in Epoch::ALL {
        for install_unrelated_receipt in [false, true] {
            let fixture = no_boot_commit_cleanup_complete_fixture(
                epoch,
                install_unrelated_receipt,
            );
            let source = fixture.fixture.source.clone();
            let successor = exact_complete(&source);
            let database_before = fixture.fixture.database_snapshot();
            let receipt_chain_before = fixture
                .fixture
                .database
                .load_current_exact_promoted_boot_publication_receipt_chain()
                .unwrap();

            let first = enter_boot(&fixture);

            assert_pending_phase(&first, Phase::Complete);
            assert_eq!(source.generation, 12);
            assert_eq!(successor.generation, 13);
            assert_eq!(successor.boot_publication_receipts, None);
            assert_eq!(fixture.fixture.canonical_record(), successor);
            assert_eq!(fixture.fixture.database_snapshot(), database_before);
            assert_eq!(
                fixture
                    .fixture
                    .database
                    .load_current_exact_promoted_boot_publication_receipt_chain()
                    .unwrap(),
                receipt_chain_before,
            );
        }
    }

    let mut trigger_disabled = no_boot_commit_cleanup_complete_fixture(Epoch::Current, false);
    trigger_disabled.fixture.source.options.run_system_triggers = false;
    fs::write(
        trigger_disabled
            .fixture
            .installation
            .root
            .join(".cast/journal/state-transition"),
        encode(&trigger_disabled.fixture.source).unwrap(),
    )
    .unwrap();

    let pending = enter_boot(&trigger_disabled);

    assert_pending_phase(&pending, Phase::CommitCleanupComplete);
    assert_eq!(
        trigger_disabled.fixture.canonical_record(),
        trigger_disabled.fixture.source,
    );

    let same_transition_receipt =
        no_boot_commit_cleanup_complete_fixture(Epoch::Current, false);
    install_current_transition_receipt(&same_transition_receipt);
    let source = same_transition_receipt.fixture.source.clone();
    let database_before = same_transition_receipt.fixture.database_snapshot();
    let namespace_before = same_transition_receipt.fixture.namespace_snapshot();
    let receipt_chain_before = same_transition_receipt
        .fixture
        .database
        .load_current_exact_promoted_boot_publication_receipt_chain()
        .unwrap();

    let error = enter_boot(&same_transition_receipt);

    assert!(matches!(
        error,
        startup_gate::Error::ActiveReblitCommitCleanupCompleteDispatch(
            active_reblit_commit_cleanup_complete::Error::Authority(_),
        )
    ));
    assert_eq!(same_transition_receipt.fixture.canonical_record(), source);
    assert_eq!(
        same_transition_receipt.fixture.database_snapshot(),
        database_before,
    );
    assert_eq!(
        same_transition_receipt.fixture.namespace_snapshot(),
        namespace_before,
    );
    assert_eq!(
        same_transition_receipt
            .fixture
            .database
            .load_current_exact_promoted_boot_publication_receipt_chain()
            .unwrap(),
        receipt_chain_before,
    );
}

#[test]
fn completed_cleanup_bound_advance_reopens_without_mutating_installed_receipt() {
    let fixture = commit_cleanup_complete_fixture(Epoch::Current);
    let source = fixture.fixture.source.clone();
    let successor = exact_complete(&source);
    let receipt_before = installed_receipt_state(&fixture);
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let ready = capture_ready(&fixture, &journal, &reservation);

    let (journal, actual) =
        persist_active_reblit_commit_cleanup_complete_to_complete_and_reopen(journal, ready)
            .unwrap();

    assert_eq!(actual, successor);
    assert_eq!(fixture.fixture.canonical_record(), successor);
    assert_eq!(installed_receipt_state(&fixture), receipt_before);
    assert_installed_receipt_promoted(&fixture);
    drop(journal);
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
        let receipt_before = installed_receipt_state(&fixture);
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
        assert_eq!(installed_receipt_state(&fixture), receipt_before);
        assert_installed_receipt_promoted(&fixture);

        if fault.durable == DurableActiveReblitCommitCleanupCompleteRecord::CommitCleanupComplete {
            let second = enter_boot(&fixture);
            assert_pending_phase(&second, Phase::Complete);
            assert_eq!(fixture.fixture.canonical_record(), successor);
            assert_eq!(installed_receipt_state(&fixture), receipt_before);
            assert_installed_receipt_promoted(&fixture);
        }
        fixture.fixture.source = successor;
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
        let ready = capture_ready(&fixture, &journal, &reservation);
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
            ready,
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
        assert_installed_receipt_promoted(&fixture);
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
    let ready = capture_ready(&fixture, &journal, &reservation);
    let staging = fixture.fixture.installation.root.join(".cast/root/staging");
    let mode = fs::metadata(&staging).unwrap().permissions().mode() & 0o7777;
    let changed_mode = if mode == 0o700 { 0o755 } else { 0o700 };
    arm_before_active_reblit_commit_cleanup_complete_same_store_validation(move || {
        fs::set_permissions(staging, fs::Permissions::from_mode(changed_mode)).unwrap();
    });

    let error = persist_active_reblit_commit_cleanup_complete_to_complete_and_reopen(
        journal,
        ready,
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
    assert_installed_receipt_promoted(&fixture);
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

pub(super) fn no_boot_commit_cleanup_complete_fixture(
    epoch: Epoch,
    install_unrelated_receipt: bool,
) -> BootRepairFixture {
    let mut fixture = no_boot_commit_decided_fixture(
        epoch,
        CleanupLayout::Finish,
        install_unrelated_receipt,
    );
    let successor = fixture.fixture.source.forward_successor(None).unwrap();
    assert_eq!(successor.phase, Phase::CommitCleanupComplete);
    assert_eq!(successor.generation, 12);

    let first = enter_boot(&fixture);

    assert_pending_phase(&first, Phase::CommitCleanupComplete);
    assert_eq!(fixture.fixture.canonical_record(), successor);
    fixture.fixture.source = successor;
    fixture
}

pub(super) fn install_current_transition_receipt(fixture: &BootRepairFixture) {
    stage_test_boot_publication_receipts(
        &fixture.fixture.database,
        &fixture.fixture.source.transition_id,
        false,
    );
    let receipt_state = fixture
        .fixture
        .database
        .boot_publication_receipt_state()
        .unwrap();
    let pending = receipt_state
        .pending()
        .expect("same-transition receipt was staged");
    fixture
        .fixture
        .database
        .promote_boot_publication_receipt(
            pending,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
    let db::state::CurrentExactPromotedBootPublicationReceiptChain::Installed(installed) = fixture
        .fixture
        .database
        .load_current_exact_promoted_boot_publication_receipt_chain()
        .unwrap()
    else {
        panic!("same-transition receipt was not installed");
    };
    assert_eq!(
        installed.installed_receipt().body().transition_id(),
        &fixture.fixture.source.transition_id,
    );
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

fn capture_ready<'reservation>(
    fixture: &BootRepairFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> ActiveReblitCommitCleanupCompleteAuthority<'reservation> {
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
        ActiveReblitCommitCleanupCompleteAdmission::Ready(authority) => authority,
        _ => panic!("exact promoted CommitCleanupComplete evidence did not admit Ready"),
    }
}

pub(super) fn installed_receipt_state(
    fixture: &BootRepairFixture,
) -> db::state::BootPublicationReceiptState {
    let pair = receipt_pair(fixture);
    fixture
        .fixture
        .database
        .load_exact_promoted_boot_publication_receipt_state(
            &fixture.fixture.source.transition_id,
            &pair,
        )
        .unwrap()
}

pub(super) fn assert_installed_receipt_promoted(fixture: &BootRepairFixture) {
    let pair = receipt_pair(fixture);
    let state = installed_receipt_state(fixture);
    assert_eq!(state.head().committed(), Some(pair.pending));
    assert!(state.head().pending().is_none());
    assert_eq!(
        state.committed().map(|receipt| receipt.fingerprint()),
        Some(pair.pending),
    );
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
