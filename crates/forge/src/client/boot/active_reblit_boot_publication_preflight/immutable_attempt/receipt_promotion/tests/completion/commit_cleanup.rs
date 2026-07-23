use super::*;

use std::{
    env,
    process::{Child, Command, ExitStatus},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use crate::{
    client::{
        CoordinatorActiveStateReservation,
        snapshot_startup_recovery_namespace,
        active_reblit_boot_sync_staging::{
            CoordinatorActiveReblitBootSyncHandoff,
            stage_active_reblit_boot_sync_from_handoff_for_test,
        },
        startup_reconciliation::{
            active_reblit_commit_cleanup_exchange_attempt_count,
            reset_active_reblit_commit_cleanup_exchange_attempt_count,
        },
        startup_recovery::arm_after_active_reblit_commit_cleanup_same_store_check_before_reopen,
    },
    transition_journal::{
        TransitionJournalStore, arm_next_update_first_directory_sync_fault,
        assert_update_first_directory_sync_fault_consumed, encode,
    },
};

const SUCCESS_SCENARIO_COUNT: usize = 1;
const REJECTION_SCENARIO_COUNT: usize = 1;
const UNCERTAIN_STORAGE_SCENARIO_COUNT: usize = 1;
const REOPEN_CONTENTION_SCENARIO_COUNT: usize = 1;
pub(super) const SCENARIO_COUNT: usize = SUCCESS_SCENARIO_COUNT
    + REJECTION_SCENARIO_COUNT
    + UNCERTAIN_STORAGE_SCENARIO_COUNT
    + REOPEN_CONTENTION_SCENARIO_COUNT
    + complete::SCENARIO_COUNT;

const ABBA_CHILD_ENV: &str = "FORGE_ACTIVE_REBLIT_COMMIT_CLEANUP_ABBA_CHILD";
const ABBA_EXACT_TEST: &str = concat!(
    "client::active_reblit_boot_publication_preflight::immutable_attempt::tests::",
    "receipt_promotion::completion::commit_cleanup::",
    "commit_cleanup_reopen_never_waits_behind_writer_blocked_journal_contender",
);
const ABBA_CHILD_DEADLINE: Duration = Duration::from_secs(20);

fn wait_for_abba_child(mut child: Child) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if started.elapsed() >= ABBA_CHILD_DEADLINE {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("commit-cleanup ABBA child exceeded its runtime deadline");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

macro_rules! with_exact_commit_cleanup_handoff {
    (|$fixture:ident, $topology_fixture:ident, $plan:ident, $client:ident,
        $committed:ident| $body:block) => {{
        let deadline = render_support::future_deadline();
        let $fixture = support::commit_decision_fixture();
        let $client = support::staging_client(&$fixture, $fixture.state_db.clone());
        let stone = match PreparedActiveReblitStoneBootInputs::prepare_until(
            &$client.installation,
            &$fixture.state_db,
            &$fixture.layout_db,
            &$fixture.head,
            deadline,
        )
        .unwrap()
        {
            ActiveReblitStoneBootInputsOutcome::Ready(stone) => stone,
            ActiveReblitStoneBootInputsOutcome::NotApplicable(reason) => {
                panic!("commit-cleanup fixture must be bootable: {reason:?}")
            }
        };
        let roots = PreparedActiveReblitBootStateRoots::prepare_until(
            &$client.installation,
            &$fixture.head_usr,
            $fixture.head.id,
            stone.state_ids(),
            deadline,
        )
        .unwrap();
        let prepared = render_support::prepare_static(&$fixture, &stone, &roots);
        let local_policy = PreparedActiveReblitLocalBootPolicy::prepare_until(
            &$client.installation,
            deadline,
        )
        .unwrap();
        let root_intent = PreparedActiveReblitRootFilesystemIntent::prepare_until(
            &$client.installation,
            deadline,
        )
        .unwrap();
        let inputs = prepared
            .revalidate_until(
                &$fixture.state_db,
                &$fixture.layout_db,
                &$client.installation,
                &local_policy,
                &root_intent,
                deadline,
            )
            .unwrap();
        let $topology_fixture =
            AliasFixture::stable().expect("commit-cleanup topology fixture must prepare");
        support::set_safe_directory($topology_fixture.publication_root());
        let topology_prepared = $topology_fixture
            .prepare_for_installation_until(&$client.installation, deadline)
            .unwrap();
        let topology = topology_prepared
            .revalidate_until(&$client.installation, deadline)
            .unwrap();
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let $plan = rendered.into_publication_plan(&topology).unwrap();
        let inventory = $plan.prepare_desired_publication_inventory().unwrap();
        let (journal, predecessor, binding) =
            support::exact_boot_sync_journal_for_commit_decision_route(
                &$client.installation,
                $fixture.head.id,
                true,
                0,
            );
        let handoff = CoordinatorActiveReblitBootSyncHandoff::from_parts_for_test(
            predecessor,
            binding,
            journal,
            $fixture.state_db.clone(),
            $fixture.installation.clone(),
            $fixture.head.clone(),
            CoordinatorActiveStateReservation::acquire().unwrap(),
        );
        let staging_preflight = arm_fixture_boot_namespace_assessments([
            FixtureBootNamespaceAssessment::new(
                BootTargetRole::Esp,
                $topology_fixture.publication_root().to_owned(),
            ),
        ]);
        let staged = stage_active_reblit_boot_sync_from_handoff_for_test(
            &$client,
            &$plan,
            &inventory,
            handoff,
        )
        .unwrap();
        drop(staging_preflight);
        let promoted = promote_alias_for_completion!(
            staged,
            &$client,
            &$plan,
            $topology_fixture.publication_root()
        );
        let completion_assessments =
            arm_exact_alias_assessments($topology_fixture.publication_root(), 4);
        let completed = promoted.persist_boot_sync_complete(&$client).unwrap();
        assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
        drop(completion_assessments);
        let decision_assessments =
            arm_exact_alias_assessments($topology_fixture.publication_root(), 4);
        let $committed = completed.persist_commit_decided(&$client).unwrap();
        assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
        drop(decision_assessments);
        $body
        $topology_fixture.assert_outside_unchanged();
    }};
}

macro_rules! persist_commit_cleanup_with_assessments {
    ($committed:expr, $client:expr, $root:expr, $count:expr) => {{
        let assessments = arm_exact_alias_assessments($root, $count);
        let result = $committed.persist_commit_cleanup_complete($client);
        assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
        drop(assessments);
        result
    }};
}

#[test]
fn exact_commit_decision_applies_cleanup_once_and_retains_writer_authority() {
    with_exact_commit_cleanup_handoff!(
        |fixture, topology_fixture, plan, client, committed| {
            let source = committed.record().clone();
            let expected = source.forward_successor(None).unwrap();
            let fingerprint = committed.receipt_fingerprint();
            let database_outcome = committed.database_outcome();
            let publication_count = committed.publication_count();
            let published_count = committed.published_count();
            let already_exact_count = committed.already_exact_count();
            let replaced_count = committed.replaced_count();
            let evidence = evidence_snapshot(committed.evidence());
            let database_before = fixture.state_db.boot_publication_receipt_state().unwrap();
            let outputs_before =
                publication_snapshot!(&plan, topology_fixture.publication_root());
            reset_active_reblit_commit_cleanup_exchange_attempt_count();

            let cleaned = persist_commit_cleanup_with_assessments!(
                committed,
                &client,
                topology_fixture.publication_root(),
                2
            )
            .unwrap();

            assert_eq!(source.phase, Phase::CommitDecided);
            assert_eq!(source.generation, 13);
            assert_eq!(cleaned.record(), &expected);
            assert_eq!(cleaned.record().phase, Phase::CommitCleanupComplete);
            assert_eq!(cleaned.record().generation, 14);
            assert!(!cleaned.record().options.archive_previous);
            assert!(cleaned.record().options.run_system_triggers);
            assert!(cleaned.record().options.run_boot_sync);
            assert_eq!(cleaned.receipt_fingerprint(), fingerprint);
            assert_eq!(cleaned.database_outcome(), database_outcome);
            assert_eq!(cleaned.publication_count(), publication_count);
            assert_eq!(cleaned.published_count(), published_count);
            assert_eq!(cleaned.already_exact_count(), already_exact_count);
            assert_eq!(cleaned.replaced_count(), replaced_count);
            assert_eq!(evidence_snapshot(cleaned.evidence()), evidence);
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
            drop(cleaned);
            acquired_receiver.recv_timeout(Duration::from_secs(120)).unwrap();
            contender.join().unwrap();
            assert_eq!(load_journal_record(&fixture.installation), expected);
        }
    );
}

#[test]
fn replaced_binding_and_uncertain_advance_return_no_cleanup_handoff() {
    with_exact_commit_cleanup_handoff!(
        |fixture, topology_fixture, _plan, client, committed| {
            let source = committed.record().clone();
            let namespace_before =
                snapshot_startup_recovery_namespace(&fixture.installation.root);
            reset_active_reblit_commit_cleanup_exchange_attempt_count();
            let canonical = canonical_journal(&fixture.installation);
            let replacement = canonical.with_extension("same-bytes-new-inode");
            fs::write(&replacement, encode(&source).unwrap()).unwrap();
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).unwrap();
            fs::rename(replacement, canonical).unwrap();

            assert!(committed.persist_commit_cleanup_complete(&client).is_err());
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 0);
            assert_eq!(
                snapshot_startup_recovery_namespace(&fixture.installation.root),
                namespace_before,
            );
            assert_eq!(load_journal_record(&fixture.installation), source);
        }
    );

    with_exact_commit_cleanup_handoff!(
        |fixture, topology_fixture, _plan, client, committed| {
            let expected = committed.record().forward_successor(None).unwrap();
            reset_active_reblit_commit_cleanup_exchange_attempt_count();
            arm_next_update_first_directory_sync_fault();

            assert!(persist_commit_cleanup_with_assessments!(
                committed,
                &client,
                topology_fixture.publication_root(),
                1
            )
            .is_err());

            assert_update_first_directory_sync_fault_consumed();
            assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
            assert_eq!(load_journal_record(&fixture.installation), expected);
        }
    );
}

#[test]
fn commit_cleanup_reopen_never_waits_behind_writer_blocked_journal_contender() {
    if env::var_os(ABBA_CHILD_ENV).is_none() {
        let child = Command::new(env::current_exe().unwrap())
            .arg(ABBA_EXACT_TEST)
            .arg("--exact")
            .arg("--test-threads=1")
            .arg("--include-ignored")
            .env(ABBA_CHILD_ENV, "1")
            .spawn()
            .unwrap();
        assert!(wait_for_abba_child(child).success());
        return;
    }

    with_exact_commit_cleanup_handoff!(
        |fixture, topology_fixture, _plan, client, committed| {
            let expected = committed.record().forward_successor(None).unwrap();
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
            arm_after_active_reblit_commit_cleanup_same_store_check_before_reopen(move || {
                journal_receiver.recv_timeout(Duration::from_secs(120)).unwrap();
            });

            assert!(persist_commit_cleanup_with_assessments!(
                committed,
                &client,
                topology_fixture.publication_root(),
                1
            )
            .is_err());

            writer_receiver.recv_timeout(Duration::from_secs(120)).unwrap();
            contender.join().unwrap();
            assert_eq!(load_journal_record(&fixture.installation), expected);
        }
    );
}

#[path = "commit_cleanup/complete.rs"]
mod complete;
