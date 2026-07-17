//! Fresh-handle restart contracts for both completion-route durability sides.

use crate::{
    client::startup_recovery::DurableUsrRollbackActiveReblitCompleteRouteRecord,
    transition_journal::{
        Phase, RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, active_wrapper_path, assert_complete_persistence_advance, assert_no_candidate_effects,
        assert_pending_phase, build_active, canonical_record, enter_candidate, enter_fresh_handles,
        expected_rollback_complete, install_persistent_database, persist_candidate_preserved,
        release_candidate_handles, reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_complete_route_source_durable_failure_converges_with_fresh_handles() {
    let mut fixture = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = persist_candidate_preserved(&fixture, CandidateOrigin::Applied);
    let expected = expected_rollback_complete(&source);
    install_persistent_database(&mut fixture);
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    let wrapper = active_wrapper_path(&fixture);
    reset_candidate_effect_observers();
    arm_next_temporary_sync_fault();

    let first = enter_candidate(&fixture);

    assert_temporary_sync_fault_consumed();
    assert_complete_persistence_advance(
        &first,
        DurableUsrRollbackActiveReblitCompleteRouteRecord::CandidatePreserved,
    );
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();

    let retained = release_candidate_handles(fixture);
    let second = enter_fresh_handles(retained.path());

    assert_pending_phase(&second, Phase::RollbackComplete);
    assert_eq!(canonical_record(retained.path()), expected);
    assert!(wrapper.join("usr").is_dir());
    assert_no_candidate_effects();
}

#[test]
fn startup_active_reblit_complete_route_successor_durable_failure_remains_terminal_with_fresh_handles() {
    let mut fixture = build_active(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = persist_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);
    let expected = expected_rollback_complete(&source);
    install_persistent_database(&mut fixture);
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    let wrapper = active_wrapper_path(&fixture);
    reset_candidate_effect_observers();
    arm_next_update_first_directory_sync_fault();

    let first = enter_candidate(&fixture);

    assert_update_first_directory_sync_fault_consumed();
    assert_complete_persistence_advance(
        &first,
        DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
    );
    assert_eq!(fixture.fixture.canonical_record(), expected);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();

    let retained = release_candidate_handles(fixture);
    let second = enter_fresh_handles(retained.path());

    assert_pending_phase(&second, Phase::RollbackComplete);
    assert_eq!(canonical_record(retained.path()), expected);
    assert!(wrapper.join("usr").is_dir());
    assert_no_candidate_effects();
}
