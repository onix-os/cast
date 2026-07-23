use super::*;

use crate::client::startup_recovery::arm_after_active_reblit_commit_cleanup_complete_same_store_before_reopen;

pub(super) const SCENARIO_COUNT: usize = 4 + finalization::SCENARIO_COUNT;

const COMPLETE_ABBA_CHILD_ENV: &str = "FORGE_ACTIVE_REBLIT_COMPLETE_ABBA_CHILD";
const COMPLETE_ABBA_EXACT_TEST: &str = concat!(
    "client::active_reblit_boot_publication_preflight::immutable_attempt::tests::",
    "receipt_promotion::completion::commit_cleanup::complete::",
    "complete_reopen_never_waits_behind_writer_blocked_journal_contender",
);

macro_rules! persist_complete_with_assessments {
    ($cleaned:expr, $client:expr, $root:expr, $count:expr) => {{
        let assessments = arm_exact_alias_assessments($root, $count);
        let result = $cleaned.persist_complete($client);
        assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
        drop(assessments);
        result
    }};
}

macro_rules! with_exact_cleanup_complete_handoff {
    (|$fixture:ident, $topology_fixture:ident, $plan:ident, $client:ident,
        $cleaned:ident| $body:block) => {{
        with_exact_commit_cleanup_handoff!(
            |$fixture, $topology_fixture, $plan, $client, committed| {
                reset_active_reblit_commit_cleanup_exchange_attempt_count();
                let $cleaned = persist_commit_cleanup_with_assessments!(
                    committed,
                    &$client,
                    $topology_fixture.publication_root(),
                    2
                )
                .unwrap();
                assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
                $body
            }
        );
    }};
}

macro_rules! with_exact_complete_handoff {
    (|$fixture:ident, $topology_fixture:ident, $plan:ident, $client:ident,
        $completed:ident| $body:block) => {{
        with_exact_cleanup_complete_handoff!(
            |$fixture, $topology_fixture, $plan, $client, cleaned| {
                let $completed = persist_complete_with_assessments!(
                    cleaned,
                    &$client,
                    $topology_fixture.publication_root(),
                    2
                )
                .unwrap();
                $body
            }
        );
    }};
}

#[test]
fn exact_cleanup_complete_rolls_forward_once_and_preserves_all_authority() {
    with_exact_cleanup_complete_handoff!(
        |fixture, topology_fixture, plan, client, cleaned| {
            let source = cleaned.record().clone();
            let expected = source.forward_successor(None).unwrap();
            let fingerprint = cleaned.receipt_fingerprint();
            let database_outcome = cleaned.database_outcome();
            let staging_outcome = cleaned.staging_outcome();
            let inventory = cleaned.inventory() as *const _;
            let publication_count = cleaned.publication_count();
            let published_count = cleaned.published_count();
            let already_exact_count = cleaned.already_exact_count();
            let replaced_count = cleaned.replaced_count();
            let evidence = evidence_snapshot(cleaned.evidence());
            let database_before = fixture.state_db.boot_publication_receipt_state().unwrap();
            let outputs_before =
                publication_snapshot!(&plan, topology_fixture.publication_root());

            let completed = persist_complete_with_assessments!(
                cleaned,
                &client,
                topology_fixture.publication_root(),
                2
            )
            .unwrap();

            assert_eq!(source.phase, Phase::CommitCleanupComplete);
            assert_eq!(source.generation, 14);
            assert_eq!(completed.record(), &expected);
            assert_eq!(completed.record().phase, Phase::Complete);
            assert_eq!(completed.record().generation, 15);
            assert!(!completed.record().options.archive_previous);
            assert!(completed.record().options.run_system_triggers);
            assert!(completed.record().options.run_boot_sync);
            assert_eq!(completed.receipt_fingerprint(), fingerprint);
            assert_eq!(completed.database_outcome(), database_outcome);
            assert_eq!(completed.staging_outcome(), staging_outcome);
            assert!(std::ptr::eq(completed.inventory(), inventory));
            assert_eq!(completed.publication_count(), publication_count);
            assert_eq!(completed.published_count(), published_count);
            assert_eq!(completed.already_exact_count(), already_exact_count);
            assert_eq!(completed.replaced_count(), replaced_count);
            assert_eq!(evidence_snapshot(completed.evidence()), evidence);
            assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
            assert_eq!(
                fixture.state_db.boot_publication_receipt_state().unwrap(),
                database_before,
            );
            assert_eq!(
                publication_snapshot!(&plan, topology_fixture.publication_root()),
                outputs_before,
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
            reached_receiver.recv_timeout(Duration::from_secs(120)).unwrap();
            assert!(matches!(
                acquired_receiver.recv_timeout(Duration::from_millis(100)),
                Err(RecvTimeoutError::Timeout),
            ));
            drop(completed);
            acquired_receiver.recv_timeout(Duration::from_secs(120)).unwrap();
            contender.join().unwrap();
            assert_eq!(load_journal_record(&fixture.installation), expected);
        }
    );
}

#[test]
fn same_bytes_new_inode_rejects_complete_without_any_effect() {
    with_exact_cleanup_complete_handoff!(
        |fixture, topology_fixture, plan, client, cleaned| {
            let source = cleaned.record().clone();
            let namespace_before =
                snapshot_startup_recovery_namespace(&fixture.installation.root);
            let database_before = fixture.state_db.boot_publication_receipt_state().unwrap();
            let outputs_before =
                publication_snapshot!(&plan, topology_fixture.publication_root());
            let canonical = canonical_journal(&fixture.installation);
            let replacement = canonical.with_extension("complete-same-bytes-new-inode");
            fs::write(&replacement, encode(&source).unwrap()).unwrap();
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).unwrap();
            fs::rename(replacement, canonical).unwrap();

            assert!(cleaned.persist_complete(&client).is_err());
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
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
            assert_eq!(load_journal_record(&fixture.installation), source);
        }
    );
}

#[test]
fn uncertain_complete_advance_returns_no_handoff() {
    with_exact_cleanup_complete_handoff!(
        |fixture, topology_fixture, plan, client, cleaned| {
            let expected = cleaned.record().forward_successor(None).unwrap();
            let database_before = fixture.state_db.boot_publication_receipt_state().unwrap();
            let outputs_before =
                publication_snapshot!(&plan, topology_fixture.publication_root());
            arm_next_update_first_directory_sync_fault();

            assert!(persist_complete_with_assessments!(
                cleaned,
                &client,
                topology_fixture.publication_root(),
                1
            )
            .is_err());

            assert_update_first_directory_sync_fault_consumed();
            assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
            assert_eq!(
                fixture.state_db.boot_publication_receipt_state().unwrap(),
                database_before,
            );
            assert_eq!(
                publication_snapshot!(&plan, topology_fixture.publication_root()),
                outputs_before,
            );
            assert_eq!(load_journal_record(&fixture.installation), expected);
        }
    );
}

#[test]
fn complete_reopen_never_waits_behind_writer_blocked_journal_contender() {
    if env::var_os(COMPLETE_ABBA_CHILD_ENV).is_none() {
        let child = Command::new(env::current_exe().unwrap())
            .arg(COMPLETE_ABBA_EXACT_TEST)
            .arg("--exact")
            .arg("--test-threads=1")
            .arg("--include-ignored")
            .env(COMPLETE_ABBA_CHILD_ENV, "1")
            .spawn()
            .unwrap();
        assert!(wait_for_abba_child(child).success());
        return;
    }

    with_exact_cleanup_complete_handoff!(
        |fixture, topology_fixture, _plan, client, cleaned| {
            let expected = cleaned.record().forward_successor(None).unwrap();
            let root = fixture.installation.root.clone();
            let (journal_sender, journal_receiver) = mpsc::channel();
            let (writer_sender, writer_receiver) = mpsc::channel();
            let contender = thread::spawn(move || {
                let journal = TransitionJournalStore::open(&root).unwrap();
                journal_sender.send(()).unwrap();
                let reservation = CoordinatorActiveStateReservation::acquire().unwrap();
                writer_sender.send(()).unwrap();
                drop(reservation);
                drop(journal);
            });
            arm_after_active_reblit_commit_cleanup_complete_same_store_before_reopen(move || {
                journal_receiver.recv_timeout(Duration::from_secs(120)).unwrap();
            });

            assert!(persist_complete_with_assessments!(
                cleaned,
                &client,
                topology_fixture.publication_root(),
                1
            )
            .is_err());

            assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
            writer_receiver.recv_timeout(Duration::from_secs(120)).unwrap();
            contender.join().unwrap();
            assert_eq!(load_journal_record(&fixture.installation), expected);
        }
    );
}

#[path = "complete/finalization.rs"]
mod finalization;
