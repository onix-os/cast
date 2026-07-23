//! RootLinks-only same-boot process-death proof for generation-12 finalization.

use std::{
    ffi::OsString,
    fs,
    os::unix::fs::MetadataExt as _,
    path::Path,
};

use crate::{
    Installation, State,
    client::{
        MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal,
        active_state_snapshot::ActiveStateReservation,
        boot::{boot_synchronize_attempt_count, reset_boot_synchronize_attempt_count},
        snapshot_startup_recovery_namespace,
        startup_reconciliation::{
            active_reblit_candidate_preserve_exchange_attempt_count, fresh_db_invalidation_removal_call_count,
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
        startup_recovery::arm_before_usr_rollback_activate_archived_finalization_final_revalidation,
    },
    db,
    state::Id,
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{
        AbortDisposition, BootRollback, CandidateOrigin, ForwardPhase, Operation, Phase, PreviousOrigin,
        RollbackAction, RollbackActionOutcome, TransitionRecord,
    },
};

use super::support::{
    CandidateOutcome, CandidateSource, Epoch, RouteFixture, candidate_move_count, install_persistent_route_database,
    open_layout_database, open_state_database, persist_rollback_complete, release_route_handles,
    reset_candidate_observers,
};

const TEST_NAME: &str = concat!(
    "client::startup_gate::usr_rollback_activate_archived::tests::root_links_terminal_process_kill::",
    "startup_activate_archived_root_links_terminal_delete_process_kills_restart_cleanly",
);
const OPERATION: TerminalOperation = TerminalOperation::ActivateArchived;
const CAST_NAME: &str = ".cast";

#[derive(Debug, Eq, PartialEq)]
struct ArchivedDatabaseEvidence {
    states: Vec<State>,
    in_flight: Option<db::state::InFlightTransition>,
    candidate_ownership: db::state::TransitionOwnership,
    previous_ownership: db::state::TransitionOwnership,
    candidate_provenance: db::state::MetadataProvenance,
    previous_provenance: Option<db::state::MetadataProvenance>,
}

impl ArchivedDatabaseEvidence {
    fn capture(database: &db::state::Database, record: &TransitionRecord) -> Self {
        let candidate = Id::from(record.candidate.id.unwrap());
        let previous = Id::from(record.previous.id.unwrap());
        let states = database.all().unwrap();
        assert_eq!(states.len(), 2);
        assert_eq!(database.get(candidate).unwrap().id, candidate);
        assert_eq!(database.get(previous).unwrap().id, previous);
        let in_flight = database.audit_in_flight_transition().unwrap();
        assert_eq!(in_flight, None);
        let candidate_ownership = database.transition_ownership(candidate, &record.transition_id).unwrap();
        let previous_ownership = database.transition_ownership(previous, &record.transition_id).unwrap();
        assert_eq!(candidate_ownership, db::state::TransitionOwnership::Cleared);
        assert_eq!(previous_ownership, db::state::TransitionOwnership::Cleared);
        let candidate_provenance = database
            .metadata_provenance(candidate)
            .unwrap()
            .expect("RootLinks ActivateArchived candidate provenance must remain present");
        let previous_provenance = database.metadata_provenance(previous).unwrap();
        assert_eq!(previous_provenance, None);
        Self {
            states,
            in_flight,
            candidate_ownership,
            previous_ownership,
            candidate_provenance,
            previous_provenance,
        }
    }
}

#[test]
fn startup_activate_archived_root_links_terminal_delete_process_kills_restart_cleanly() {
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
    assert_eq!(cases, 12, "ActivateArchived RootLinks matrix must remain exactly 2 x 6");
}

fn run_parent_case(epoch: ProcessEpoch, scenario: RootLinksDeleteScenario) {
    let fixture_epoch = match epoch {
        ProcessEpoch::Current => Epoch::Current,
        ProcessEpoch::Historical => Epoch::Historical,
    };
    let outcome = epoch_outcome(epoch);
    let mut fixture = RouteFixture::new(
        fixture_epoch,
        CandidateSource::RootLinksComplete,
        outcome,
        candidate_outcome(epoch),
    );
    let terminal = persist_rollback_complete(&fixture);
    assert_exact_terminal(&terminal, epoch);
    install_persistent_route_database(&mut fixture);
    drop(open_layout_database(&fixture.fixture.fixture.installation));

    let root = fs::canonicalize(&fixture.fixture.fixture.installation.root).unwrap();
    let journal = JournalExpectation::capture(&root, &terminal);
    let root_links = RootLinksSnapshot::capture(&root);
    let namespace = snapshot_startup_recovery_namespace(&root);
    let database = ArchivedDatabaseEvidence::capture(&fixture.fixture.fixture.database, &terminal);
    assert_archived_topology(&root, &terminal);
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
    let retained_root = release_route_handles(fixture);

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
        assert_archived_topology(&root, &terminal);
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
    assert_archived_topology(&case.root, &terminal);

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
    ArchivedDatabaseEvidence::capture(system.state_db(), &terminal);
    match case.role {
        ProcessRole::InitialCrash => run_initial_crash(&case, &system),
        ProcessRole::RecoveryCrash => run_recovery_crash(&case, &system),
        ProcessRole::FinalRecover => run_final_recovery(&case, &system, &terminal, &journal, &root_links),
    }
}

fn run_initial_crash(case: &ChildInvocation, system: &MutableSystemCapabilities) {
    reset_zero_effect_observers();
    assert_zero_effects();
    forbid_journal_update("ActivateArchived RootLinks terminal crash");
    if !case.scenario.arm_initial_bound_delete_kill(kill_after_zero_effects) {
        arm_before_usr_rollback_activate_archived_finalization_final_revalidation(kill_after_zero_effects);
    }
    let reservation = ActiveStateReservation::acquire().unwrap();
    let result = CleanSystemStartup::enter(system, &reservation);
    panic!(
        "initial ActivateArchived RootLinks crash escaped {:?}: success={} error={:?}",
        case.scenario,
        result.is_ok(),
        result.err(),
    );
}

fn run_recovery_crash(case: &ChildInvocation, system: &MutableSystemCapabilities) {
    reset_zero_effect_observers();
    assert_zero_effects();
    forbid_journal_update("ActivateArchived RootLinks residue recovery");
    case.scenario.arm_recovery_kill(kill_after_zero_effects);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let result = CleanSystemStartup::enter(system, &reservation);
    panic!(
        "ActivateArchived RootLinks residue recovery escaped {:?}: success={} error={:?}",
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
    let database_before = ArchivedDatabaseEvidence::capture(system.state_db(), terminal);
    let namespace_before = snapshot_startup_recovery_namespace(&case.root);
    reset_zero_effect_observers();
    assert_zero_effects();
    forbid_journal_update("ActivateArchived RootLinks final recovery");

    {
        let reservation = ActiveStateReservation::acquire().unwrap();
        let clean = CleanSystemStartup::enter(system, &reservation)
            .unwrap_or_else(|error| panic!("ActivateArchived RootLinks recovery did not reach clean: {error:?}"));
        assert_zero_effects();
        assert_clean_holds_journal_lock(system.installation(), &case.root);
        drop(clean);
    }
    journal.assert_raw(&case.root, RawRecordState::Absent);
    assert_clean_store_reopens(system.installation(), &case.root);

    {
        let reservation = ActiveStateReservation::acquire().unwrap();
        let clean_again = CleanSystemStartup::enter(system, &reservation).unwrap_or_else(|error| {
            panic!("ActivateArchived RootLinks clean endpoint did not remain clean: {error:?}")
        });
        assert_clean_holds_journal_lock(system.installation(), &case.root);
        drop(clean_again);
    }

    assert_zero_effects();
    assert_eq!(ArchivedDatabaseEvidence::capture(system.state_db(), terminal), database_before);
    assert_eq!(snapshot_startup_recovery_namespace(&case.root), namespace_before);
    root_links.assert_unchanged(&case.root);
    assert_archived_topology(&case.root, terminal);
}

fn capture_database_at_root(root: &Path, terminal: &TransitionRecord) -> ArchivedDatabaseEvidence {
    let installation = Installation::open(root, None).unwrap();
    let database = open_state_database(&installation);
    ArchivedDatabaseEvidence::capture(&database, terminal)
}

fn assert_exact_terminal(record: &TransitionRecord, epoch: ProcessEpoch) {
    assert_eq!(record.operation, Operation::ActivateArchived);
    assert_eq!(record.phase, Phase::RollbackComplete);
    assert_eq!(record.generation, 12);
    assert_eq!(record.candidate.origin, CandidateOrigin::Archived);
    assert_eq!(record.previous.origin, PreviousOrigin::ActiveState);
    assert_ne!(record.candidate.id, record.previous.id);
    let rollback = record.rollback.as_ref().unwrap();
    assert_eq!(rollback.source, ForwardPhase::RootLinksComplete);
    assert_eq!(rollback.usr_exchange, recorded_action(epoch_outcome(epoch)));
    assert_eq!(rollback.candidate.action, recorded_candidate_action(candidate_outcome(epoch)));
    assert_eq!(rollback.previous_archive, RollbackAction::NotRequired);
    assert_eq!(rollback.candidate.disposition, AbortDisposition::Rearchive);
    assert_eq!(rollback.fresh_db, RollbackAction::NotRequired);
    assert_eq!(rollback.boot, BootRollback::NotRequired);
    assert!(!rollback.external_effects_may_remain);
}

fn assert_archived_topology(root: &Path, record: &TransitionRecord) {
    let candidate = record.candidate.id.unwrap();
    let previous = record.previous.id.unwrap();
    let wrapper = root.join(CAST_NAME).join("root").join(candidate.to_string());
    assert!(fs::symlink_metadata(&wrapper).unwrap().is_dir());
    let usr = wrapper.join("usr");
    assert!(fs::symlink_metadata(&usr).unwrap().is_dir());
    let marker = usr.join(".cast-tree-id");
    let slot = wrapper.join(format!(
        ".cast-state-slot-{candidate}-{}",
        record.candidate.tree_token.as_str()
    ));
    let marker_metadata = fs::symlink_metadata(&marker).unwrap();
    let slot_metadata = fs::symlink_metadata(&slot).unwrap();
    assert_eq!(
        (slot_metadata.dev(), slot_metadata.ino()),
        (marker_metadata.dev(), marker_metadata.ino())
    );
    assert_eq!(marker_metadata.nlink(), 2);
    assert_eq!(slot_metadata.nlink(), 2);
    let mut names = fs::read_dir(&wrapper)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    let mut expected = vec![OsString::from("usr"), slot.file_name().unwrap().to_owned()];
    expected.sort();
    assert_eq!(names, expected);
    assert!(fs::read_dir(root.join(CAST_NAME).join("root/staging")).unwrap().next().is_none());
    assert!(!root.join(CAST_NAME).join("quarantine").join(record.quarantine_name.as_str()).exists());
    assert_eq!(fs::read_to_string(root.join("usr/.stateID")).unwrap(), previous.to_string());
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

fn recorded_candidate_action(outcome: CandidateOutcome) -> RollbackAction {
    match outcome {
        CandidateOutcome::Applied => RollbackAction::Applied,
        CandidateOutcome::AlreadySatisfied => RollbackAction::AlreadySatisfied,
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
        "usr={:?};candidate={:?}",
        epoch_outcome(epoch),
        candidate_outcome(epoch),
    )
}

fn kill_after_zero_effects() {
    assert_zero_effects();
    kill_self();
}

fn reset_zero_effect_observers() {
    reset_candidate_observers();
    reset_retained_exchange_syscall_count();
    reset_boot_synchronize_attempt_count();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
}

fn assert_zero_effects() {
    assert_eq!(candidate_move_count(), 0);
    assert_eq!(retained_exchange_syscall_count(), 0);
    assert_eq!(boot_synchronize_attempt_count(), 0);
    assert_eq!(fresh_db_invalidation_removal_call_count(), 0);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
}
