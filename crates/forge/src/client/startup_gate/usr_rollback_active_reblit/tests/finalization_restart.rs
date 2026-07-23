//! Fresh-handle restart contracts around the two observed delete outcomes.
//!
//! These tests deliberately prove only reconstruction from fresh process-like
//! handles. They do not claim SIGKILL, reboot, or power-loss durability.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            UsrRollbackActiveReblitFinalizationError, finalize_usr_rollback_active_reblit,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_delete_canonical_unlink_fault, arm_next_delete_directory_sync_fault,
        assert_delete_canonical_unlink_fault_consumed, assert_delete_directory_sync_fault_consumed,
        TransitionJournalRecordDeleteError, TransitionJournalRecordDeleteState,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, active_wrapper_path, assert_canonical_absent, assert_fresh_existing_candidate_database,
        assert_no_candidate_effects, build_active, canonical_record, capture_finalization_ready,
        enter_clean_fresh_handles, install_persistent_database, persist_rollback_complete, release_candidate_handles,
        reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_finalization_restarts_from_retained_terminal_source_with_fresh_handles() {
    let mut fixture = build_active(
        Epoch::Current,
        CandidateSource::RootLinksComplete,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let terminal = persist_rollback_complete(&fixture, CandidateOrigin::Applied);
    assert_eq!(terminal.generation, 14);
    install_persistent_database(&mut fixture);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
    let wrapper = active_wrapper_path(&fixture);
    let provenance = fixture
        .fixture
        .database
        .metadata_provenance(fixture.fixture.candidate_state)
        .unwrap()
        .unwrap();
    reset_candidate_effect_observers();
    arm_next_delete_canonical_unlink_fault();

    let error = finalize_usr_rollback_active_reblit(journal, authority).unwrap_err();

    assert_delete_canonical_unlink_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackActiveReblitFinalizationError::Delete(
            TransitionJournalRecordDeleteError::Storage {
                state: TransitionJournalRecordDeleteState::ExactSource,
                ..
            }
        )
    ));
    assert_eq!(fixture.fixture.canonical_record(), terminal);
    assert_no_candidate_effects();
    drop(reservation);

    let retained = release_candidate_handles(fixture);
    assert_eq!(canonical_record(retained.path()), terminal);
    assert_fresh_existing_candidate_database(retained.path(), &terminal, &provenance);
    let clean = enter_clean_fresh_handles(retained.path());

    assert_canonical_absent(retained.path());
    assert!(wrapper.join("usr").is_dir());
    assert_no_candidate_effects();
    drop(clean);
    assert_fresh_existing_candidate_database(retained.path(), &terminal, &provenance);
}

#[test]
fn startup_active_reblit_finalization_restarts_from_observed_absence_with_fresh_handles() {
    let mut fixture = build_active(
        Epoch::Historical,
        CandidateSource::RootLinksComplete,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let terminal = persist_rollback_complete(&fixture, CandidateOrigin::AlreadySatisfied);
    assert_eq!(terminal.generation, 14);
    install_persistent_database(&mut fixture);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
    let wrapper = active_wrapper_path(&fixture);
    let provenance = fixture
        .fixture
        .database
        .metadata_provenance(fixture.fixture.candidate_state)
        .unwrap()
        .unwrap();
    reset_candidate_effect_observers();
    arm_next_delete_directory_sync_fault();

    let error = finalize_usr_rollback_active_reblit(journal, authority).unwrap_err();

    assert_delete_directory_sync_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackActiveReblitFinalizationError::Delete(
            TransitionJournalRecordDeleteError::Storage {
                state: TransitionJournalRecordDeleteState::Absent,
                ..
            }
        )
    ));
    assert_canonical_absent(&fixture.fixture.installation.root);
    assert_no_candidate_effects();
    drop(reservation);

    let retained = release_candidate_handles(fixture);
    assert_fresh_existing_candidate_database(retained.path(), &terminal, &provenance);
    let clean = enter_clean_fresh_handles(retained.path());

    assert_canonical_absent(retained.path());
    assert!(wrapper.join("usr").is_dir());
    assert_no_candidate_effects();
    drop(clean);
    assert_fresh_existing_candidate_database(retained.path(), &terminal, &provenance);
}
