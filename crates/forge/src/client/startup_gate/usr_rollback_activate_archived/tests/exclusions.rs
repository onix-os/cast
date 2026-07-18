//! Operation, phase, plan, database, topology, and dispatch exclusions.

use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::UsrRollbackActivateArchivedCompleteRouteAdmission,
    },
    state::TransitionId,
    transition_journal::{
        AbortDisposition, BootRollback, CandidateOrigin, ForwardPhase, Operation, Phase, PreviousOrigin,
        RollbackAction, RollbackActionOutcome,
    },
};

use super::support::{CandidateOutcome, CandidateSource, Epoch, RouteFixture, assert_pending_phase, capture_record};

#[test]
fn startup_activate_archived_complete_route_remains_absent_from_production_dispatch() {
    let fixture = exact_fixture();
    let database_before = fixture.database_snapshot();
    let namespace_before = fixture.namespace_snapshot();

    let error = fixture.fixture.fixture.enter();

    assert_pending_phase(&error, Phase::CandidatePreserved);
    assert_eq!(fixture.canonical_record(), fixture.source);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    fixture.assert_exact_database_pair();
    fixture.assert_exact_archived_topology();
}

#[test]
fn startup_activate_archived_complete_route_rejects_other_operations_and_phases() {
    let fixture = exact_fixture();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();

    assert!(matches!(
        capture_record(&fixture, &journal, &reservation, &fixture.fixture.candidate_intent,).unwrap(),
        UsrRollbackActivateArchivedCompleteRouteAdmission::NotApplicable
    ));
    let terminal = fixture.expected_successor();
    assert!(matches!(
        capture_record(&fixture, &journal, &reservation, &terminal).unwrap(),
        UsrRollbackActivateArchivedCompleteRouteAdmission::NotApplicable
    ));
    for operation in [Operation::NewState, Operation::ActiveReblit] {
        let mut other = fixture.source.clone();
        other.operation = operation;
        assert!(matches!(
            capture_record(&fixture, &journal, &reservation, &other).unwrap(),
            UsrRollbackActivateArchivedCompleteRouteAdmission::NotApplicable
        ));
    }
    assert_eq!(fixture.canonical_record(), fixture.source);
}

#[test]
fn startup_activate_archived_complete_route_defers_every_inexact_plan_boundary() {
    assert_plan_deferred(|record| {
        record.rollback = None;
    });
    assert_plan_deferred(|record| {
        record.candidate.origin = CandidateOrigin::Fresh;
    });
    assert_plan_deferred(|record| {
        record.previous.origin = PreviousOrigin::Unmanaged;
    });
    assert_plan_deferred(|record| {
        record.candidate.id = None;
    });
    assert_plan_deferred(|record| {
        record.previous.id = None;
    });
    assert_plan_deferred(|record| {
        record.candidate.id = record.previous.id;
    });
    assert_plan_deferred(|record| {
        record.rollback.as_mut().unwrap().source = ForwardPhase::TransactionTriggersComplete;
    });
    assert_plan_deferred(|record| {
        record.rollback.as_mut().unwrap().previous_archive = RollbackAction::Pending;
    });
    assert_plan_deferred(|record| {
        record.rollback.as_mut().unwrap().usr_exchange = RollbackAction::Pending;
    });
    assert_plan_deferred(|record| {
        record.rollback.as_mut().unwrap().usr_exchange = RollbackAction::NotRequired;
    });
    assert_plan_deferred(|record| {
        record.rollback.as_mut().unwrap().candidate.action = RollbackAction::Pending;
    });
    assert_plan_deferred(|record| {
        record.rollback.as_mut().unwrap().candidate.action = RollbackAction::NotRequired;
    });
    assert_plan_deferred(|record| {
        record.rollback.as_mut().unwrap().candidate.disposition = AbortDisposition::Quarantine;
    });
    assert_plan_deferred(|record| {
        record.rollback.as_mut().unwrap().fresh_db = RollbackAction::Applied;
    });
    assert_plan_deferred(|record| {
        record.rollback.as_mut().unwrap().boot = BootRollback::PendingUnverifiable;
    });
    assert_plan_deferred(|record| {
        record.rollback.as_mut().unwrap().external_effects_may_remain = true;
    });
}

#[test]
fn startup_activate_archived_complete_route_requires_exact_candidate_previous_and_provenance_rows() {
    for mutation in [
        DatabaseMutation::Candidate,
        DatabaseMutation::Previous,
        DatabaseMutation::Provenance,
    ] {
        let fixture = exact_fixture();
        match mutation {
            DatabaseMutation::Candidate => fixture
                .fixture
                .fixture
                .database
                .remove(&fixture.fixture.fixture.candidate_state)
                .unwrap(),
            DatabaseMutation::Previous => fixture
                .fixture
                .fixture
                .database
                .remove(&fixture.fixture.fixture.previous_state)
                .unwrap(),
            DatabaseMutation::Provenance => fixture
                .fixture
                .fixture
                .database
                .delete_metadata_provenance_for_test(fixture.fixture.fixture.candidate_state)
                .unwrap(),
        };
        let rows_after_mutation = fixture.fixture.fixture.database.all().unwrap();
        let in_flight_after_mutation = fixture.fixture.fixture.database.audit_in_flight_transition().unwrap();
        let namespace_before = fixture.namespace_snapshot();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();

        assert!(matches!(
            fixture.capture(&journal, &reservation).unwrap(),
            UsrRollbackActivateArchivedCompleteRouteAdmission::Deferred
        ));
        assert_eq!(fixture.canonical_record(), fixture.source, "{mutation:?}");
        assert_eq!(
            fixture.fixture.fixture.database.all().unwrap(),
            rows_after_mutation,
            "{mutation:?}"
        );
        assert_eq!(
            fixture.fixture.fixture.database.audit_in_flight_transition().unwrap(),
            in_flight_after_mutation,
            "{mutation:?}"
        );
        assert_eq!(fixture.namespace_snapshot(), namespace_before, "{mutation:?}");
    }

    let fixture = exact_fixture();
    let transition = TransitionId::parse("abababababababababababababababab").unwrap();
    fixture
        .fixture
        .fixture
        .database
        .add_with_transition(&transition, &[], Some("foreign in-flight row"), None)
        .unwrap();
    let database_after_mutation = fixture.database_snapshot();
    let namespace_before = fixture.namespace_snapshot();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        fixture.capture(&journal, &reservation).unwrap(),
        UsrRollbackActivateArchivedCompleteRouteAdmission::Deferred
    ));
    assert_eq!(fixture.canonical_record(), fixture.source);
    assert_eq!(fixture.database_snapshot(), database_after_mutation);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
}

#[test]
fn startup_activate_archived_complete_route_refuses_missing_and_extra_archived_topology() {
    let fixture = exact_fixture();
    fs::remove_file(fixture.archived_slot_path()).unwrap();
    assert_topology_deferred(&fixture);

    let fixture = exact_fixture();
    fs::create_dir(fixture.transition_quarantine_path()).unwrap();
    assert_topology_deferred(&fixture);
}

#[derive(Clone, Copy, Debug)]
enum DatabaseMutation {
    Candidate,
    Previous,
    Provenance,
}

fn exact_fixture() -> RouteFixture {
    RouteFixture::new(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
    )
}

fn assert_plan_deferred(mutate: impl FnOnce(&mut crate::transition_journal::TransitionRecord)) {
    let fixture = exact_fixture();
    let mut changed = fixture.source.clone();
    mutate(&mut changed);
    let database_before = fixture.database_snapshot();
    let namespace_before = fixture.namespace_snapshot();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        capture_record(&fixture, &journal, &reservation, &changed).unwrap(),
        UsrRollbackActivateArchivedCompleteRouteAdmission::Deferred
    ));
    assert_eq!(fixture.canonical_record(), fixture.source);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
}

fn assert_topology_deferred(fixture: &RouteFixture) {
    let database_before = fixture.database_snapshot();
    let namespace_after_mutation = fixture.namespace_snapshot();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        fixture.capture(&journal, &reservation).unwrap(),
        UsrRollbackActivateArchivedCompleteRouteAdmission::Deferred
    ));
    assert_eq!(fixture.canonical_record(), fixture.source);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_after_mutation);
}
