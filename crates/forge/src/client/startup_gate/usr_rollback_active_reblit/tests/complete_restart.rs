//! Fresh-handle reopen contracts for both completion-route durability sides.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::UsrRollbackActiveReblitCompleteRouteAdmission,
        startup_recovery::{
            DurableUsrRollbackActiveReblitCompleteRouteRecord,
            UsrRollbackActiveReblitCompleteRoutePersistenceError,
            persist_usr_rollback_active_reblit_complete_route_and_reopen,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, FreshCompleteRouteHandles, active_wrapper_path,
        assert_complete_route_journal_only, assert_exact_no_boot_completion_plan,
        build_active, capture_complete_route_ready, expected_rollback_complete,
        install_persistent_database, persist_candidate_preserved, release_candidate_handles,
        reset_complete_route_effect_observers,
    },
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

#[test]
fn startup_active_reblit_complete_route_source_durable_fresh_handle_reopen_retries_only_the_route() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for candidate_source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in USR_OUTCOMES {
                for candidate_outcome in CandidateOrigin::ALL {
                    let mut fixture = build_active(
                        epoch,
                        candidate_source,
                        usr_outcome,
                        CandidateOrigin::AlreadySatisfied,
                    );
                    let source = persist_candidate_preserved(&fixture, candidate_outcome);
                    let expected = expected_rollback_complete(&source);
                    assert_exact_no_boot_completion_plan(&source, candidate_source);
                    install_persistent_database(&mut fixture);
                    let provenance = fixture
                        .fixture
                        .database
                        .metadata_provenance(fixture.fixture.candidate_state)
                        .unwrap()
                        .unwrap();
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = capture_complete_route_ready(&fixture, &journal, &reservation, &source);
                    let wrapper = active_wrapper_path(&fixture);
                    reset_complete_route_effect_observers();
                    arm_next_temporary_sync_fault();

                    let error =
                        persist_usr_rollback_active_reblit_complete_route_and_reopen(journal, authority)
                            .unwrap_err();

                    assert_temporary_sync_fault_consumed();
                    assert!(matches!(
                        error,
                        UsrRollbackActiveReblitCompleteRoutePersistenceError::Advance {
                            durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::CandidatePreserved,
                            ..
                        }
                    ));
                    assert_eq!(fixture.fixture.canonical_record(), source);
                    assert_complete_route_journal_only();
                    drop(reservation);

                    let retained = release_candidate_handles(fixture);
                    let fresh = FreshCompleteRouteHandles::open(retained.path());
                    assert_eq!(fresh.record, source);
                    let all_before = fresh.database.all().unwrap();
                    let candidate_provenance = fresh.database.metadata_provenance(
                        crate::state::Id::from(source.candidate.id.unwrap()),
                    )
                    .unwrap();
                    assert_eq!(candidate_provenance.as_ref(), Some(&provenance));
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = fresh.capture_ready(&reservation);
                    let database = fresh.database.clone();
                    let journal = fresh.journal;

                    let (reopened, actual) =
                        persist_usr_rollback_active_reblit_complete_route_and_reopen(journal, authority)
                            .unwrap();

                    assert_eq!(actual, expected);
                    assert_eq!(reopened.load().unwrap(), Some(expected.clone()));
                    assert_eq!(database.all().unwrap(), all_before);
                    assert_eq!(database.audit_in_flight_transition().unwrap(), None);
                    assert_eq!(
                        database.metadata_provenance(
                            crate::state::Id::from(expected.candidate.id.unwrap()),
                        )
                        .unwrap(),
                        candidate_provenance
                    );
                    assert!(wrapper.join("usr").is_dir());
                    assert_complete_route_journal_only();
                    drop(reopened);
                    drop(database);
                    drop(reservation);
                    drop(retained);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 24);
}

#[test]
fn startup_active_reblit_complete_route_successor_durable_fresh_handle_reopen_skips_the_route() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for candidate_source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in USR_OUTCOMES {
                for candidate_outcome in CandidateOrigin::ALL {
                    let mut fixture = build_active(
                        epoch,
                        candidate_source,
                        usr_outcome,
                        CandidateOrigin::AlreadySatisfied,
                    );
                    let source = persist_candidate_preserved(&fixture, candidate_outcome);
                    let expected = expected_rollback_complete(&source);
                    assert_exact_no_boot_completion_plan(&source, candidate_source);
                    install_persistent_database(&mut fixture);
                    let provenance = fixture
                        .fixture
                        .database
                        .metadata_provenance(fixture.fixture.candidate_state)
                        .unwrap()
                        .unwrap();
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = capture_complete_route_ready(&fixture, &journal, &reservation, &source);
                    let wrapper = active_wrapper_path(&fixture);
                    reset_complete_route_effect_observers();
                    arm_next_update_first_directory_sync_fault();

                    let error =
                        persist_usr_rollback_active_reblit_complete_route_and_reopen(journal, authority)
                            .unwrap_err();

                    assert_update_first_directory_sync_fault_consumed();
                    assert!(matches!(
                        error,
                        UsrRollbackActiveReblitCompleteRoutePersistenceError::Advance {
                            durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
                            ..
                        }
                    ));
                    assert_eq!(fixture.fixture.canonical_record(), expected);
                    assert_complete_route_journal_only();
                    drop(reservation);

                    let retained = release_candidate_handles(fixture);
                    let fresh = FreshCompleteRouteHandles::open(retained.path());
                    assert_eq!(fresh.record, expected);
                    let all_before = fresh.database.all().unwrap();
                    let candidate = crate::state::Id::from(expected.candidate.id.unwrap());
                    let candidate_provenance = fresh.database.metadata_provenance(candidate).unwrap();
                    assert_eq!(candidate_provenance.as_ref(), Some(&provenance));
                    let reservation = ActiveStateReservation::acquire().unwrap();

                    assert!(matches!(
                        fresh.capture(&reservation).unwrap(),
                        UsrRollbackActiveReblitCompleteRouteAdmission::NotApplicable
                    ));
                    assert_eq!(fresh.journal.load().unwrap(), Some(expected));
                    assert_eq!(fresh.database.all().unwrap(), all_before);
                    assert_eq!(fresh.database.audit_in_flight_transition().unwrap(), None);
                    assert_eq!(fresh.database.metadata_provenance(candidate).unwrap(), candidate_provenance);
                    assert!(wrapper.join("usr").is_dir());
                    assert_complete_route_journal_only();
                    drop(reservation);
                    drop(fresh);
                    drop(retained);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 24);
}
