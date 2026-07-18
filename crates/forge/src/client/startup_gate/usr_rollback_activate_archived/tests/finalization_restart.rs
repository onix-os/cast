//! Fresh-handle restart contracts around the two observed delete outcomes.
//!
//! These tests deliberately prove only reconstruction from fresh process-like
//! handles. They do not claim SIGKILL, reboot, or power-loss durability.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            DurableUsrRollbackActivateArchivedFinalizationRecord, UsrRollbackActivateArchivedFinalizationError,
            finalize_usr_rollback_activate_archived,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_delete_canonical_unlink_fault, arm_next_delete_directory_sync_fault,
        assert_delete_canonical_unlink_fault_consumed, assert_delete_directory_sync_fault_consumed,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOutcome, Epoch, RouteFixture, assert_canonical_absent, assert_fresh_exact_database_pair,
        candidate_move_count, canonical_record_from_root, capture_finalization_ready, enter_clean_fresh_handles,
        install_persistent_route_database, persist_rollback_complete, release_route_handles, reset_candidate_observers,
    },
};

#[test]
fn startup_activate_archived_finalization_restarts_from_retained_terminal_source_with_fresh_handles() {
    let mut fixture = exact_route(Epoch::Current);
    let terminal = persist_rollback_complete(&fixture);
    install_persistent_route_database(&mut fixture);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
    let wrapper = fixture.archived_wrapper_path();
    let provenance = fixture
        .fixture
        .fixture
        .database
        .metadata_provenance(fixture.fixture.fixture.candidate_state)
        .unwrap()
        .unwrap();
    reset_candidate_observers();
    arm_next_delete_canonical_unlink_fault();

    let error = finalize_usr_rollback_activate_archived(journal, authority).unwrap_err();

    assert_delete_canonical_unlink_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackActivateArchivedFinalizationError::Delete {
            durable: DurableUsrRollbackActivateArchivedFinalizationRecord::RollbackComplete,
            ..
        }
    ));
    assert_eq!(fixture.canonical_record(), terminal);
    assert_eq!(candidate_move_count(), 0);
    drop(reservation);

    let retained = release_route_handles(fixture);
    assert_eq!(canonical_record_from_root(retained.path()), terminal);
    assert_fresh_exact_database_pair(retained.path(), &terminal, &provenance);
    let clean = enter_clean_fresh_handles(retained.path());

    assert_canonical_absent(retained.path());
    assert!(wrapper.join("usr").is_dir());
    assert_eq!(candidate_move_count(), 0);
    drop(clean);
    assert_fresh_exact_database_pair(retained.path(), &terminal, &provenance);
}

#[test]
fn startup_activate_archived_finalization_restarts_from_observed_absence_with_fresh_handles() {
    let mut fixture = exact_route(Epoch::Historical);
    let terminal = persist_rollback_complete(&fixture);
    install_persistent_route_database(&mut fixture);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
    let wrapper = fixture.archived_wrapper_path();
    let provenance = fixture
        .fixture
        .fixture
        .database
        .metadata_provenance(fixture.fixture.fixture.candidate_state)
        .unwrap()
        .unwrap();
    reset_candidate_observers();
    arm_next_delete_directory_sync_fault();

    let error = finalize_usr_rollback_activate_archived(journal, authority).unwrap_err();

    assert_delete_directory_sync_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackActivateArchivedFinalizationError::Delete {
            durable: DurableUsrRollbackActivateArchivedFinalizationRecord::Absent,
            ..
        }
    ));
    assert_canonical_absent(&fixture.fixture.fixture.installation.root);
    assert_eq!(candidate_move_count(), 0);
    drop(reservation);

    let retained = release_route_handles(fixture);
    assert_fresh_exact_database_pair(retained.path(), &terminal, &provenance);
    let clean = enter_clean_fresh_handles(retained.path());

    assert_canonical_absent(retained.path());
    assert!(wrapper.join("usr").is_dir());
    assert_eq!(candidate_move_count(), 0);
    drop(clean);
    assert_fresh_exact_database_pair(retained.path(), &terminal, &provenance);
}

fn exact_route(epoch: Epoch) -> RouteFixture {
    RouteFixture::new(
        epoch,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
    )
}
