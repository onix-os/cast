//! Fresh-handle restart contracts for both completion-route durability sides.

use crate::{
    client::startup_recovery::DurableUsrRollbackActivateArchivedCompleteRouteRecord,
    transition_journal::{
        RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{
    CandidateOutcome, CandidateSource, Epoch, RouteFixture, assert_complete_persistence_advance, assert_pending_phase,
    candidate_move_count, canonical_record_from_root, enter_candidate_with_fresh_handles, enter_route,
    install_persistent_route_database, release_route_handles, reset_candidate_observers,
};

#[test]
fn startup_activate_archived_complete_route_source_fault_restart_retries_only_the_route() {
    let mut fixture = RouteFixture::new(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
    );
    install_persistent_route_database(&mut fixture);
    let expected = fixture.expected_successor();
    let database_before = fixture.database_snapshot();
    let namespace_before = fixture.namespace_snapshot();
    let wrapper = fixture.archived_wrapper_path();
    let slot = fixture.archived_slot_path();
    reset_candidate_observers();
    arm_next_temporary_sync_fault();

    let first = enter_route(&fixture);

    assert_temporary_sync_fault_consumed();
    assert_complete_persistence_advance(
        &first,
        DurableUsrRollbackActivateArchivedCompleteRouteRecord::CandidatePreserved,
    );
    assert_eq!(fixture.canonical_record(), fixture.source);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    assert_eq!(candidate_move_count(), 0);

    let retained = release_route_handles(fixture);
    let second = enter_candidate_with_fresh_handles(retained.path());

    assert_pending_phase(&second, crate::transition_journal::Phase::RollbackComplete);
    assert_eq!(canonical_record_from_root(retained.path()), expected);
    assert!(wrapper.join("usr").is_dir());
    assert!(slot.is_file());
    assert_eq!(candidate_move_count(), 0);
}

#[test]
fn startup_activate_archived_complete_route_successor_fault_restart_skips_the_route() {
    let mut fixture = RouteFixture::new(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::AlreadySatisfied,
    );
    install_persistent_route_database(&mut fixture);
    let expected = fixture.expected_successor();
    let database_before = fixture.database_snapshot();
    let namespace_before = fixture.namespace_snapshot();
    let wrapper = fixture.archived_wrapper_path();
    let slot = fixture.archived_slot_path();
    reset_candidate_observers();
    arm_next_update_first_directory_sync_fault();

    let first = enter_route(&fixture);

    assert_update_first_directory_sync_fault_consumed();
    assert_complete_persistence_advance(
        &first,
        DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
    );
    assert_eq!(fixture.canonical_record(), expected);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    assert_eq!(candidate_move_count(), 0);

    let retained = release_route_handles(fixture);
    let second = enter_candidate_with_fresh_handles(retained.path());

    assert_pending_phase(&second, crate::transition_journal::Phase::RollbackComplete);
    assert_eq!(canonical_record_from_root(retained.path()), expected);
    assert!(retained.path().join(".cast/journal/state-transition").is_file());
    assert!(wrapper.join("usr").is_dir());
    assert!(slot.is_file());
    assert_eq!(candidate_move_count(), 0);
}
