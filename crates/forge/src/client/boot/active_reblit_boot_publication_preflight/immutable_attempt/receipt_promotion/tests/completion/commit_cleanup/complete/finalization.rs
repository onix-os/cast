use super::*;

use std::{
    ffi::OsStr,
    fs,
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::Duration,
};

use crate::{
    client::{
        CoordinatorActiveStateReservation,
        active_reblit_boot_publication_preflight::ActiveReblitBootFinalizationError,
        active_reblit_boot_sync_staging::CompleteStagedActiveReblitFinalizationError,
        snapshot_startup_recovery_namespace,
        startup_recovery::{
            ActiveReblitCompleteFinalizationError,
            arm_before_active_reblit_complete_finalization_final_revalidation,
        },
    },
    transition_journal::{
        TransitionJournalRecordDeleteError, TransitionJournalRecordDeleteState,
        TransitionJournalStore, arm_next_delete_canonical_unlink_fault,
        arm_next_delete_directory_sync_fault,
        assert_delete_canonical_unlink_fault_consumed,
        assert_delete_directory_sync_fault_consumed,
    },
};

const SUCCESS_SCENARIO_COUNT: usize = 1;
const BINDING_REJECTION_SCENARIO_COUNT: usize = 1;
const DELETE_FAULT_SCENARIO_COUNT: usize = 2;
pub(super) const SCENARIO_COUNT: usize = SUCCESS_SCENARIO_COUNT
    + BINDING_REJECTION_SCENARIO_COUNT
    + DELETE_FAULT_SCENARIO_COUNT;

#[test]
fn exact_complete_finalizes_once_and_preserves_clean_authority() {
    with_exact_complete_handoff!(
        |fixture, topology_fixture, plan, client, completed| {
            let complete = completed.record().clone();
            let fingerprint = completed.receipt_fingerprint();
            let database_outcome = completed.database_outcome();
            let staging_outcome = completed.staging_outcome();
            let inventory = completed.inventory() as *const _;
            let publication_count = completed.publication_count();
            let published_count = completed.published_count();
            let already_exact_count = completed.already_exact_count();
            let replaced_count = completed.replaced_count();
            let evidence = evidence_snapshot(completed.evidence());
            let namespace_before =
                snapshot_startup_recovery_namespace(&fixture.installation.root);
            let database_before = fixture.state_db.boot_publication_receipt_state().unwrap();
            let outputs_before =
                publication_snapshot!(&plan, topology_fixture.publication_root());

            let finalized = completed.finalize(&client).unwrap();

            assert_eq!(complete.phase, Phase::Complete);
            assert_eq!(complete.generation, 15);
            assert_eq!(finalized.complete_record(), &complete);
            assert_eq!(finalized.receipt_fingerprint(), fingerprint);
            assert_eq!(finalized.database_outcome(), database_outcome);
            assert_eq!(finalized.staging_outcome(), staging_outcome);
            assert!(std::ptr::eq(finalized.inventory(), inventory));
            assert_eq!(finalized.publication_count(), publication_count);
            assert_eq!(finalized.published_count(), published_count);
            assert_eq!(finalized.already_exact_count(), already_exact_count);
            assert_eq!(finalized.replaced_count(), replaced_count);
            assert_eq!(evidence_snapshot(finalized.evidence()), evidence);
            assert_eq!(
                snapshot_startup_recovery_namespace(&fixture.installation.root),
                namespace_before,
            );
            assert_eq!(
                fixture.state_db.boot_publication_receipt_state().unwrap(),
                database_before,
            );
            assert_eq!(
                publication_snapshot!(&plan, topology_fixture.publication_root()),
                outputs_before,
            );
            assert_terminal_journal_absent(&fixture.installation);
            let cast = fixture
                .installation
                .retained_mutable_cast_directory()
                .unwrap();
            assert!(
                TransitionJournalStore::try_open_in_retained_cast(
                    cast,
                    &fixture.installation.root,
                )
                .is_err(),
                "clean handoff must retain the same journal lock",
            );

            let (reached_sender, reached_receiver) = mpsc::channel();
            let (acquired_sender, acquired_receiver) = mpsc::channel();
            let contender = thread::spawn(move || {
                crate::client::fixed_staging::arm_before_coordinator_lock(move || {
                    reached_sender.send(()).unwrap();
                });
                let reservation = CoordinatorActiveStateReservation::acquire().unwrap();
                acquired_sender.send(()).unwrap();
                drop(reservation);
            });
            reached_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(matches!(
                acquired_receiver.recv_timeout(Duration::from_millis(100)),
                Err(RecvTimeoutError::Timeout),
            ));
            drop(finalized);
            acquired_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            contender.join().unwrap();

            let reopened = TransitionJournalStore::try_open_in_retained_cast(
                fixture
                    .installation
                    .retained_mutable_cast_directory()
                    .unwrap(),
                &fixture.installation.root,
            )
            .unwrap();
            assert!(reopened.load().unwrap().is_none());
        }
    );
}

#[test]
fn same_bytes_new_inode_rejects_finalization_before_delete_without_other_effects() {
    with_exact_complete_handoff!(
        |fixture, topology_fixture, plan, client, completed| {
            let complete = completed.record().clone();
            let namespace_before =
                snapshot_startup_recovery_namespace(&fixture.installation.root);
            let database_before = fixture.state_db.boot_publication_receipt_state().unwrap();
            let outputs_before =
                publication_snapshot!(&plan, topology_fixture.publication_root());
            let canonical = canonical_journal(&fixture.installation);
            let displaced = canonical.with_extension("finalization-bound-inode");
            arm_before_active_reblit_complete_finalization_final_revalidation(move || {
                replace_file_identity(&canonical, &displaced);
            });

            assert!(completed.finalize(&client).is_err());

            assert_eq!(load_journal_record(&fixture.installation), complete);
            assert_clean_journal_inventory(&fixture.installation);
            assert_eq!(
                snapshot_startup_recovery_namespace(&fixture.installation.root),
                namespace_before,
            );
            assert_eq!(
                fixture.state_db.boot_publication_receipt_state().unwrap(),
                database_before,
            );
            assert_eq!(
                publication_snapshot!(&plan, topology_fixture.publication_root()),
                outputs_before,
            );
        }
    );
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
fn terminal_delete_fault_states_return_no_clean_handoff() {
    for fault in DELETE_FAULTS {
        with_exact_complete_handoff!(
            |fixture, topology_fixture, plan, client, completed| {
                let complete = completed.record().clone();
                let namespace_before =
                    snapshot_startup_recovery_namespace(&fixture.installation.root);
                let database_before =
                    fixture.state_db.boot_publication_receipt_state().unwrap();
                let outputs_before =
                    publication_snapshot!(&plan, topology_fixture.publication_root());
                (fault.arm)();

                let error = completed.finalize(&client).unwrap_err();

                (fault.consumed)();
                assert!(matches!(
                    error,
                    ActiveReblitBootFinalizationError::Finalization(
                        CompleteStagedActiveReblitFinalizationError::Finalization(
                            ActiveReblitCompleteFinalizationError::Delete(
                                TransitionJournalRecordDeleteError::Storage { state, .. },
                            ),
                        ),
                    ) if state == fault.state
                ));
                match fault.state {
                    TransitionJournalRecordDeleteState::ExactSource => {
                        assert_eq!(load_journal_record(&fixture.installation), complete);
                        assert_clean_journal_inventory(&fixture.installation);
                    }
                    TransitionJournalRecordDeleteState::Absent => {
                        assert_terminal_journal_absent(&fixture.installation);
                    }
                }
                assert_eq!(
                    snapshot_startup_recovery_namespace(&fixture.installation.root),
                    namespace_before,
                );
                assert_eq!(
                    fixture.state_db.boot_publication_receipt_state().unwrap(),
                    database_before,
                );
                assert_eq!(
                    publication_snapshot!(&plan, topology_fixture.publication_root()),
                    outputs_before,
                );
            }
        );
    }
}

fn assert_terminal_journal_absent(installation: &Installation) {
    assert!(!canonical_journal(installation).exists());
    let mut names = fs::read_dir(installation.root.join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        [OsStr::new("state-transition.lock").to_owned()],
        "terminal finalization left journal residue",
    );
}
