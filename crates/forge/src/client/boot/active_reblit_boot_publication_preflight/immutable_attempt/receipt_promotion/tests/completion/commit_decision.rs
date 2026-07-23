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
        active_reblit_boot_sync_staging::{
            CoordinatorActiveReblitBootSyncHandoff,
            stage_active_reblit_boot_sync_from_handoff_for_test,
        },
        startup_reconciliation::arm_after_active_reblit_boot_commit_decision_bound_terminal_validation,
        startup_recovery::arm_after_active_reblit_boot_sync_commit_decision_same_store_check_before_reopen,
        startup_recovery::arm_before_active_reblit_boot_sync_commit_decision_final_revalidation,
    },
    transition_journal::{
        arm_next_update_first_directory_sync_fault,
        assert_update_first_directory_sync_fault_consumed,
    },
};

const SUCCESS_SCENARIO_COUNT: usize = 1;
const REJECTION_SCENARIO_COUNT: usize = 5;
const INNER_BOUNDARY_SCENARIO_COUNT: usize = 2;
const AUTHORITY_SANDWICH_SCENARIO_COUNT: usize = 1;
const UNCERTAIN_STORAGE_SCENARIO_COUNT: usize = 1;
const REOPEN_CONTENTION_SCENARIO_COUNT: usize = 1;
pub(super) const SCENARIO_COUNT: usize = SUCCESS_SCENARIO_COUNT
    + REJECTION_SCENARIO_COUNT
    + INNER_BOUNDARY_SCENARIO_COUNT
    + AUTHORITY_SANDWICH_SCENARIO_COUNT
    + UNCERTAIN_STORAGE_SCENARIO_COUNT
    + REOPEN_CONTENTION_SCENARIO_COUNT;

const ABBA_CHILD_ENV: &str = "FORGE_ACTIVE_REBLIT_COMMIT_DECISION_ABBA_CHILD";
const ABBA_EXACT_TEST: &str = concat!(
    "client::active_reblit_boot_publication_preflight::immutable_attempt::tests::",
    "receipt_promotion::completion::commit_decision::",
    "commit_decision_reopen_never_waits_behind_writer_blocked_journal_contender",
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
            panic!("commit-decision ABBA child exceeded its runtime deadline");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

macro_rules! with_exact_commit_decision_completion {
    (|$fixture:ident, $topology_fixture:ident, $plan:ident, $client:ident,
        $completed:ident| $body:block) => {{
        with_exact_commit_decision_completion!(
            @route true, 0,
            |$fixture, $topology_fixture, $plan, $client, $completed| $body
        )
    }};
    (@route $run_system_triggers:expr, $pre_boot_generation_offset:expr,
        |$fixture:ident, $topology_fixture:ident, $plan:ident, $client:ident,
        $completed:ident| $body:block) => {{
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
                panic!("commit-decision fixture must be bootable: {reason:?}")
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
            AliasFixture::stable().expect("commit-decision topology fixture must prepare");
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
                $run_system_triggers,
                $pre_boot_generation_offset,
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
        let $completed = promoted.persist_boot_sync_complete(&$client).unwrap();
        assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
        drop(completion_assessments);
        $body
        $topology_fixture.assert_outside_unchanged();
    }};
}

macro_rules! persist_commit_decision_with_assessments {
    ($completed:expr, $client:expr, $root:expr, $count:expr) => {{
        let assessments = arm_exact_alias_assessments($root, $count);
        let result = $completed.persist_commit_decided($client);
        assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
        drop(assessments);
        result
    }};
}

#[test]
fn exact_completion_persists_one_commit_decision_and_retains_writer_authority() {
    with_exact_commit_decision_completion!(
        |fixture, topology_fixture, plan, client, completed| {
            let source = completed.record().clone();
            let expected = source.forward_successor(None).unwrap();
            let fingerprint = completed.receipt_fingerprint();
            let database_before = fixture.state_db.boot_publication_receipt_state().unwrap();
            let outputs_before = publication_snapshot!(
                &plan,
                topology_fixture.publication_root()
            );

            assert_eq!(source.generation, 12);
            assert_eq!(expected.generation, 13);
            assert!(!source.options.archive_previous);
            assert!(source.options.run_system_triggers);
            assert!(source.options.run_boot_sync);

            let committed = persist_commit_decision_with_assessments!(
                completed,
                &client,
                topology_fixture.publication_root(),
                4
            )
            .unwrap();

            assert_eq!(committed.record(), &expected);
            assert_eq!(committed.record().phase, Phase::CommitDecided);
            assert_eq!(committed.record().generation, 13);
            assert!(!committed.record().options.archive_previous);
            assert!(committed.record().options.run_system_triggers);
            assert!(committed.record().options.run_boot_sync);
            assert_eq!(committed.receipt_fingerprint(), fingerprint);
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
            reached_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(matches!(
                acquired_receiver.recv_timeout(Duration::from_millis(100)),
                Err(RecvTimeoutError::Timeout),
            ));
            drop(committed);
            acquired_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            contender.join().unwrap();
            assert_eq!(load_journal_record(&fixture.installation), expected);
        }
    );

}

#[test]
fn wrong_client_state_and_record_are_rejected_before_commit_decision() {
    with_exact_commit_decision_completion!(
        |fixture, topology_fixture, _plan, client, completed| {
            let source = completed.record().clone();
            let wrong_client = support::staging_client(
                &fixture,
                db::state::Database::new(":memory:").unwrap(),
            );
            assert!(completed.persist_commit_decided(&wrong_client).is_err());
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            assert_eq!(load_journal_record(&client.installation), source);
        }
    );

    with_exact_commit_decision_completion!(
        |fixture, topology_fixture, _plan, client, completed| {
            let source = completed.record().clone();
            let state_id = fixture.installation.root.join("usr/.stateID");
            arm_after_active_reblit_commit_decision_terminal_validation(move || {
                fs::write(state_id, "999").unwrap();
            });
            assert!(persist_commit_decision_with_assessments!(
                completed,
                &client,
                topology_fixture.publication_root(),
                2
            )
            .is_err());
            assert_after_active_reblit_commit_decision_terminal_validation_hook_consumed();
            assert_eq!(load_journal_record(&fixture.installation), source);
        }
    );

    with_exact_commit_decision_completion!(
        |fixture, topology_fixture, _plan, client, completed| {
            let mut wrong = completed.record().clone();
            wrong.generation += 2;
            let canonical = canonical_journal(&fixture.installation);
            let wrong_bytes = encode(&wrong).unwrap();
            arm_after_active_reblit_commit_decision_terminal_validation(move || {
                fs::write(canonical, wrong_bytes).unwrap();
            });
            assert!(persist_commit_decision_with_assessments!(
                completed,
                &client,
                topology_fixture.publication_root(),
                1
            )
            .is_err());
            assert_after_active_reblit_commit_decision_terminal_validation_hook_consumed();
            assert_eq!(load_journal_record(&fixture.installation), wrong);
            assert_ne!(wrong.phase, Phase::CommitDecided);
        }
    );

    with_exact_commit_decision_completion!(
        @route true, 1,
        |fixture, topology_fixture, _plan, client, completed| {
            let source = completed.record().clone();
            assert_eq!(source.phase, Phase::BootSyncComplete);
            assert_eq!(source.generation, 13);
            assert!(!source.options.archive_previous);
            assert!(source.options.run_system_triggers);
            assert!(source.options.run_boot_sync);

            assert!(persist_commit_decision_with_assessments!(
                completed,
                &client,
                topology_fixture.publication_root(),
                2
            )
            .is_err());
            assert_eq!(load_journal_record(&fixture.installation), source);
        }
    );

    with_exact_commit_decision_completion!(
        @route false, 2,
        |fixture, topology_fixture, _plan, client, completed| {
            let source = completed.record().clone();
            assert_eq!(source.phase, Phase::BootSyncComplete);
            assert_eq!(source.generation, 12);
            assert!(!source.options.archive_previous);
            assert!(!source.options.run_system_triggers);
            assert!(source.options.run_boot_sync);

            assert!(persist_commit_decision_with_assessments!(
                completed,
                &client,
                topology_fixture.publication_root(),
                2
            )
            .is_err());
            assert_eq!(load_journal_record(&fixture.installation), source);
        }
    );
}

#[test]
fn inner_output_drift_and_deadline_expiry_never_reach_commit_decision() {
    with_exact_commit_decision_completion!(
        |fixture, topology_fixture, plan, client, completed| {
            let source = completed.record().clone();
            let leaf = topology_fixture
                .publication_root()
                .join(plan.outputs().next().unwrap().relative_path());
            arm_before_active_reblit_boot_sync_commit_decision_final_revalidation(
                move || fs::remove_file(leaf).unwrap(),
            );

            assert!(persist_commit_decision_with_assessments!(
                completed,
                &client,
                topology_fixture.publication_root(),
                3
            )
            .is_err());

            assert_eq!(load_journal_record(&fixture.installation), source);
        }
    );

    with_exact_commit_decision_completion!(
        |fixture, topology_fixture, _plan, client, completed| {
            let source = completed.record().clone();
            arm_before_active_reblit_boot_sync_commit_decision_final_revalidation(
                arm_expired_deadline,
            );

            assert!(persist_commit_decision_with_assessments!(
                completed,
                &client,
                topology_fixture.publication_root(),
                2
            )
            .is_err());

            assert_eq!(load_journal_record(&fixture.installation), source);
        }
    );
}

#[test]
fn bound_terminal_validation_is_sandwiched_by_authority_revalidation() {
    with_exact_commit_decision_completion!(
        |fixture, topology_fixture, _plan, client, completed| {
            let source = completed.record().clone();
            let database = fixture.state_db.clone();
            let candidate = fixture.head.id;
            arm_after_active_reblit_boot_commit_decision_bound_terminal_validation(
                move || {
                    database
                        .change_summary_for_test(
                            candidate,
                            Some("changed after bound terminal validation"),
                        )
                        .unwrap();
                },
            );

            assert!(persist_commit_decision_with_assessments!(
                completed,
                &client,
                topology_fixture.publication_root(),
                3
            )
            .is_err());
            assert_eq!(load_journal_record(&fixture.installation), source);
        }
    );
}

#[test]
fn uncertain_commit_decision_fault_returns_no_handoff() {
    with_exact_commit_decision_completion!(
        |fixture, topology_fixture, _plan, client, completed| {
            let expected = completed.record().forward_successor(None).unwrap();
            arm_next_update_first_directory_sync_fault();

            assert!(persist_commit_decision_with_assessments!(
                completed,
                &client,
                topology_fixture.publication_root(),
                3
            )
            .is_err());

            assert_update_first_directory_sync_fault_consumed();
            assert_eq!(load_journal_record(&fixture.installation), expected);
        }
    );
}

#[test]
fn commit_decision_reopen_never_waits_behind_writer_blocked_journal_contender() {
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

    with_exact_commit_decision_completion!(
        |fixture, topology_fixture, _plan, client, completed| {
            let expected = completed.record().forward_successor(None).unwrap();
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
            arm_after_active_reblit_boot_sync_commit_decision_same_store_check_before_reopen(
                move || {
                    journal_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
                },
            );

            assert!(persist_commit_decision_with_assessments!(
                completed,
                &client,
                topology_fixture.publication_root(),
                3
            )
            .is_err());

            writer_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            contender.join().unwrap();
            assert_eq!(load_journal_record(&fixture.installation), expected);
        }
    );
}
