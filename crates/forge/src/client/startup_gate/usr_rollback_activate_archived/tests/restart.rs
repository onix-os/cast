//! Fresh-handle reopen contracts for both completion-route durability sides.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::UsrRollbackActivateArchivedCompleteRouteAdmission,
        startup_recovery::{
            DurableUsrRollbackActivateArchivedCompleteRouteRecord,
            UsrRollbackActivateArchivedCompleteRoutePersistenceError,
            persist_usr_rollback_activate_archived_complete_route_and_reopen,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{
    CandidateOutcome, CandidateSource, Epoch, FreshRouteHandles, RouteFixture, candidate_move_count,
    install_persistent_route_database, release_route_handles, reset_candidate_observers,
};

#[test]
fn startup_activate_archived_complete_route_source_durable_fresh_handle_reopen_retries_only_the_route() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    let mut fixture = RouteFixture::new(epoch, source, usr_outcome, candidate_outcome);
                    install_persistent_route_database(&mut fixture);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_candidate_observers();
                    let authority = fixture.capture_ready(&journal, &reservation);
                    arm_next_temporary_sync_fault();

                    let error = persist_usr_rollback_activate_archived_complete_route_and_reopen(journal, authority)
                        .unwrap_err();

                    assert_temporary_sync_fault_consumed();
                    assert!(matches!(
                        error,
                        UsrRollbackActivateArchivedCompleteRoutePersistenceError::Advance {
                            durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::CandidatePreserved,
                            ..
                        }
                    ));
                    assert_eq!(fixture.canonical_record(), fixture.source);
                    assert_eq!(candidate_move_count(), 0);
                    drop(reservation);

                    let expected_source = fixture.source.clone();
                    let expected = fixture.expected_successor();
                    let candidate = fixture.fixture.fixture.candidate_state;
                    let previous = fixture.fixture.fixture.previous_state;
                    let retained = release_route_handles(fixture);
                    let fresh = FreshRouteHandles::open(retained.path());
                    assert_eq!(fresh.record, expected_source);
                    let all_before = fresh.database.all().unwrap();
                    let candidate_provenance = fresh.database.metadata_provenance(candidate).unwrap();
                    let previous_provenance = fresh.database.metadata_provenance(previous).unwrap();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = fresh.capture_ready(&reservation);
                    let database = fresh.database.clone();
                    let journal = fresh.journal;

                    let (reopened, actual) =
                        persist_usr_rollback_activate_archived_complete_route_and_reopen(journal, authority)
                            .unwrap();

                    assert_eq!(actual, expected);
                    assert_eq!(reopened.load().unwrap(), Some(expected));
                    assert_eq!(database.all().unwrap(), all_before);
                    assert_eq!(database.audit_in_flight_transition().unwrap(), None);
                    assert_eq!(database.metadata_provenance(candidate).unwrap(), candidate_provenance);
                    assert_eq!(database.metadata_provenance(previous).unwrap(), previous_provenance);
                    assert_eq!(candidate_move_count(), 0);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 24);
}

#[test]
fn startup_activate_archived_complete_route_successor_durable_fresh_handle_reopen_skips_the_route() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    let mut fixture = RouteFixture::new(epoch, source, usr_outcome, candidate_outcome);
                    install_persistent_route_database(&mut fixture);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_candidate_observers();
                    let authority = fixture.capture_ready(&journal, &reservation);
                    let expected = fixture.expected_successor();
                    arm_next_update_first_directory_sync_fault();

                    let error = persist_usr_rollback_activate_archived_complete_route_and_reopen(journal, authority)
                        .unwrap_err();

                    assert_update_first_directory_sync_fault_consumed();
                    assert!(matches!(
                        error,
                        UsrRollbackActivateArchivedCompleteRoutePersistenceError::Advance {
                            durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
                            ..
                        }
                    ));
                    assert_eq!(fixture.canonical_record(), expected);
                    drop(reservation);

                    let candidate = fixture.fixture.fixture.candidate_state;
                    let previous = fixture.fixture.fixture.previous_state;
                    let retained = release_route_handles(fixture);
                    let fresh = FreshRouteHandles::open(retained.path());
                    assert_eq!(fresh.record, expected);
                    let all_before = fresh.database.all().unwrap();
                    let candidate_provenance = fresh.database.metadata_provenance(candidate).unwrap();
                    let previous_provenance = fresh.database.metadata_provenance(previous).unwrap();
                    let reservation = ActiveStateReservation::acquire().unwrap();

                    assert!(matches!(
                        fresh.capture(&reservation).unwrap(),
                        UsrRollbackActivateArchivedCompleteRouteAdmission::NotApplicable
                    ));
                    assert_eq!(fresh.journal.load().unwrap(), Some(expected));
                    assert_eq!(fresh.database.all().unwrap(), all_before);
                    assert_eq!(fresh.database.audit_in_flight_transition().unwrap(), None);
                    assert_eq!(fresh.database.metadata_provenance(candidate).unwrap(), candidate_provenance);
                    assert_eq!(fresh.database.metadata_provenance(previous).unwrap(), previous_provenance);
                    assert_eq!(candidate_move_count(), 0);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 24);
}
