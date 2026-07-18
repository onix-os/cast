//! Real-process restart proof across NewState candidate preservation.

use std::{env, fs, os::unix::process::ExitStatusExt as _};

use crate::{
    Installation,
    client::{
        MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal,
        active_state_snapshot::ActiveStateReservation,
        snapshot_startup_recovery_namespace,
        startup_gate::{self, CleanSystemStartup},
        startup_reconciliation::{
            arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture,
            new_state_candidate_preserve_move_attempt_count, reset_new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_post_move_durability_events,
            take_new_state_candidate_preserve_post_move_durability_events,
        },
    },
    transition_journal::{Phase, TransitionRecord, decode, encode},
};

use super::{
    super::candidate_test_support::CandidateSource,
    candidate_move_process_harness::{
        CHILD_DEADLINE, CandidateMoveEvidence, ChildCase, DeadlineChild, FreshCandidateDatabase, MatrixDimensions,
        ProcessEpoch, ProcessRole, ProcessSource, PublicJournalIdentity, ROLE_ENV, assert_candidate_source,
        assert_journal_inventory, assert_journal_reopenable, assert_journal_reopenable_from_installation,
        assert_parent_environment_clean, assert_preserved_topology, assert_separate_control_path,
        assert_staged_topology, canonical_path, capture_database_at_root, expected_candidate_preserved,
        expected_post_events, kill_after_real_candidate_move, spawn_child, write_control_case,
    },
    candidate_process_kill_boundaries::CandidateProcessKillBoundary,
    support::{
        Epoch, TargetPrefix, build_candidate, install_persistent_database, release_candidate_handles,
        reopen_persistent_state_database,
    },
};

#[test]
fn startup_new_state_candidate_move_process_kill_recovers_without_second_move() {
    match env::var_os(ROLE_ENV) {
        Some(_) => run_child(ChildCase::from_environment()),
        None => run_parent(),
    }
}

fn run_parent() {
    assert_parent_environment_clean();
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for boundary in CandidateProcessKillBoundary::ALL {
                run_parent_case(epoch, source, boundary);
                cases += 1;
            }
        }
    }
    assert_eq!(
        cases, 28,
        "NewState candidate-move SIGKILL matrix must remain exactly 2 x 2 x 7"
    );
}

fn run_parent_case(epoch: Epoch, source: CandidateSource, boundary: CandidateProcessKillBoundary) {
    let process_epoch = ProcessEpoch::from_fixture(epoch);
    let process_source = ProcessSource::from_fixture(source);
    let dimensions = MatrixDimensions::for_case(process_epoch, process_source);
    let mut fixture = build_candidate(epoch, source, dimensions.usr_outcome, TargetPrefix::Canonical);
    install_persistent_database(&mut fixture);
    let source_record = fixture.candidate_intent.clone();
    assert_candidate_source(&source_record, process_epoch, process_source, dimensions);
    let expected = expected_candidate_preserved(&source_record);

    let root = fs::canonicalize(&fixture.fixture.installation.root).unwrap();
    let source_bytes = fs::read(canonical_path(&root)).unwrap();
    assert_eq!(source_bytes, encode(&source_record).unwrap());
    let public_before = PublicJournalIdentity::capture(&root);
    let database_before = FreshCandidateDatabase::capture(&fixture.fixture.database, &source_record);
    let move_evidence = CandidateMoveEvidence::capture_staged(&root, &source_record);

    let control = tempfile::tempdir().unwrap();
    let control_path = fs::canonicalize(control.path()).unwrap();
    assert_separate_control_path(&root, &control_path);
    write_control_case(
        &control_path,
        process_epoch,
        process_source,
        boundary,
        dimensions,
        &source_record,
        &source_bytes,
    );
    let retained_root = release_candidate_handles(fixture);

    let crash = spawn_child(
        ProcessRole::Crash,
        process_epoch,
        process_source,
        boundary,
        &root,
        &control_path,
    );
    let crash_status = DeadlineChild::new(crash, "NewState candidate-move crash child").wait(CHILD_DEADLINE);
    assert_eq!(
        crash_status.signal(),
        Some(nix::libc::SIGKILL),
        "crash child for {process_epoch:?} {process_source:?} {boundary:?} missed its boundary: {crash_status:?}"
    );
    let public_after_crash = PublicJournalIdentity::capture(&root);
    assert_eq!(public_after_crash, public_before);
    assert_eq!(fs::read(canonical_path(&root)).unwrap(), source_bytes);
    assert_eq!(capture_database_at_root(&root, &source_record), database_before);
    move_evidence.assert_preserved(&root, &source_record);
    let namespace_after_crash = snapshot_startup_recovery_namespace(&root);
    assert_journal_reopenable(&root, &source_record);

    let recovery = spawn_child(
        ProcessRole::Recover,
        process_epoch,
        process_source,
        boundary,
        &root,
        &control_path,
    );
    let recovery_status = DeadlineChild::new(recovery, "NewState candidate-move recovery child").wait(CHILD_DEADLINE);
    assert!(
        recovery_status.success(),
        "recovery child failed for {process_epoch:?} {process_source:?} {boundary:?}: {recovery_status:?}"
    );
    assert_eq!(recovery_status.signal(), None);

    let public_after_recovery = PublicJournalIdentity::capture(&root);
    public_before.assert_same_public_anchors(public_after_recovery);
    assert_eq!(fs::read(canonical_path(&root)).unwrap(), encode(&expected).unwrap());
    assert_eq!(capture_database_at_root(&root, &expected), database_before);
    assert_eq!(snapshot_startup_recovery_namespace(&root), namespace_after_crash);
    move_evidence.assert_preserved(&root, &expected);
    assert_journal_reopenable(&root, &expected);

    drop(retained_root);
    drop(control);
}

fn run_child(case: ChildCase) {
    let source = case.source_record();
    let installation = Installation::open(&case.root, None).unwrap();
    assert_eq!(installation.root, case.root);
    let database = reopen_persistent_state_database(&installation);
    let layout_database = super::support::open_layout_database(&installation);
    FreshCandidateDatabase::capture(&database, &source);
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        installation,
        database,
        layout_database,
    );
    match case.role {
        ProcessRole::Crash => run_crash_child(&case, &system, &source),
        ProcessRole::Recover => run_recovery_child(&case, &system, &source),
    }
}

fn run_crash_child(case: &ChildCase, system: &MutableSystemCapabilities, source: &TransitionRecord) {
    assert_eq!(decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(), *source);
    assert_journal_inventory(&case.root);
    assert_staged_topology(&case.root, source);
    reset_new_state_candidate_preserve_move_attempt_count();
    reset_new_state_candidate_preserve_post_move_durability_events();
    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
    assert!(take_new_state_candidate_preserve_post_move_durability_events().is_empty());
    case.boundary.arm(kill_after_real_candidate_move);

    let reservation = ActiveStateReservation::acquire().unwrap();
    let result = CleanSystemStartup::enter(system, &reservation);
    panic!(
        "crash child escaped NewState post-move boundary with startup success={} error={:?}",
        result.is_ok(),
        result.err(),
    );
}

fn run_recovery_child(case: &ChildCase, system: &MutableSystemCapabilities, source: &TransitionRecord) {
    let installation = system.installation();
    let database = system.state_db();
    assert_eq!(decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(), *source);
    assert_journal_inventory(&case.root);
    assert_preserved_topology(&case.root, source);
    let database_before = FreshCandidateDatabase::capture(database, source);
    let namespace_before = snapshot_startup_recovery_namespace(&case.root);
    let expected = expected_candidate_preserved(source);
    let expected_events = expected_post_events(&case.root, source);
    reset_new_state_candidate_preserve_move_attempt_count();
    reset_new_state_candidate_preserve_post_move_durability_events();
    arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture(|| {
        panic!("fresh NewState recovery selected Apply instead of the zero-move Finish path")
    });

    let reservation = ActiveStateReservation::acquire().unwrap();
    let error = match CleanSystemStartup::enter(system, &reservation) {
        Ok(_) => panic!("fresh startup admitted unresolved NewState candidate evidence"),
        Err(error) => error,
    };
    let startup_gate::Error::RecoveryPending(pending) = &error else {
        panic!("expected exact CandidatePreserved recovery-pending result, got {error:?}");
    };
    assert_eq!(pending.transition_id(), &expected.transition_id);
    assert_eq!(pending.phase(), Phase::CandidatePreserved);
    assert_eq!(pending.disposition(), expected.recovery_disposition());
    assert!(
        pending.blockers().is_empty(),
        "unexpected startup blockers: {:?}",
        pending.blockers()
    );
    assert!(pending.retains_database(database));
    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
    assert_eq!(
        take_new_state_candidate_preserve_post_move_durability_events(),
        expected_events
    );
    assert_eq!(
        decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(),
        expected
    );
    assert_eq!(FreshCandidateDatabase::capture(database, &expected), database_before);
    assert_eq!(snapshot_startup_recovery_namespace(&case.root), namespace_before);
    assert_preserved_topology(&case.root, &expected);
    assert_journal_inventory(&case.root);
    drop(error);
    drop(reservation);
    assert_journal_reopenable_from_installation(installation, &expected);
}
