//! Focused exact forward `Complete` terminal-finalization contracts.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{
            self, ActiveReblitCompleteFinalizationSeal, CleanSystemStartup,
            active_reblit_complete_finalization,
        },
        startup_reconciliation::{
            ActiveReblitCompleteFinalizationAdmission,
            ActiveReblitCompleteFinalizationAuthority,
            active_reblit_commit_cleanup_exchange_attempt_count,
            arm_between_active_reblit_complete_finalization_database_captures,
            arm_between_active_reblit_complete_finalization_post_delete_database_captures,
            reset_active_reblit_commit_cleanup_durability_events,
            reset_active_reblit_commit_cleanup_exchange_attempt_count,
            take_active_reblit_commit_cleanup_durability_events,
        },
        startup_recovery::{
            ActiveReblitCompleteFinalizationError,
            arm_after_active_reblit_complete_finalization_delete,
            arm_before_active_reblit_complete_finalization_final_revalidation,
            finalize_active_reblit_complete,
        },
    },
    transition_journal::{
        Phase, TransitionJournalRecordDeleteError,
        TransitionJournalRecordDeleteState, TransitionJournalStore,
        arm_next_delete_canonical_unlink_fault,
        arm_next_delete_directory_sync_fault,
        assert_delete_canonical_unlink_fault_consumed,
        assert_delete_directory_sync_fault_consumed, encode,
    },
};

use super::{
    boot_sync_complete_support::{open_boot_sync_complete_journal, same_byte_different_inode_hook},
    commit_cleanup_complete_startup_dispatch::{
        assert_installed_receipt_promoted, commit_cleanup_complete_fixture,
        install_current_transition_receipt, no_boot_commit_cleanup_complete_fixture,
    },
    commit_cleanup_effect::{CleanupLayout, commit_decided_fixture},
    support::{
        BootRepairFixture, Epoch, assert_canonical_absent,
        assert_complete_route_journal_only, assert_pending_phase, enter_boot,
        enter_clean_boot, reset_complete_route_effect_observers,
    },
};

#[test]
fn forward_complete_current_and_historical_finalizes_next_entry_and_clean_reentry() {
    for epoch in Epoch::ALL {
        let fixture = complete_fixture(epoch);
        let states_before = fixture.fixture.database.all().unwrap();
        let in_flight_before = fixture.fixture.database.audit_in_flight_transition().unwrap();
        let namespace_before = fixture.fixture.namespace_snapshot();
        let exact_complete = fixture.fixture.source.clone();
        reset_unrelated_effect_observers();

        let clean = enter_clean_boot(&fixture);

        assert_canonical_absent(&fixture.fixture.installation.root);
        assert_eq!(fixture.fixture.database.all().unwrap(), states_before);
        assert_eq!(
            fixture.fixture.database.audit_in_flight_transition().unwrap(),
            in_flight_before,
        );
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_installed_receipt_promoted(&fixture);
        assert_no_unrelated_effects();
        drop(clean);

        let clean_again = enter_clean_boot(&fixture);

        assert_canonical_absent(&fixture.fixture.installation.root);
        assert_eq!(fixture.fixture.database.all().unwrap(), states_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_installed_receipt_promoted(&fixture);
        assert_no_unrelated_effects();
        assert_eq!(exact_complete.phase, Phase::Complete);
        drop(clean_again);
    }
}

#[test]
fn system_triggered_no_boot_complete_finalizes_without_receipt_mutation() {
    for epoch in Epoch::ALL {
        for install_unrelated_receipt in [false, true] {
            let fixture = no_boot_complete_fixture(epoch, install_unrelated_receipt);
            let states_before = fixture.fixture.database.all().unwrap();
            let in_flight_before = fixture
                .fixture
                .database
                .audit_in_flight_transition()
                .unwrap();
            let namespace_before = fixture.fixture.namespace_snapshot();
            let receipt_chain_before = fixture
                .fixture
                .database
                .load_current_exact_promoted_boot_publication_receipt_chain()
                .unwrap();
            reset_unrelated_effect_observers();

            let clean = enter_clean_boot(&fixture);

            assert_canonical_absent(&fixture.fixture.installation.root);
            assert_eq!(fixture.fixture.source.generation, 13);
            assert_eq!(fixture.fixture.source.boot_publication_receipts, None);
            assert_eq!(fixture.fixture.database.all().unwrap(), states_before);
            assert_eq!(
                fixture
                    .fixture
                    .database
                    .audit_in_flight_transition()
                    .unwrap(),
                in_flight_before,
            );
            assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
            assert_eq!(
                fixture
                    .fixture
                    .database
                    .load_current_exact_promoted_boot_publication_receipt_chain()
                    .unwrap(),
                receipt_chain_before,
            );
            assert_no_unrelated_effects();
            drop(clean);

            let clean_again = enter_clean_boot(&fixture);

            assert_canonical_absent(&fixture.fixture.installation.root);
            assert_eq!(fixture.fixture.database.all().unwrap(), states_before);
            assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
            assert_eq!(
                fixture
                    .fixture
                    .database
                    .load_current_exact_promoted_boot_publication_receipt_chain()
                    .unwrap(),
                receipt_chain_before,
            );
            assert_no_unrelated_effects();
            drop(clean_again);
        }
    }

    let same_transition_receipt = no_boot_complete_fixture(Epoch::Current, false);
    install_current_transition_receipt(&same_transition_receipt);
    let source = same_transition_receipt.fixture.source.clone();
    let states_before = same_transition_receipt.fixture.database.all().unwrap();
    let in_flight_before = same_transition_receipt
        .fixture
        .database
        .audit_in_flight_transition()
        .unwrap();
    let namespace_before = same_transition_receipt.fixture.namespace_snapshot();
    let receipt_chain_before = same_transition_receipt
        .fixture
        .database
        .load_current_exact_promoted_boot_publication_receipt_chain()
        .unwrap();
    reset_unrelated_effect_observers();

    let error = enter_boot(&same_transition_receipt);

    assert!(matches!(
        error,
        startup_gate::Error::ActiveReblitCompleteFinalizationDispatch(
            active_reblit_complete_finalization::Error::Authority(_),
        )
    ));
    assert_eq!(same_transition_receipt.fixture.canonical_record(), source);
    assert_eq!(
        same_transition_receipt.fixture.database.all().unwrap(),
        states_before,
    );
    assert_eq!(
        same_transition_receipt
            .fixture
            .database
            .audit_in_flight_transition()
            .unwrap(),
        in_flight_before,
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
    assert_no_unrelated_effects();
}

#[test]
fn forward_complete_exact_incompatibilities_stay_pending_without_unrelated_effects() {
    let pending = complete_fixture(Epoch::Current);
    let pending_source = pending.fixture.source.clone();
    let pair = receipt_pair(&pending);
    pending
        .fixture
        .database
        .replace_boot_publication_receipt_head_for_test(
            pair.committed,
            Some((&pending.fixture.source.transition_id, pair.pending)),
        )
        .unwrap();
    reset_unrelated_effect_observers();

    let pending_error = enter_boot_with_context(&pending, "still-pending receipt");

    assert_pending_phase(&pending_error, Phase::Complete);
    assert_eq!(pending.fixture.canonical_record(), pending_source);
    assert_no_unrelated_effects();

    let no_provenance = complete_fixture(Epoch::Historical);
    let no_provenance_source = no_provenance.fixture.source.clone();
    no_provenance
        .fixture
        .database
        .delete_metadata_provenance_for_test(no_provenance.fixture.candidate_state)
        .unwrap();
    reset_unrelated_effect_observers();

    let provenance_error = enter_boot_with_context(&no_provenance, "missing provenance");

    assert_pending_phase(&provenance_error, Phase::Complete);
    assert_eq!(no_provenance.fixture.canonical_record(), no_provenance_source);
    assert_no_unrelated_effects();

    let mut apply_layout = commit_decided_fixture(Epoch::Current, CleanupLayout::Apply);
    let cleanup_complete = apply_layout.fixture.source.forward_successor(None).unwrap();
    let complete = cleanup_complete.forward_successor(None).unwrap();
    assert_eq!(complete.phase, Phase::Complete);
    let journal = open_boot_sync_complete_journal(&apply_layout);
    journal.advance(&apply_layout.fixture.source, &cleanup_complete).unwrap();
    journal.advance(&cleanup_complete, &complete).unwrap();
    drop(journal);
    apply_layout.fixture.source = complete.clone();
    reset_unrelated_effect_observers();

    let layout_error = enter_boot_with_context(&apply_layout, "incomplete cleanup layout");

    assert_pending_phase(&layout_error, Phase::Complete);
    assert_eq!(apply_layout.fixture.canonical_record(), complete);
    assert_no_unrelated_effects();

    let mut trigger_disabled = no_boot_complete_fixture(Epoch::Current, false);
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
    reset_unrelated_effect_observers();

    let trigger_error = enter_boot_with_context(&trigger_disabled, "disabled system triggers");

    assert_pending_phase(&trigger_error, Phase::Complete);
    assert_eq!(
        trigger_disabled.fixture.canonical_record(),
        trigger_disabled.fixture.source,
    );
    assert_no_unrelated_effects();
}

#[test]
fn forward_complete_binding_substitution_fails_before_delete_and_converges() {
    let fixture = complete_fixture(Epoch::Current);
    let complete = fixture.fixture.source.clone();
    reset_unrelated_effect_observers();
    arm_before_active_reblit_complete_finalization_final_revalidation(
        same_byte_different_inode_hook(&fixture, "forward-complete-finalizer"),
    );

    let error = enter_boot(&fixture);

    assert!(matches!(
        error,
        startup_gate::Error::ActiveReblitCompleteFinalizationDispatch(
            active_reblit_complete_finalization::Error::Finalization(
                ActiveReblitCompleteFinalizationError::Authority(_),
            ),
        )
    ));
    assert_eq!(fixture.fixture.canonical_record(), complete);
    assert_installed_receipt_promoted(&fixture);
    assert_no_unrelated_effects();

    let clean = enter_clean_boot(&fixture);
    assert_canonical_absent(&fixture.fixture.installation.root);
    assert_installed_receipt_promoted(&fixture);
    assert_no_unrelated_effects();
    drop(clean);
}

#[derive(Clone, Copy)]
struct DeleteFault {
    arm: fn(),
    consumed: fn(),
    state: TransitionJournalRecordDeleteState,
}

const DELETE_FAULTS: [DeleteFault; 2] = [
    DeleteFault {
        arm: arm_next_delete_canonical_unlink_fault,
        consumed: assert_delete_canonical_unlink_fault_consumed,
        state: TransitionJournalRecordDeleteState::ExactSource,
    },
    DeleteFault {
        arm: arm_next_delete_directory_sync_fault,
        consumed: assert_delete_directory_sync_fault_consumed,
        state: TransitionJournalRecordDeleteState::Absent,
    },
];

#[test]
fn forward_complete_delete_fault_states_remain_errors_and_next_entry_converges() {
    for fault in DELETE_FAULTS {
        let fixture = complete_fixture(Epoch::Current);
        let complete = fixture.fixture.source.clone();
        let states_before = fixture.fixture.database.all().unwrap();
        let namespace_before = fixture.fixture.namespace_snapshot();
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = capture_ready(&fixture, &journal, &reservation);
        reset_unrelated_effect_observers();
        (fault.arm)();

        let error = finalize_active_reblit_complete(journal, authority).unwrap_err();

        (fault.consumed)();
        assert!(matches!(
            error,
            ActiveReblitCompleteFinalizationError::Delete(
                TransitionJournalRecordDeleteError::Storage { state, .. },
            ) if state == fault.state
        ));
        match fault.state {
            TransitionJournalRecordDeleteState::ExactSource => {
                assert_eq!(fixture.fixture.canonical_record(), complete);
            }
            TransitionJournalRecordDeleteState::Absent => {
                assert_canonical_absent(&fixture.fixture.installation.root);
            }
        }
        assert_eq!(fixture.fixture.database.all().unwrap(), states_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_installed_receipt_promoted(&fixture);
        assert_no_unrelated_effects();
        drop(reservation);

        let clean = enter_clean_boot(&fixture);
        assert_canonical_absent(&fixture.fixture.installation.root);
        assert_eq!(fixture.fixture.database.all().unwrap(), states_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_installed_receipt_promoted(&fixture);
        assert_no_unrelated_effects();
        drop(clean);
    }
}

#[derive(Clone, Copy, Debug)]
enum PostDeleteRace {
    Database,
    Selection,
    Namespace,
    PublicRecord,
}

#[test]
fn forward_complete_post_delete_database_selection_namespace_and_public_record_races_fail_closed() {
    let capture_database = complete_fixture(Epoch::Current);
    let capture_database_record = capture_database.fixture.source.clone();
    let database = capture_database.fixture.database.clone();
    let candidate = capture_database.fixture.candidate_state;
    reset_unrelated_effect_observers();
    arm_between_active_reblit_complete_finalization_database_captures(move || {
        database
            .change_summary_for_test(candidate, Some("Complete capture database race"))
            .unwrap();
    });

    let database_error = enter_boot(&capture_database);

    assert!(matches!(
        database_error,
        startup_gate::Error::ActiveReblitCompleteFinalizationDispatch(
            active_reblit_complete_finalization::Error::Authority(_),
        )
    ));
    assert_eq!(capture_database.fixture.canonical_record(), capture_database_record);
    assert_installed_receipt_promoted(&capture_database);
    assert_no_unrelated_effects();

    let capture_namespace = complete_fixture(Epoch::Historical);
    let capture_namespace_record = capture_namespace.fixture.source.clone();
    let staging = capture_namespace
        .fixture
        .installation
        .root
        .join(".cast/root/staging");
    reset_unrelated_effect_observers();
    arm_between_active_reblit_complete_finalization_database_captures(move || {
        fs::set_permissions(staging, fs::Permissions::from_mode(0o755)).unwrap();
    });

    let namespace_error = enter_boot(&capture_namespace);

    assert!(matches!(
        namespace_error,
        startup_gate::Error::ActiveReblitCompleteFinalizationDispatch(
            active_reblit_complete_finalization::Error::Authority(_),
        )
    ));
    assert_eq!(capture_namespace.fixture.canonical_record(), capture_namespace_record);
    assert_installed_receipt_promoted(&capture_namespace);
    assert_no_unrelated_effects();

    for race in [
        PostDeleteRace::Database,
        PostDeleteRace::Selection,
        PostDeleteRace::Namespace,
        PostDeleteRace::PublicRecord,
    ] {
        let fixture = complete_fixture(Epoch::Current);
        let complete = fixture.fixture.source.clone();
        let database = fixture.fixture.database.clone();
        let candidate = fixture.fixture.candidate_state;
        let root = fixture.fixture.installation.root.clone();
        let canonical = root.join(".cast/journal/state-transition");
        let complete_bytes = encode(&complete).unwrap();
        reset_unrelated_effect_observers();
        arm_after_active_reblit_complete_finalization_delete(move || {
            arm_between_active_reblit_complete_finalization_post_delete_database_captures(
                move || match race {
                    PostDeleteRace::Database => database
                        .change_summary_for_test(candidate, Some("post-delete Complete race"))
                        .unwrap(),
                    PostDeleteRace::Selection => {
                        fs::write(
                            root.join("usr/.stateID"),
                            (i32::from(candidate) + 100).to_string(),
                        )
                        .unwrap();
                    }
                    PostDeleteRace::Namespace => {
                        fs::set_permissions(
                            root.join(".cast/root/staging"),
                            fs::Permissions::from_mode(0o755),
                        )
                        .unwrap();
                    }
                    PostDeleteRace::PublicRecord => {
                        fs::write(&canonical, complete_bytes).unwrap();
                        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
                    }
                },
            );
        });

        let error = enter_boot(&fixture);

        assert!(matches!(
            error,
            startup_gate::Error::ActiveReblitCompleteFinalizationDispatch(
                active_reblit_complete_finalization::Error::Finalization(
                    ActiveReblitCompleteFinalizationError::PostDeleteAuthority(_),
                ),
            )
        ), "unexpected {race:?} result: {error:?}");
        if matches!(race, PostDeleteRace::PublicRecord) {
            assert_eq!(fixture.fixture.canonical_record(), complete);
        } else {
            assert_canonical_absent(&fixture.fixture.installation.root);
        }
        assert_installed_receipt_promoted(&fixture);
        assert_no_unrelated_effects();
    }
}

fn complete_fixture(epoch: Epoch) -> BootRepairFixture {
    let mut fixture = commit_cleanup_complete_fixture(epoch);
    let complete = fixture.fixture.source.forward_successor(None).unwrap();
    assert_eq!(complete.phase, Phase::Complete);

    let entry = enter_boot(&fixture);

    assert_pending_phase(&entry, Phase::Complete);
    assert_eq!(fixture.fixture.canonical_record(), complete);
    fixture.fixture.source = complete;
    assert_installed_receipt_promoted(&fixture);
    fixture
}

fn no_boot_complete_fixture(
    epoch: Epoch,
    install_unrelated_receipt: bool,
) -> BootRepairFixture {
    let mut fixture = no_boot_commit_cleanup_complete_fixture(
        epoch,
        install_unrelated_receipt,
    );
    let complete = fixture.fixture.source.forward_successor(None).unwrap();
    assert_eq!(complete.phase, Phase::Complete);
    assert_eq!(complete.generation, 13);

    let entry = enter_boot(&fixture);

    assert_pending_phase(&entry, Phase::Complete);
    assert_eq!(fixture.fixture.canonical_record(), complete);
    fixture.fixture.source = complete;
    fixture
}

fn capture_ready<'reservation>(
    fixture: &BootRepairFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> ActiveReblitCompleteFinalizationAuthority<'reservation> {
    let seal = ActiveReblitCompleteFinalizationSeal::new_for_test();
    match ActiveReblitCompleteFinalizationAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        journal,
        &fixture.fixture.database,
        reservation,
        &fixture.fixture.source,
    )
    .unwrap()
    {
        ActiveReblitCompleteFinalizationAdmission::Ready(authority) => authority,
        _ => panic!("exact forward ActiveReblit Complete evidence did not admit finalization"),
    }
}

fn enter_boot_with_context(
    fixture: &BootRepairFixture,
    context: &str,
) -> startup_gate::Error {
    let reservation = ActiveStateReservation::acquire().unwrap();
    match CleanSystemStartup::enter(&fixture.fixture.system, &reservation) {
        Ok(_) => panic!("{context} unexpectedly admitted clean startup"),
        Err(error) => error,
    }
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

fn reset_unrelated_effect_observers() {
    reset_complete_route_effect_observers();
    reset_active_reblit_commit_cleanup_exchange_attempt_count();
    reset_active_reblit_commit_cleanup_durability_events();
}

fn assert_no_unrelated_effects() {
    assert_complete_route_journal_only();
    assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 0);
    assert!(take_active_reblit_commit_cleanup_durability_events().is_empty());
}
