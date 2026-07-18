//! Genuine same-boot process-death proof across ActiveReblit wrapper exchange.

use std::{env, fs, os::unix::process::ExitStatusExt as _};

use crate::{
    Installation,
    client::{
        active_state_snapshot::ActiveStateReservation,
        snapshot_startup_recovery_namespace,
        startup_gate::{self, CleanSystemStartup},
        startup_reconciliation::{
            active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_post_exchange_durability_events,
            take_active_reblit_candidate_preserve_post_exchange_durability_events,
        },
    },
    db,
    transition_journal::{Phase, decode, encode},
};

use super::{
    super::candidate_test_support::CandidateSource,
    candidate_wrapper_exchange_kill_boundaries::CandidateWrapperExchangeKillBoundary,
    candidate_wrapper_exchange_process_harness::{
        CHILD_DEADLINE, ChildCase, DeadlineChild, ExistingCandidateDatabase, MatrixDimensions, ProcessEpoch,
        ProcessRole, ProcessSource, PublicJournalIdentity, ROLE_ENV, WrapperExchangeEvidence, assert_candidate_source,
        assert_journal_inventory, assert_journal_reopenable, assert_journal_reopenable_from_installation,
        assert_parent_environment_clean, assert_preserved_topology, assert_separate_control_path,
        assert_staged_topology, canonical_path, capture_database_at_root, expected_candidate_preserved,
        expected_post_events, kill_after_real_wrapper_exchange, spawn_child, write_control_case,
    },
    support::{
        CandidateOrigin, Epoch, build_active_at_wrapper_index, install_persistent_database, open_state_database,
        release_candidate_handles,
    },
};

#[test]
fn startup_active_reblit_candidate_wrapper_exchange_process_kill_recovers_without_second_exchange() {
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
            for boundary in CandidateWrapperExchangeKillBoundary::ALL {
                run_parent_case(epoch, source, boundary);
                cases += 1;
            }
        }
    }
    assert_eq!(
        cases, 32,
        "ActiveReblit wrapper-exchange SIGKILL matrix must remain exactly 2 x 2 x 8"
    );
}

fn run_parent_case(epoch: Epoch, source: CandidateSource, boundary: CandidateWrapperExchangeKillBoundary) {
    let process_epoch = ProcessEpoch::from_fixture(epoch);
    let process_source = ProcessSource::from_fixture(source);
    let dimensions = MatrixDimensions::for_case(process_epoch, process_source);
    let mut fixture = build_active_at_wrapper_index(
        epoch,
        source,
        dimensions.usr_outcome,
        CandidateOrigin::Applied,
        dimensions.wrapper_index,
    );
    install_persistent_database(&mut fixture);
    let source_record = fixture.candidate_intent.clone();
    assert_candidate_source(&source_record, process_epoch, process_source, dimensions);
    let expected = expected_candidate_preserved(&source_record);

    let root = fs::canonicalize(&fixture.fixture.installation.root).unwrap();
    let source_bytes = fs::read(canonical_path(&root)).unwrap();
    assert_eq!(source_bytes, encode(&source_record).unwrap());
    let public_before = PublicJournalIdentity::capture(&root);
    let database_before = ExistingCandidateDatabase::capture(&fixture.fixture.database, &source_record);
    let exchange_evidence = WrapperExchangeEvidence::capture_staged(&root, &source_record, dimensions.wrapper_index);

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
    let crash_status = DeadlineChild::new(crash, "ActiveReblit wrapper-exchange crash child").wait(CHILD_DEADLINE);
    assert_eq!(
        crash_status.signal(),
        Some(nix::libc::SIGKILL),
        "crash child for {process_epoch:?} {process_source:?} {boundary:?} missed its boundary: {crash_status:?}"
    );
    public_before.assert_source_unchanged(&root, &source_bytes);
    assert_eq!(capture_database_at_root(&root, &source_record), database_before);
    exchange_evidence.assert_preserved(&root, &source_record, dimensions.wrapper_index);
    assert_journal_reopenable(&root, &source_record);
    let namespace_after_crash = snapshot_startup_recovery_namespace(&root);

    let recovery = spawn_child(
        ProcessRole::Recover,
        process_epoch,
        process_source,
        boundary,
        &root,
        &control_path,
    );
    let recovery_status =
        DeadlineChild::new(recovery, "ActiveReblit wrapper-exchange recovery child").wait(CHILD_DEADLINE);
    assert!(
        recovery_status.success(),
        "recovery child failed for {process_epoch:?} {process_source:?} {boundary:?}: {recovery_status:?}"
    );
    assert_eq!(recovery_status.signal(), None);

    let public_after = PublicJournalIdentity::capture(&root);
    public_before.assert_same_public_anchors(public_after);
    assert_eq!(fs::read(canonical_path(&root)).unwrap(), encode(&expected).unwrap());
    assert_eq!(decode(&fs::read(canonical_path(&root)).unwrap()).unwrap(), expected);
    assert_eq!(capture_database_at_root(&root, &expected), database_before);
    assert_eq!(snapshot_startup_recovery_namespace(&root), namespace_after_crash);
    exchange_evidence.assert_preserved(&root, &expected, dimensions.wrapper_index);
    assert_journal_reopenable(&root, &expected);

    drop(retained_root);
    drop(control);
}

fn run_child(case: ChildCase) {
    let source = case.source_record();
    let installation = Installation::open(&case.root, None).unwrap();
    assert_eq!(installation.root, case.root);
    let database = open_state_database(&installation);
    ExistingCandidateDatabase::capture(&database, &source);
    match case.role {
        ProcessRole::Crash => run_crash_child(&case, &installation, &database, &source),
        ProcessRole::Recover => run_recovery_child(&case, &installation, &database, &source),
    }
}

fn run_crash_child(
    case: &ChildCase,
    installation: &Installation,
    database: &db::state::Database,
    source: &crate::transition_journal::TransitionRecord,
) {
    assert_eq!(decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(), *source);
    assert_journal_inventory(&case.root);
    assert_staged_topology(&case.root, source, case.dimensions().wrapper_index);
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    reset_active_reblit_candidate_preserve_post_exchange_durability_events();
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    assert!(take_active_reblit_candidate_preserve_post_exchange_durability_events().is_empty());
    case.boundary.arm(kill_after_real_wrapper_exchange);

    let reservation = ActiveStateReservation::acquire().unwrap();
    let result = CleanSystemStartup::enter(installation, database, &reservation);
    panic!(
        "crash child escaped ActiveReblit wrapper-exchange boundary {:?} with startup success={} error={:?}",
        case.boundary,
        result.is_ok(),
        result.err(),
    );
}

fn run_recovery_child(
    case: &ChildCase,
    installation: &Installation,
    database: &db::state::Database,
    source: &crate::transition_journal::TransitionRecord,
) {
    assert_eq!(decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(), *source);
    assert_journal_inventory(&case.root);
    assert_preserved_topology(&case.root, source, case.dimensions().wrapper_index);
    let database_before = ExistingCandidateDatabase::capture(database, source);
    let namespace_before = snapshot_startup_recovery_namespace(&case.root);
    let expected = expected_candidate_preserved(source);
    let expected_events = expected_post_events(&case.root, source, case.dimensions().wrapper_index);
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    reset_active_reblit_candidate_preserve_post_exchange_durability_events();

    let reservation = ActiveStateReservation::acquire().unwrap();
    let error = match CleanSystemStartup::enter(installation, database, &reservation) {
        Ok(_) => panic!("fresh startup admitted unresolved ActiveReblit candidate evidence"),
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
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    assert_eq!(
        take_active_reblit_candidate_preserve_post_exchange_durability_events(),
        expected_events
    );
    assert_eq!(
        decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(),
        expected
    );
    assert_eq!(ExistingCandidateDatabase::capture(database, &expected), database_before);
    assert_eq!(snapshot_startup_recovery_namespace(&case.root), namespace_before);
    assert_preserved_topology(&case.root, &expected, case.dimensions().wrapper_index);
    assert_journal_inventory(&case.root);
    drop(error);
    drop(reservation);
    assert_journal_reopenable_from_installation(installation, &expected);
}
