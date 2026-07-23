//! Fresh-handle restart contracts for generation-18 RootLinks finalization.
//!
//! These tests prove reconstruction from fresh process-like handles after the
//! two classified delete errors. They deliberately do not claim SIGKILL,
//! reboot, or power-loss durability for the RootLinks source.

use crate::{
    client::snapshot_startup_recovery_namespace,
    transition_journal::{
        RollbackActionOutcome, arm_next_delete_canonical_unlink_fault,
        arm_next_delete_directory_sync_fault, assert_delete_canonical_unlink_fault_consumed,
        assert_delete_directory_sync_fault_consumed,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOutcome, Epoch, FreshOutcome, assert_canonical_absent,
        assert_suffix_dispatch_error, build_fresh_invalidation, effect_counts,
        enter_fresh_clean_handles, enter_invalidation, install_persistent_joint_absence_database,
        open_layout_database, persist_fresh_invalidated, persist_rollback_complete,
        release_invalidation_fixture_handles, reset_namespace_effect_counts,
    },
};

#[test]
fn startup_new_state_root_links_finalization_restarts_from_retained_terminal_source_with_fresh_handles() {
    let mut fixture = exact_terminal_fixture(Epoch::Current);
    install_persistent_joint_absence_database(&mut fixture);
    drop(open_layout_database(&fixture.fixture.fixture.installation));
    let root = fixture.fixture.fixture.installation.root.clone();
    let namespace_before = snapshot_startup_recovery_namespace(&root);
    let terminal = fixture.canonical_record();
    assert_eq!(terminal.generation, 18);
    reset_namespace_effect_counts();
    arm_next_delete_canonical_unlink_fault();

    let error = enter_invalidation(&fixture);

    assert_delete_canonical_unlink_fault_consumed();
    assert_suffix_dispatch_error(&error);
    assert_eq!(fixture.canonical_record(), terminal);
    assert_eq!(effect_counts().create, 0);
    assert_eq!(effect_counts().normalize, 0);
    assert_eq!(effect_counts().candidate_move, 0);
    assert_eq!(effect_counts().fresh_removal, 0);

    let retained = release_invalidation_fixture_handles(fixture);
    enter_fresh_clean_handles(retained.path());

    assert_canonical_absent(retained.path());
    assert_eq!(snapshot_startup_recovery_namespace(retained.path()), namespace_before);
    assert_eq!(effect_counts().create, 0);
    assert_eq!(effect_counts().normalize, 0);
    assert_eq!(effect_counts().candidate_move, 0);
    assert_eq!(effect_counts().fresh_removal, 0);
}

#[test]
fn startup_new_state_root_links_finalization_restarts_from_observed_absence_with_fresh_handles() {
    let mut fixture = exact_terminal_fixture(Epoch::Historical);
    install_persistent_joint_absence_database(&mut fixture);
    drop(open_layout_database(&fixture.fixture.fixture.installation));
    let root = fixture.fixture.fixture.installation.root.clone();
    let namespace_before = snapshot_startup_recovery_namespace(&root);
    assert_eq!(fixture.canonical_record().generation, 18);
    reset_namespace_effect_counts();
    arm_next_delete_directory_sync_fault();

    let error = enter_invalidation(&fixture);

    assert_delete_directory_sync_fault_consumed();
    assert_suffix_dispatch_error(&error);
    assert_canonical_absent(&root);
    assert_eq!(effect_counts().create, 0);
    assert_eq!(effect_counts().normalize, 0);
    assert_eq!(effect_counts().candidate_move, 0);
    assert_eq!(effect_counts().fresh_removal, 0);

    let retained = release_invalidation_fixture_handles(fixture);
    enter_fresh_clean_handles(retained.path());

    assert_canonical_absent(retained.path());
    assert_eq!(snapshot_startup_recovery_namespace(retained.path()), namespace_before);
    assert_eq!(effect_counts().create, 0);
    assert_eq!(effect_counts().normalize, 0);
    assert_eq!(effect_counts().candidate_move, 0);
    assert_eq!(effect_counts().fresh_removal, 0);
}

fn exact_terminal_fixture(epoch: Epoch) -> super::super::invalidation_test_support::FreshDbInvalidationFixture {
    let fixture = build_fresh_invalidation(
        epoch,
        CandidateSource::RootLinksComplete,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
        FreshOutcome::AlreadySatisfied,
    );
    let invalidated = persist_fresh_invalidated(&fixture, FreshOutcome::AlreadySatisfied);
    let terminal = persist_rollback_complete(&fixture, &invalidated);
    assert_eq!(terminal.generation, 18);
    fixture
}
