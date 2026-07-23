//! RootLinks-only same-boot process-death proof for generation-14 finalization.

use std::{fs, path::Path};

use crate::{
    Installation, State,
    client::{
        MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal,
        active_state_snapshot::ActiveStateReservation,
        snapshot_startup_recovery_namespace,
        startup_reconciliation::fresh_db_invalidation_removal_call_count,
        startup_gate::{
            CleanSystemStartup,
            root_links_terminal_process_harness::{
                ChildInvocation, ControlDirectory, JournalExpectation, ParentCase, ProcessEpoch, ProcessRole,
                RawRecordState, RootLinksDeleteScenario, RootLinksSnapshot, TerminalOperation,
                assert_clean_holds_journal_lock, assert_clean_store_reopens, forbid_journal_update, kill_self,
            },
        },
        startup_recovery::arm_before_usr_rollback_active_reblit_finalization_final_revalidation,
    },
    db,
    state::Id,
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, RollbackActionOutcome,
        TransitionRecord,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, active_wrapper_path_at, assert_complete_route_journal_only,
        build_active_at_wrapper_index, install_persistent_database, open_layout_database, open_state_database,
        persist_rollback_complete, release_candidate_handles, reset_complete_route_effect_observers,
    },
};

const TEST_NAME: &str = concat!(
    "client::startup_gate::usr_rollback_active_reblit::tests::root_links_terminal_process_kill::",
    "startup_active_reblit_root_links_terminal_delete_process_kills_restart_cleanly",
);
const OPERATION: TerminalOperation = TerminalOperation::ActiveReblit;
const CAST_NAME: &str = ".cast";

#[derive(Debug, Eq, PartialEq)]
struct ActiveReblitDatabaseEvidence {
    states: Vec<State>,
    in_flight: Option<db::state::InFlightTransition>,
    ownership: db::state::TransitionOwnership,
    provenance: db::state::MetadataProvenance,
}

impl ActiveReblitDatabaseEvidence {
    fn capture(database: &db::state::Database, record: &TransitionRecord) -> Self {
        assert_eq!(record.candidate.id, record.previous.id);
        let candidate = Id::from(record.candidate.id.unwrap());
        let states = database.all().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(database.get(candidate).unwrap().id, candidate);
        let in_flight = database.audit_in_flight_transition().unwrap();
        assert_eq!(in_flight, None);
        let ownership = database.transition_ownership(candidate, &record.transition_id).unwrap();
        assert_eq!(ownership, db::state::TransitionOwnership::Cleared);
        let provenance = database
            .metadata_provenance(candidate)
            .unwrap()
            .expect("RootLinks ActiveReblit candidate provenance must remain present");
        Self {
            states,
            in_flight,
            ownership,
            provenance,
        }
    }
}

#[test]
fn startup_active_reblit_root_links_terminal_delete_process_kills_restart_cleanly() {
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
    assert_eq!(cases, 12, "ActiveReblit RootLinks matrix must remain exactly 2 x 6");
}

fn run_parent_case(epoch: ProcessEpoch, scenario: RootLinksDeleteScenario) {
    let fixture_epoch = match epoch {
        ProcessEpoch::Current => Epoch::Current,
        ProcessEpoch::Historical => Epoch::Historical,
    };
    let wrapper_index = wrapper_index(epoch);
    let mut fixture = build_active_at_wrapper_index(
        fixture_epoch,
        CandidateSource::RootLinksComplete,
        epoch_outcome(epoch),
        CandidateOrigin::AlreadySatisfied,
        wrapper_index,
    );
    let terminal = persist_rollback_complete(&fixture, candidate_origin(epoch));
    assert_exact_terminal(&terminal, epoch);
    install_persistent_database(&mut fixture);
    drop(open_layout_database(&fixture.fixture.installation));

    let root = fs::canonicalize(&fixture.fixture.installation.root).unwrap();
    assert_eq!(active_wrapper_path_at(&fixture, wrapper_index), wrapper_path(&root, &terminal, wrapper_index));
    let journal = JournalExpectation::capture(&root, &terminal);
    let root_links = RootLinksSnapshot::capture(&root);
    let namespace = snapshot_startup_recovery_namespace(&root);
    let database = ActiveReblitDatabaseEvidence::capture(&fixture.fixture.database, &terminal);
    assert_wrapper(&root, &terminal, wrapper_index);
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
    let retained_root = release_candidate_handles(fixture);

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
        assert_wrapper(&root, &terminal, wrapper_index);
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
    assert_wrapper(&case.root, &terminal, wrapper_index(case.epoch));

    let installation = Installation::open(&case.root, None).unwrap();
    assert_eq!(installation.root, case.root);
    let database = open_state_database(&installation);
    let layout_database = open_layout_database(&installation);
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation,
        database,
        layout_database,
    );
    ActiveReblitDatabaseEvidence::capture(system.state_db(), &terminal);
    match case.role {
        ProcessRole::InitialCrash => run_initial_crash(&case, &system),
        ProcessRole::RecoveryCrash => run_recovery_crash(&case, &system),
        ProcessRole::FinalRecover => run_final_recovery(&case, &system, &terminal, &journal, &root_links),
    }
}

fn run_initial_crash(case: &ChildInvocation, system: &MutableSystemCapabilities) {
    reset_complete_route_effect_observers();
    assert_zero_effects();
    forbid_journal_update("ActiveReblit RootLinks terminal crash");
    if !case.scenario.arm_initial_bound_delete_kill(kill_after_zero_effects) {
        arm_before_usr_rollback_active_reblit_finalization_final_revalidation(kill_after_zero_effects);
    }
    let reservation = ActiveStateReservation::acquire().unwrap();
    let result = CleanSystemStartup::enter(system, &reservation);
    panic!(
        "initial ActiveReblit RootLinks crash escaped {:?}: success={} error={:?}",
        case.scenario,
        result.is_ok(),
        result.err(),
    );
}

fn run_recovery_crash(case: &ChildInvocation, system: &MutableSystemCapabilities) {
    reset_complete_route_effect_observers();
    assert_zero_effects();
    forbid_journal_update("ActiveReblit RootLinks residue recovery");
    case.scenario.arm_recovery_kill(kill_after_zero_effects);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let result = CleanSystemStartup::enter(system, &reservation);
    panic!(
        "ActiveReblit RootLinks residue recovery escaped {:?}: success={} error={:?}",
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
    let database_before = ActiveReblitDatabaseEvidence::capture(system.state_db(), terminal);
    let namespace_before = snapshot_startup_recovery_namespace(&case.root);
    reset_complete_route_effect_observers();
    assert_zero_effects();
    forbid_journal_update("ActiveReblit RootLinks final recovery");

    {
        let reservation = ActiveStateReservation::acquire().unwrap();
        let clean = CleanSystemStartup::enter(system, &reservation)
            .unwrap_or_else(|error| panic!("ActiveReblit RootLinks recovery did not reach clean: {error:?}"));
        assert_zero_effects();
        assert_clean_holds_journal_lock(system.installation(), &case.root);
        drop(clean);
    }
    journal.assert_raw(&case.root, RawRecordState::Absent);
    assert_clean_store_reopens(system.installation(), &case.root);

    {
        let reservation = ActiveStateReservation::acquire().unwrap();
        let clean_again = CleanSystemStartup::enter(system, &reservation).unwrap_or_else(|error| {
            panic!("ActiveReblit RootLinks clean endpoint did not remain clean: {error:?}")
        });
        assert_clean_holds_journal_lock(system.installation(), &case.root);
        drop(clean_again);
    }

    assert_zero_effects();
    assert_eq!(ActiveReblitDatabaseEvidence::capture(system.state_db(), terminal), database_before);
    assert_eq!(snapshot_startup_recovery_namespace(&case.root), namespace_before);
    root_links.assert_unchanged(&case.root);
    assert_wrapper(&case.root, terminal, wrapper_index(case.epoch));
}

fn capture_database_at_root(root: &Path, terminal: &TransitionRecord) -> ActiveReblitDatabaseEvidence {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    ActiveReblitDatabaseEvidence::capture(&database, terminal)
}

fn assert_exact_terminal(record: &TransitionRecord, epoch: ProcessEpoch) {
    assert_eq!(record.operation, Operation::ActiveReblit);
    assert_eq!(record.phase, Phase::RollbackComplete);
    assert_eq!(record.generation, 14);
    assert_eq!(record.candidate.id, record.previous.id);
    let rollback = record.rollback.as_ref().unwrap();
    assert_eq!(rollback.source, ForwardPhase::RootLinksComplete);
    assert_eq!(rollback.usr_exchange, recorded_action(epoch_outcome(epoch)));
    assert_eq!(rollback.candidate.action, recorded_action(candidate_origin(epoch).outcome()));
    assert_eq!(rollback.previous_archive, RollbackAction::NotRequired);
    assert_eq!(rollback.candidate.disposition, AbortDisposition::Quarantine);
    assert_eq!(rollback.fresh_db, RollbackAction::NotRequired);
    assert_eq!(rollback.boot, BootRollback::NotRequired);
    assert!(rollback.external_effects_may_remain);
}

fn assert_wrapper(root: &Path, record: &TransitionRecord, index: usize) {
    let wrapper = wrapper_path(root, record, index);
    let metadata = fs::symlink_metadata(&wrapper).unwrap();
    assert!(metadata.is_dir(), "{} is not the preserved wrapper", wrapper.display());
    assert!(fs::symlink_metadata(wrapper.join("usr")).unwrap().is_dir());
}

fn wrapper_path(root: &Path, record: &TransitionRecord, index: usize) -> std::path::PathBuf {
    root.join(CAST_NAME).join("quarantine").join(format!(
        "replaced-active-reblit-wrapper-{}-{}-{index}",
        record.previous.id.unwrap(),
        record.previous.tree_token.as_str(),
    ))
}

fn wrapper_index(epoch: ProcessEpoch) -> usize {
    match epoch {
        ProcessEpoch::Current => 0,
        ProcessEpoch::Historical => 13,
    }
}

fn epoch_outcome(epoch: ProcessEpoch) -> RollbackActionOutcome {
    match epoch {
        ProcessEpoch::Current => RollbackActionOutcome::Applied,
        ProcessEpoch::Historical => RollbackActionOutcome::AlreadySatisfied,
    }
}

fn candidate_origin(epoch: ProcessEpoch) -> CandidateOrigin {
    match epoch {
        ProcessEpoch::Current => CandidateOrigin::Applied,
        ProcessEpoch::Historical => CandidateOrigin::AlreadySatisfied,
    }
}

fn recorded_action(outcome: RollbackActionOutcome) -> RollbackAction {
    match outcome {
        RollbackActionOutcome::Applied => RollbackAction::Applied,
        RollbackActionOutcome::AlreadySatisfied => RollbackAction::AlreadySatisfied,
    }
}

fn control_dimensions(epoch: ProcessEpoch) -> String {
    format!(
        "usr={:?};candidate={:?};wrapper-index={}",
        epoch_outcome(epoch),
        candidate_origin(epoch),
        wrapper_index(epoch),
    )
}

fn kill_after_zero_effects() {
    assert_zero_effects();
    kill_self();
}

fn assert_zero_effects() {
    assert_complete_route_journal_only();
    assert_eq!(fresh_db_invalidation_removal_call_count(), 0);
}
