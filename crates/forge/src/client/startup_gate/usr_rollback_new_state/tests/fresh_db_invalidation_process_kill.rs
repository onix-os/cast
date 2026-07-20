//! Genuine same-boot process-death proof for RootLinks NewState invalidation.

use std::{env, fs, os::unix::process::ExitStatusExt as _};

use crate::{
    Installation,
    client::{
        MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal,
        active_state_snapshot::ActiveStateReservation,
        boot::{boot_synchronize_attempt_count, reset_boot_synchronize_attempt_count},
        startup_gate::{self, CleanSystemStartup},
        startup_recovery::arm_before_usr_rollback_finalization_final_revalidation,
    },
    db::state::exact_fresh_transition_removal_transaction_attempts,
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{Phase, RollbackActionOutcome, decode, encode},
};

use super::{
    super::candidate_test_support::CandidateSource,
    fresh_db_invalidation_process_boundaries::FreshDbInvalidationProcessBoundary,
    fresh_db_invalidation_process_evidence::{
        FreshDatabaseEvidence, PublicJournalIdentity, RawJournalInventory, RootAbiSnapshot,
        StableNamespaceSnapshot, assert_joint_absence, assert_journal_reopenable,
        assert_selected_present, canonical_path,
    },
    fresh_db_invalidation_process_harness::{
        CHILD_DEADLINE, ChildCase, DeadlineChild, MatrixDimensions, ProcessEpoch, ProcessRole,
        ROLE_ENV, assert_exact_root_links_source, assert_parent_environment_clean,
        assert_separate_control_path, expected_fresh_db_invalidated,
        kill_after_real_invalidation_attempt, mark_recovery_as_next_database_opener,
        spawn_child, write_control_case,
    },
    support::{
        Epoch, FreshOutcome, build_fresh_invalidation, effect_counts,
        install_persistent_selected_fresh_database, release_invalidation_fixture_handles,
        reopen_persistent_state_database, reset_namespace_effect_counts,
    },
};

#[test]
fn startup_root_links_new_state_fresh_db_invalidation_process_kills_recover_exactly() {
    match env::var_os(ROLE_ENV) {
        Some(_) => run_child(ChildCase::from_environment()),
        None => run_parent(),
    }
}

fn run_parent() {
    assert_parent_environment_clean();
    assert_eq!(FreshDbInvalidationProcessBoundary::DATABASE.len(), 5);
    assert_eq!(FreshDbInvalidationProcessBoundary::JOURNAL.len(), 5);
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for boundary in FreshDbInvalidationProcessBoundary::ALL {
            run_parent_case(epoch, boundary);
            cases += 1;
        }
    }
    assert_eq!(
        cases, 20,
        "RootLinks invalidation SIGKILL matrix must remain exactly 2 x (5 SQLite + 5 journal)"
    );
}

fn run_parent_case(epoch: Epoch, boundary: FreshDbInvalidationProcessBoundary) {
    let process_epoch = ProcessEpoch::from_fixture(epoch);
    let dimensions = MatrixDimensions::for_epoch(process_epoch);
    let mut fixture = build_fresh_invalidation(
        epoch,
        CandidateSource::RootLinksComplete,
        dimensions.usr_outcome,
        dimensions.candidate_outcome,
        FreshOutcome::Applied,
    );
    install_persistent_selected_fresh_database(&mut fixture);
    let source = fixture.record.clone();
    assert_exact_root_links_source(&source, process_epoch, dimensions);
    let applied = expected_fresh_db_invalidated(&source, RollbackActionOutcome::Applied);
    let already_satisfied =
        expected_fresh_db_invalidated(&source, RollbackActionOutcome::AlreadySatisfied);

    let root = fs::canonicalize(&fixture.fixture.fixture.installation.root).unwrap();
    let source_bytes = fs::read(canonical_path(&root)).unwrap();
    assert_eq!(source_bytes, encode(&source).unwrap());
    let applied_bytes = encode(&applied).unwrap();
    let public_before = PublicJournalIdentity::capture(&root);
    let root_abi_before = RootAbiSnapshot::capture(&root);
    let namespace_before = StableNamespaceSnapshot::capture(&root);
    let database_before = FreshDatabaseEvidence::capture(&fixture.fixture.fixture.database, &source);

    let control = tempfile::tempdir().unwrap();
    let control_path = fs::canonicalize(control.path()).unwrap();
    assert_separate_control_path(&root, &control_path);
    write_control_case(
        &control_path,
        process_epoch,
        boundary,
        dimensions,
        &source,
        &source_bytes,
    );
    let retained_root = release_invalidation_fixture_handles(fixture);

    let crash = spawn_child(ProcessRole::Crash, process_epoch, boundary, &root, &control_path);
    let crash_status =
        DeadlineChild::new(crash, "RootLinks invalidation crash child").wait(CHILD_DEADLINE);
    assert_eq!(
        crash_status.signal(),
        Some(nix::libc::SIGKILL),
        "crash child for {process_epoch:?} {boundary:?} missed its armed boundary: {crash_status:?}"
    );

    // Raw inspection deliberately precedes any journal store or SQLite open.
    let raw_after_crash = RawJournalInventory::capture(&root);
    raw_after_crash.assert_after_crash(boundary, &source_bytes, &applied_bytes);
    public_before.assert_crash_identity(PublicJournalIdentity::capture(&root), boundary);
    assert_eq!(RootAbiSnapshot::capture(&root), root_abi_before);
    assert_eq!(StableNamespaceSnapshot::capture(&root), namespace_before);

    // The parent does not reopen SQLite here. The recovery child below is the
    // first database opener after the SIGKILL and owns rollback recovery.
    mark_recovery_as_next_database_opener(&control_path);
    let recovery = spawn_child(
        ProcessRole::Recover,
        process_epoch,
        boundary,
        &root,
        &control_path,
    );
    let recovery_status =
        DeadlineChild::new(recovery, "RootLinks invalidation recovery child").wait(CHILD_DEADLINE);
    assert!(
        recovery_status.success(),
        "recovery child failed for {process_epoch:?} {boundary:?}: {recovery_status:?}"
    );
    assert_eq!(recovery_status.signal(), None);

    let expected = expected_after_recovery(boundary, &applied, &already_satisfied);
    RawJournalInventory::capture(&root).assert_clean_successor(&encode(expected).unwrap());
    public_before.assert_same_anchors(PublicJournalIdentity::capture(&root));
    assert_eq!(RootAbiSnapshot::capture(&root), root_abi_before);
    assert_eq!(StableNamespaceSnapshot::capture(&root), namespace_before);

    let installation = Installation::open(&root, None).unwrap();
    let database = reopen_persistent_state_database(&installation);
    database_before.assert_recovered(&database, expected);
    drop(database);
    drop(installation);
    assert_journal_reopenable(&root, expected);

    drop(retained_root);
    drop(control);
}

fn expected_after_recovery<'record>(
    boundary: FreshDbInvalidationProcessBoundary,
    applied: &'record crate::transition_journal::TransitionRecord,
    already_satisfied: &'record crate::transition_journal::TransitionRecord,
) -> &'record crate::transition_journal::TransitionRecord {
    if boundary.is_database() && !boundary.database_commit_survives() {
        applied
    } else if boundary.canonical_is_source() {
        already_satisfied
    } else {
        applied
    }
}

fn run_child(case: ChildCase) {
    let source = case.source_record();
    if case.role == ProcessRole::Recover {
        case.claim_recovery_first_database_open();
    }
    let installation = Installation::open(&case.root, None).unwrap();
    assert_eq!(installation.root, case.root);
    // This state handle is intentionally the recovery process's first SQLite
    // open. In rollback cases it performs SQLite's real transaction recovery.
    let database = reopen_persistent_state_database(&installation);
    let layout_database = super::support::open_layout_database(&installation);
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

fn run_crash_child(
    case: &ChildCase,
    system: &MutableSystemCapabilities,
    source: &crate::transition_journal::TransitionRecord,
) {
    assert_eq!(decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(), *source);
    FreshDatabaseEvidence::capture(system.state_db(), source);
    let root_abi_before = RootAbiSnapshot::capture(&case.root);
    let namespace_before = StableNamespaceSnapshot::capture(&case.root);
    reset_unrelated_effect_observers();
    assert_zero_unrelated_effects();
    case.boundary.arm(kill_after_real_invalidation_attempt);

    let reservation = ActiveStateReservation::acquire().unwrap();
    let result = CleanSystemStartup::enter(system, &reservation);
    assert_eq!(RootAbiSnapshot::capture(&case.root), root_abi_before);
    assert_eq!(StableNamespaceSnapshot::capture(&case.root), namespace_before);
    panic!(
        "crash child escaped RootLinks invalidation boundary {:?} with startup success={} error={:?}",
        case.boundary,
        result.is_ok(),
        result.err(),
    );
}

fn run_recovery_child(
    case: &ChildCase,
    system: &MutableSystemCapabilities,
    source: &crate::transition_journal::TransitionRecord,
) {
    let applied = expected_fresh_db_invalidated(source, RollbackActionOutcome::Applied);
    let already_satisfied =
        expected_fresh_db_invalidated(source, RollbackActionOutcome::AlreadySatisfied);
    let expected = expected_after_recovery(case.boundary, &applied, &already_satisfied);
    let expected_start = if case.boundary.canonical_is_source() {
        source
    } else {
        &applied
    };
    assert_eq!(decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(), *expected_start);

    if case.boundary.is_database() && !case.boundary.database_commit_survives() {
        assert_selected_present(system.state_db(), source);
    } else {
        assert_joint_absence(system.state_db(), source);
    }
    let root_abi_before = RootAbiSnapshot::capture(&case.root);
    let namespace_before = StableNamespaceSnapshot::capture(&case.root);
    reset_unrelated_effect_observers();
    assert_zero_unrelated_effects();
    arm_before_usr_rollback_finalization_final_revalidation(|| {
        panic!("generation-16 invalidation recovery attempted terminal finalization")
    });

    let reservation = ActiveStateReservation::acquire().unwrap();
    let error = match CleanSystemStartup::enter(system, &reservation) {
        Ok(_) => panic!("fresh startup admitted unresolved RootLinks invalidation"),
        Err(error) => error,
    };
    let startup_gate::Error::RecoveryPending(pending) = &error else {
        panic!("expected generation-17 recovery-pending result, got {error:?}");
    };
    assert_eq!(pending.transition_id(), &expected.transition_id);
    assert_eq!(pending.phase(), Phase::FreshDbInvalidated);
    assert_eq!(pending.disposition(), expected.recovery_disposition());
    assert!(pending.blockers().is_empty(), "unexpected blockers: {:?}", pending.blockers());
    assert!(pending.retains_database(system.state_db()));

    let expected_removals = usize::from(
        case.boundary.is_database() && !case.boundary.database_commit_survives(),
    );
    let effects = effect_counts();
    assert_eq!(effects.fresh_removal, expected_removals);
    assert_eq!(
        exact_fresh_transition_removal_transaction_attempts(),
        expected_removals
    );
    assert_eq!(effects.create, 0);
    assert_eq!(effects.normalize, 0);
    assert_eq!(effects.candidate_move, 0);
    assert_eq!(retained_exchange_syscall_count(), 0);
    assert_eq!(boot_synchronize_attempt_count(), 0);
    assert_joint_absence(system.state_db(), expected);
    assert_eq!(decode(&fs::read(canonical_path(&case.root)).unwrap()).unwrap(), *expected);
    RawJournalInventory::capture(&case.root).assert_clean_successor(&encode(expected).unwrap());
    assert_eq!(RootAbiSnapshot::capture(&case.root), root_abi_before);
    assert_eq!(StableNamespaceSnapshot::capture(&case.root), namespace_before);
    drop(error);
    drop(reservation);
}

fn reset_unrelated_effect_observers() {
    reset_namespace_effect_counts();
    reset_retained_exchange_syscall_count();
    reset_boot_synchronize_attempt_count();
}

fn assert_zero_unrelated_effects() {
    let effects = effect_counts();
    assert_eq!(effects.create, 0);
    assert_eq!(effects.normalize, 0);
    assert_eq!(effects.candidate_move, 0);
    assert_eq!(effects.fresh_removal, 0);
    assert_eq!(retained_exchange_syscall_count(), 0);
    assert_eq!(boot_synchronize_attempt_count(), 0);
}
