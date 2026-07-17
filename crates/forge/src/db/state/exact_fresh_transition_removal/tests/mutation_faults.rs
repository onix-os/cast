use super::super::super::{
    ExactFreshTransitionObservation, ExactFreshTransitionRemovalFault, ExactFreshTransitionRemovalOutcome,
    arm_exact_fresh_transition_removal_fault, assert_exact_fresh_transition_removal_fault_consumed,
    exact_fresh_transition_removal_transaction_attempts,
};
use super::support::{
    DurableMutation, RelationCounts, all_relation_counts, apply_mutation, disable_foreign_keys, fixture, present,
    relation_counts,
};

#[test]
fn exact_fresh_removal_rejects_every_state_selection_provenance_and_transition_change() {
    for (index, mutation) in DurableMutation::ALL.into_iter().enumerate() {
        let digit = char::from_digit(u32::try_from(index + 1).unwrap(), 16).unwrap();
        let fixture = fixture(digit, &format!("changed-{mutation:?}"));
        let preimage = present(&fixture);
        let before = all_relation_counts(&fixture.database);
        apply_mutation(&fixture, mutation);

        let error = fixture.database.remove_exact_fresh_transition(preimage).unwrap_err();

        assert_ne!(
            error.outcome(),
            ExactFreshTransitionRemovalOutcome::DefinitelyNotApplied
        );
        assert_eq!(all_relation_counts(&fixture.database), before);
    }
}

#[test]
fn exact_fresh_removal_atomically_deletes_state_selections_and_provenance_only() {
    let database = super::super::super::Database::new(":memory:").unwrap();
    let fixture = super::support::fixture_in(database, 'a', "exact-target");
    let decoy = fixture
        .database
        .add(&[], Some("unrelated archived state"), None)
        .unwrap();
    let preimage = present(&fixture);
    disable_foreign_keys(&fixture.database);

    assert_eq!(
        relation_counts(&fixture.database, fixture.state.id),
        RelationCounts {
            states: 1,
            selections: 2,
            provenance: 1,
        }
    );
    let absence = fixture.database.remove_exact_fresh_transition(preimage).unwrap();

    assert_eq!(absence.state_id(), fixture.state.id);
    assert_eq!(absence.transition_id(), &fixture.transition);
    assert_eq!(
        relation_counts(&fixture.database, fixture.state.id),
        RelationCounts {
            states: 0,
            selections: 0,
            provenance: 0,
        }
    );
    assert_eq!(
        fixture
            .database
            .inspect_exact_fresh_transition(fixture.state.id, &fixture.transition)
            .unwrap(),
        ExactFreshTransitionObservation::JointlyAbsent(absence)
    );
    assert_eq!(fixture.database.get(decoy.id).unwrap(), decoy);
}

#[test]
fn exact_fresh_removal_pre_and_in_transaction_faults_preserve_one_complete_preimage_without_retry() {
    let cases = [
        (ExactFreshTransitionRemovalFault::BeforeTransaction, 0),
        (ExactFreshTransitionRemovalFault::BetweenProvenanceAndStateDelete, 1),
        (ExactFreshTransitionRemovalFault::BeforeCommit, 1),
    ];
    for (index, (fault, expected_attempts)) in cases.into_iter().enumerate() {
        let digit = char::from_digit(u32::try_from(index + 12).unwrap(), 16).unwrap();
        let fixture = fixture(digit, &format!("fault-{fault:?}"));
        let preimage = present(&fixture);
        arm_exact_fresh_transition_removal_fault(fault);

        let error = fixture.database.remove_exact_fresh_transition(preimage).unwrap_err();

        assert_exact_fresh_transition_removal_fault_consumed();
        assert!(error.definitely_not_applied());
        assert_eq!(
            error.outcome(),
            ExactFreshTransitionRemovalOutcome::DefinitelyNotApplied
        );
        assert_eq!(exact_fresh_transition_removal_transaction_attempts(), expected_attempts);
        let observation = fixture
            .database
            .inspect_exact_fresh_transition(fixture.state.id, &fixture.transition)
            .unwrap();
        let ExactFreshTransitionObservation::Present(actual) = observation else {
            panic!("faulted removal did not retain the exact present preimage");
        };
        assert_eq!(actual.state(), &fixture.state);
        assert_eq!(actual.transition_id(), &fixture.transition);
        assert_eq!(actual.metadata_provenance(), &fixture.provenance);
    }
}
