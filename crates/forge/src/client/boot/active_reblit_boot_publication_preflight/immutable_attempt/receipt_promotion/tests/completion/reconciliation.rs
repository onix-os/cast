use super::*;

type FaultArm = fn();
type FaultAssert = fn();

const JOURNAL_FAULT_SCENARIO_COUNT: usize = 5;
const WRONG_GENERATION_SCENARIO_COUNT: usize = 1;
const REOPEN_CONTENTION_SCENARIO_COUNT: usize = 1;
pub(super) const SCENARIO_COUNT: usize =
    JOURNAL_FAULT_SCENARIO_COUNT
        + WRONG_GENERATION_SCENARIO_COUNT
        + REOPEN_CONTENTION_SCENARIO_COUNT;

#[test]
fn completion_journal_faults_reconcile_only_exact_started_or_complete_without_token() {
    let cases: [
        (
            FaultArm,
            FaultAssert,
            DurableActiveReblitBootSyncCompletionRecord,
        );
        JOURNAL_FAULT_SCENARIO_COUNT
    ] = [
        (
            arm_next_temporary_sync_fault,
            assert_temporary_sync_fault_consumed,
            DurableActiveReblitBootSyncCompletionRecord::BootSyncStarted,
        ),
        (
            arm_next_update_exchange_fault,
            assert_update_exchange_fault_consumed,
            DurableActiveReblitBootSyncCompletionRecord::BootSyncStarted,
        ),
        (
            arm_next_update_first_directory_sync_fault,
            assert_update_first_directory_sync_fault_consumed,
            DurableActiveReblitBootSyncCompletionRecord::BootSyncComplete,
        ),
        (
            arm_next_displaced_unlink_fault,
            assert_displaced_unlink_fault_consumed,
            DurableActiveReblitBootSyncCompletionRecord::BootSyncComplete,
        ),
        (
            arm_next_update_final_directory_sync_fault,
            assert_update_final_directory_sync_fault_consumed,
            DurableActiveReblitBootSyncCompletionRecord::BootSyncComplete,
        ),
    ];

    let mut exercised = 0usize;
    for (arm, assert_consumed, expected_durable) in cases {
        with_staged_alias_attempt!(
            |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, _fingerprint| {
                let promoted = promote_alias_for_completion!(
                    staged,
                    &client,
                    &plan,
                    topology_fixture.publication_root()
                );
                let pair = expected_record
                    .boot_publication_receipt_correlation()
                    .unwrap()
                    .unwrap();
                let successor = expected_record
                    .boot_sync_complete_successor(pair)
                    .unwrap();
                let database_before = fixture
                    .state_db
                    .boot_publication_receipt_state()
                    .unwrap();
                let outputs_before = publication_snapshot!(
                    &plan,
                    topology_fixture.publication_root()
                );
                reset_and_assert_no_legacy_boot_effect();
                arm();
                let assessments =
                    arm_exact_alias_assessments(topology_fixture.publication_root(), 2);

                let error = promoted.persist_boot_sync_complete(&client).unwrap_err();

                assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
                drop(assessments);
                assert_consumed();
                assert!(matches!(
                    error,
                    ActiveReblitBootSyncCompletionError::Persistence(
                        ActiveReblitBootSyncCompletePersistenceError::JournalAdvance {
                            durable,
                            ..
                        },
                    ) if durable == expected_durable,
                ));
                assert_eq!(
                    fixture.state_db.boot_publication_receipt_state().unwrap(),
                    database_before,
                );
                assert_eq!(
                    publication_snapshot!(&plan, topology_fixture.publication_root()),
                    outputs_before,
                );
                assert_no_legacy_boot_effect();
                let expected = match expected_durable {
                    DurableActiveReblitBootSyncCompletionRecord::BootSyncStarted => {
                        expected_record
                    }
                    DurableActiveReblitBootSyncCompletionRecord::BootSyncComplete => {
                        successor
                    }
                };
                assert_eq!(load_journal_record(&fixture.installation), expected);
                assert_clean_journal_inventory(&fixture.installation);
            }
        );
        exercised += 1;
    }
    assert_eq!(exercised, JOURNAL_FAULT_SCENARIO_COUNT);
}

#[test]
fn completion_reconciliation_rejects_representative_wrong_generation_record() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, _fingerprint| {
            let promoted = promote_alias_for_completion!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let mut unexpected = expected_record.clone();
            unexpected.generation += 2;
            let unexpected_bytes = encode(&unexpected).unwrap();
            let canonical = canonical_journal(&fixture.installation);
            arm_public_binding_revalidation_callback(
                PublicBindingRevalidationBoundary::BeforeBoundAdvancePublish,
                move || fs::write(&canonical, unexpected_bytes).unwrap(),
            );
            let database_before = fixture
                .state_db
                .boot_publication_receipt_state()
                .unwrap();
            let outputs_before = publication_snapshot!(
                &plan,
                topology_fixture.publication_root()
            );
            reset_and_assert_no_legacy_boot_effect();
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 2);

            let error = promoted.persist_boot_sync_complete(&client).unwrap_err();

            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert_public_binding_revalidation_callback_consumed();
            assert!(matches!(
                error,
                ActiveReblitBootSyncCompletionError::Persistence(
                    ActiveReblitBootSyncCompletePersistenceError::JournalAdvanceAndReconciliation {
                        reconciliation:
                            ActiveReblitBootSyncCompletionReconciliationError::UnexpectedRecord {
                                actual: Some(actual),
                            },
                        ..
                    },
                ) if *actual == unexpected,
            ));
            assert_eq!(
                fixture.state_db.boot_publication_receipt_state().unwrap(),
                database_before,
            );
            assert_eq!(
                publication_snapshot!(&plan, topology_fixture.publication_root()),
                outputs_before,
            );
            assert_no_legacy_boot_effect();
            assert_eq!(load_journal_record(&fixture.installation), unexpected);
            assert_clean_journal_inventory(&fixture.installation);
        }
    );
}

#[test]
fn completion_reopens_never_wait_behind_a_writer_blocked_journal_contender() {
    use std::{sync::mpsc, time::Duration};

    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, _fingerprint| {
            let promoted = promote_alias_for_completion!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let pair = expected_record
                .boot_publication_receipt_correlation()
                .unwrap()
                .unwrap();
            let successor = expected_record
                .boot_sync_complete_successor(pair)
                .unwrap();
            let root = fixture.installation.root.clone();
            let (journal_sender, journal_receiver) = mpsc::channel();
            let (writer_sender, writer_receiver) = mpsc::channel();
            let contender = std::thread::spawn(move || {
                let journal = TransitionJournalStore::open(&root).unwrap();
                journal_sender.send(()).unwrap();
                let reservation =
                    CoordinatorActiveStateReservation::acquire().unwrap();
                writer_sender.send(()).unwrap();
                drop(reservation);
                drop(journal);
            });
            arm_before_completion_journal_reopen(move || {
                journal_receiver
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
            });
            reset_and_assert_no_legacy_boot_effect();
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 2);

            let error = promoted.persist_boot_sync_complete(&client).unwrap_err();

            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert!(matches!(
                error,
                ActiveReblitBootSyncCompletionError::Persistence(
                    ActiveReblitBootSyncCompletePersistenceError::PostAdvanceValidationAndReconciliation {
                        validation: ActiveReblitBootSyncCompleteValidationError::Reopen(_),
                        reconciliation: ActiveReblitBootSyncCompletionReconciliationError::Reopen(_),
                    },
                ),
            ));
            writer_receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            contender.join().unwrap();
            assert_no_legacy_boot_effect();
            assert_eq!(load_journal_record(&fixture.installation), successor);
            assert_clean_journal_inventory(&fixture.installation);
        }
    );
}
