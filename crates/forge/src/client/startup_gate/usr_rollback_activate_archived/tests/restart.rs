//! Fresh journal/reservation behavior on both durable fault sides.

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

use super::support::{CandidateOutcome, CandidateSource, Epoch, RouteFixture, capture_record};

#[test]
fn startup_activate_archived_complete_route_source_fault_restart_retries_only_the_route() {
    let fixture = RouteFixture::new(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
    );
    let database_before = fixture.database_snapshot();
    let namespace_before = fixture.namespace_snapshot();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    arm_next_temporary_sync_fault();

    let first = persist_usr_rollback_activate_archived_complete_route_and_reopen(journal, authority).unwrap_err();

    assert_temporary_sync_fault_consumed();
    assert!(matches!(
        first,
        UsrRollbackActivateArchivedCompleteRoutePersistenceError::Advance {
            durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::CandidatePreserved,
            ..
        }
    ));
    assert_eq!(fixture.canonical_record(), fixture.source);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    drop(reservation);

    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let expected = fixture.expected_successor();
    let (reopened, actual) =
        persist_usr_rollback_activate_archived_complete_route_and_reopen(journal, authority).unwrap();

    assert_eq!(actual, expected);
    assert_eq!(reopened.load().unwrap(), Some(expected.clone()));
    assert_eq!(fixture.canonical_record(), expected);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    fixture.assert_exact_archived_topology();
}

#[test]
fn startup_activate_archived_complete_route_successor_fault_restart_skips_the_route() {
    let fixture = RouteFixture::new(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::AlreadySatisfied,
    );
    let database_before = fixture.database_snapshot();
    let namespace_before = fixture.namespace_snapshot();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let expected = fixture.expected_successor();
    arm_next_update_first_directory_sync_fault();

    let first = persist_usr_rollback_activate_archived_complete_route_and_reopen(journal, authority).unwrap_err();

    assert_update_first_directory_sync_fault_consumed();
    assert!(matches!(
        first,
        UsrRollbackActivateArchivedCompleteRoutePersistenceError::Advance {
            durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
            ..
        }
    ));
    assert_eq!(fixture.canonical_record(), expected);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    drop(reservation);

    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        capture_record(&fixture, &journal, &reservation, &expected).unwrap(),
        UsrRollbackActivateArchivedCompleteRouteAdmission::NotApplicable
    ));
    assert_eq!(journal.load().unwrap(), Some(expected.clone()));
    assert_eq!(fixture.canonical_record(), expected);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    fixture.assert_exact_database_pair();
    fixture.assert_exact_archived_topology();
}
