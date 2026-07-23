//! RootLinks-only same-boot process-death proof for generation-18 finalization.

use std::{fs, path::Path};

use crate::{
    Installation, State,
    client::{
        MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal,
        active_state_snapshot::ActiveStateReservation,
        boot::{boot_synchronize_attempt_count, reset_boot_synchronize_attempt_count},
        snapshot_startup_recovery_namespace,
        startup_reconciliation::{
            active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
        },
        startup_gate::{
            CleanSystemStartup,
            root_links_terminal_process_harness::{
                ChildInvocation, ControlDirectory, JournalExpectation, ParentCase, ProcessEpoch, ProcessRole,
                RawRecordState, RootLinksDeleteScenario, RootLinksSnapshot, TerminalOperation,
                assert_clean_holds_journal_lock, assert_clean_store_reopens, forbid_journal_update, kill_self,
            },
        },
        startup_recovery::arm_before_usr_rollback_finalization_final_revalidation,
    },
    db,
    state::Id,
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{ForwardPhase, Operation, Phase, RollbackAction, RollbackActionOutcome, TransitionRecord},
};

use super::{
    super::candidate_test_support::CandidateSource,
    candidate_move_process_harness::assert_preserved_topology,
    support::{
        CandidateOutcome, Epoch, FreshOutcome, build_fresh_invalidation, effect_counts,
        install_persistent_joint_absence_database, open_layout_database, persist_fresh_invalidated,
        persist_rollback_complete, release_invalidation_fixture_handles, reopen_persistent_state_database,
        reset_namespace_effect_counts,
    },
};

const TEST_NAME: &str = concat!(
    "client::startup_gate::usr_rollback_new_state::tests::root_links_terminal_process_kill::",
    "startup_new_state_root_links_terminal_delete_process_kills_restart_cleanly",
);
const OPERATION: TerminalOperation = TerminalOperation::NewState;

#[derive(Debug, Eq, PartialEq)]
struct NewStateDatabaseEvidence {
    states: Vec<State>,
    in_flight: Option<db::state::InFlightTransition>,
}

impl NewStateDatabaseEvidence {
    fn capture(database: &db::state::Database, record: &TransitionRecord) -> Self {
        let candidate = Id::from(record.candidate.id.unwrap());
        let in_flight = database.audit_in_flight_transition().unwrap();
        assert_eq!(in_flight, None);
        assert!(matches!(
            database.inspect_exact_fresh_transition(candidate, &record.transition_id),
            Ok(db::state::ExactFreshTransitionObservation::JointlyAbsent(_))
        ));
        Self {
            states: database.all().unwrap(),
            in_flight,
        }
    }
}

#[test]
fn startup_new_state_root_links_terminal_delete_process_kills_restart_cleanly() {
    let Some(case) = ChildInvocation::from_environment(OPERATION) else {
        run_parent();
        return;
    };
    run_child(case);
}

fn run_parent() {
    let mut cases = 0;
    for epoch in ProcessEpoch::ALL {
        for scenario in RootLinksDeleteScenario::ALL {
            run_parent_case(epoch, scenario);
            cases += 1;
        }
    }
    assert_eq!(cases, 12, "NewState RootLinks matrix must remain exactly 2 x 6");
}

fn run_parent_case(epoch: ProcessEpoch, scenario: RootLinksDeleteScenario) {
    let fixture_epoch = match epoch {
        ProcessEpoch::Current => Epoch::Current,
        ProcessEpoch::Historical => Epoch::Historical,
    };
    let usr_outcome = epoch_outcome(epoch);
    let mut fixture = build_fresh_invalidation(
        fixture_epoch,
        CandidateSource::RootLinksComplete,
        usr_outcome,
        candidate_outcome(epoch),
        FreshOutcome::AlreadySatisfied,
    );
    let invalidated = persist_fresh_invalidated(&fixture, fresh_outcome(epoch));
    let terminal = persist_rollback_complete(&fixture, &invalidated);
    assert_exact_terminal(&terminal, epoch);
    install_persistent_joint_absence_database(&mut fixture);
    drop(open_layout_database(&fixture.fixture.fixture.installation));

    let root = fs::canonicalize(&fixture.fixture.fixture.installation.root).unwrap();
    let journal = JournalExpectation::capture(&root, &terminal);
    let root_links = RootLinksSnapshot::capture(&root);
    let namespace = snapshot_startup_recovery_namespace(&root);
    let database = NewStateDatabaseEvidence::capture(&fixture.fixture.fixture.database, &terminal);
    assert_preserved_topology(&root, &terminal);
    let control = ControlDirectory::new(
        &root,
        OPERATION,
        epoch,
        scenario,
        &terminal,
        &control_dimensions(epoch),
        &journal,
    );
    let control_path = control.path();
    let retained_root = release_invalidation_fixture_handles(fixture);

    ParentCase {
        test_name: TEST_NAME,
        operation: OPERATION,
        epoch,
        scenario,
        root: &root,
        control: &control_path,
    }
    .run(&journal, &root_links, || {
        assert_eq!(capture_database_at_root(&root, &terminal), database);
        assert_eq!(snapshot_startup_recovery_namespace(&root), namespace);
        assert_preserved_topology(&root, &terminal);
    });

    drop(retained_root);
    drop(control);
}

fn run_child(case: ChildInvocation) {
    let terminal = case.terminal_record(&control_dimensions(case.epoch));
    assert_exact_terminal(&terminal, case.epoch);
    let journal = case.journal_expectation();
    journal.assert_raw(&case.root, case.expected_entry_state());
    let root_links = RootLinksSnapshot::capture(&case.root);
    assert_preserved_topology(&case.root, &terminal);

    let installation = Installation::open(&case.root, None).unwrap();
    assert_eq!(installation.root, case.root);
    let database = reopen_persistent_state_database(&installation);
    let layout_database = open_layout_database(&installation);
    NewStateDatabaseEvidence::capture(&database, &terminal);
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation,
        database,
        layout_database,
    );
    match case.role {
        ProcessRole::InitialCrash => run_initial_crash(&case, &system),
        ProcessRole::RecoveryCrash => run_recovery_crash(&case, &system),
        ProcessRole::FinalRecover => run_final_recovery(&case, &system, &terminal, &journal, &root_links),
    }
}

fn run_initial_crash(case: &ChildInvocation, system: &MutableSystemCapabilities) {
    reset_zero_effect_observers();
    assert_zero_effects();
    forbid_journal_update("NewState RootLinks terminal crash");
    if !case.scenario.arm_initial_bound_delete_kill(kill_after_zero_effects) {
        arm_before_usr_rollback_finalization_final_revalidation(kill_after_zero_effects);
    }
    let reservation = ActiveStateReservation::acquire().unwrap();
    let result = CleanSystemStartup::enter(system, &reservation);
    panic!(
        "initial NewState RootLinks crash escaped {:?}: success={} error={:?}",
        case.scenario,
        result.is_ok(),
        result.err(),
    );
}

fn run_recovery_crash(case: &ChildInvocation, system: &MutableSystemCapabilities) {
    reset_zero_effect_observers();
    assert_zero_effects();
    forbid_journal_update("NewState RootLinks residue recovery");
    case.scenario.arm_recovery_kill(kill_after_zero_effects);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let result = CleanSystemStartup::enter(system, &reservation);
    panic!(
        "NewState RootLinks residue recovery escaped {:?}: success={} error={:?}",
        case.scenario,
        result.is_ok(),
        result.err(),
    );
}

fn run_final_recovery(
    case: &ChildInvocation,
    system: &MutableSystemCapabilities,
    terminal: &TransitionRecord,
    journal: &JournalExpectation,
    root_links: &RootLinksSnapshot,
) {
    let database_before = NewStateDatabaseEvidence::capture(system.state_db(), terminal);
    let namespace_before = snapshot_startup_recovery_namespace(&case.root);
    reset_zero_effect_observers();
    assert_zero_effects();
    forbid_journal_update("NewState RootLinks final recovery");

    {
        let reservation = ActiveStateReservation::acquire().unwrap();
        let clean = CleanSystemStartup::enter(system, &reservation)
            .unwrap_or_else(|error| panic!("NewState RootLinks recovery did not reach clean: {error:?}"));
        assert_zero_effects();
        assert_clean_holds_journal_lock(system.installation(), &case.root);
        drop(clean);
    }
    journal.assert_raw(&case.root, RawRecordState::Absent);
    assert_clean_store_reopens(system.installation(), &case.root);

    {
        let reservation = ActiveStateReservation::acquire().unwrap();
        let clean_again = CleanSystemStartup::enter(system, &reservation)
            .unwrap_or_else(|error| panic!("NewState RootLinks clean endpoint did not remain clean: {error:?}"));
        assert_clean_holds_journal_lock(system.installation(), &case.root);
        drop(clean_again);
    }

    assert_zero_effects();
    assert_eq!(NewStateDatabaseEvidence::capture(system.state_db(), terminal), database_before);
    assert_eq!(snapshot_startup_recovery_namespace(&case.root), namespace_before);
    root_links.assert_unchanged(&case.root);
    assert_preserved_topology(&case.root, terminal);
}

fn capture_database_at_root(root: &Path, terminal: &TransitionRecord) -> NewStateDatabaseEvidence {
    let installation = Installation::open(root, None).unwrap();
    let database = reopen_persistent_state_database(&installation);
    NewStateDatabaseEvidence::capture(&database, terminal)
}

fn assert_exact_terminal(record: &TransitionRecord, epoch: ProcessEpoch) {
    assert_eq!(record.operation, Operation::NewState);
    assert_eq!(record.phase, Phase::RollbackComplete);
    assert_eq!(record.generation, 18);
    let rollback = record.rollback.as_ref().unwrap();
    assert_eq!(rollback.source, ForwardPhase::RootLinksComplete);
    assert_eq!(rollback.usr_exchange, recorded_action(epoch_outcome(epoch)));
    assert_eq!(rollback.candidate.action, recorded_action(candidate_outcome(epoch).journal()));
    assert_eq!(rollback.fresh_db, recorded_action(fresh_outcome(epoch).journal()));
    let current = crate::transition_journal::RuntimeEpoch::capture().unwrap();
    match epoch {
        ProcessEpoch::Current => assert_eq!(record.creation_epoch, current),
        ProcessEpoch::Historical => assert_ne!(record.creation_epoch, current),
    }
}

fn epoch_outcome(epoch: ProcessEpoch) -> RollbackActionOutcome {
    match epoch {
        ProcessEpoch::Current => RollbackActionOutcome::Applied,
        ProcessEpoch::Historical => RollbackActionOutcome::AlreadySatisfied,
    }
}

fn candidate_outcome(epoch: ProcessEpoch) -> CandidateOutcome {
    match epoch {
        ProcessEpoch::Current => CandidateOutcome::Applied,
        ProcessEpoch::Historical => CandidateOutcome::AlreadySatisfied,
    }
}

fn fresh_outcome(epoch: ProcessEpoch) -> FreshOutcome {
    match epoch {
        ProcessEpoch::Current => FreshOutcome::Applied,
        ProcessEpoch::Historical => FreshOutcome::AlreadySatisfied,
    }
}

fn control_dimensions(epoch: ProcessEpoch) -> String {
    format!(
        "usr={:?};candidate={:?};fresh={:?}",
        epoch_outcome(epoch),
        candidate_outcome(epoch),
        fresh_outcome(epoch),
    )
}

fn recorded_action(outcome: RollbackActionOutcome) -> RollbackAction {
    match outcome {
        RollbackActionOutcome::Applied => RollbackAction::Applied,
        RollbackActionOutcome::AlreadySatisfied => RollbackAction::AlreadySatisfied,
    }
}

fn assert_zero_effects() {
    let effects = effect_counts();
    assert_eq!(effects.create, 0);
    assert_eq!(effects.normalize, 0);
    assert_eq!(effects.candidate_move, 0);
    assert_eq!(effects.fresh_removal, 0);
    assert_eq!(retained_exchange_syscall_count(), 0);
    assert_eq!(boot_synchronize_attempt_count(), 0);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
}

fn reset_zero_effect_observers() {
    reset_namespace_effect_counts();
    reset_retained_exchange_syscall_count();
    reset_boot_synchronize_attempt_count();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
}

fn kill_after_zero_effects() {
    assert_zero_effects();
    kill_self();
}
