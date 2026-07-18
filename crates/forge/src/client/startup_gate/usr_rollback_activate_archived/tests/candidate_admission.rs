//! Exact operation/phase admission for the production startup child.

use crate::{
    client::active_state_snapshot::ActiveStateReservation,
    transition_journal::{Operation, Phase, RollbackActionOutcome},
};

use super::{
    super::{Dispatch, dispatch},
    support::{
        CandidateOrigin, CandidateSource, Epoch, build_candidate, candidate_move_count, expected_candidate_preserved,
        reset_candidate_observers,
    },
};

#[test]
fn startup_activate_archived_candidate_child_handles_only_exact_operation_and_phase() {
    let fixture = build_candidate(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let expected = expected_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let in_flight = fixture.fixture.database.audit_in_flight_transition().unwrap();
    reset_candidate_observers();

    let admitted = dispatch(
        &fixture.fixture.installation,
        &fixture.fixture.database,
        &reservation,
        journal,
        fixture.candidate_intent.clone(),
        in_flight,
    )
    .unwrap();

    let Dispatch::Handled { journal, record } = admitted else {
        panic!("exact ActivateArchived CandidatePreserveIntent was not handled");
    };
    assert_eq!(record, expected);
    assert_eq!(journal.load().unwrap(), Some(expected));
    assert_eq!(candidate_move_count(), 0);
}

#[test]
fn startup_activate_archived_candidate_child_excludes_other_operations_and_phases_without_effects() {
    for operation in [Operation::NewState, Operation::ActiveReblit] {
        assert_unhandled(|record| record.operation = operation);
    }
    for phase in [Phase::UsrRestored, Phase::RollbackComplete] {
        assert_unhandled(|record| record.phase = phase);
    }
}

fn assert_unhandled(mutate: impl FnOnce(&mut crate::transition_journal::TransitionRecord)) {
    let fixture = build_candidate(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::Applied,
    );
    let source = fixture.candidate_intent.clone();
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let in_flight = fixture.fixture.database.audit_in_flight_transition().unwrap();
    let mut presented = source.clone();
    mutate(&mut presented);
    reset_candidate_observers();

    let result = dispatch(
        &fixture.fixture.installation,
        &fixture.fixture.database,
        &reservation,
        journal,
        presented.clone(),
        in_flight,
    )
    .unwrap();

    let Dispatch::Unhandled { journal, record } = result else {
        panic!("inexact ActivateArchived candidate evidence was handled");
    };
    assert_eq!(record, presented);
    assert_eq!(journal.load().unwrap(), Some(source.clone()));
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_eq!(candidate_move_count(), 0);
}
