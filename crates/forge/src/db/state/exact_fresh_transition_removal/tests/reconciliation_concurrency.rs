use super::super::super::{
    ExactFreshTransitionInspectionError, ExactFreshTransitionObservation, ExactFreshTransitionRemovalFault,
    ExactFreshTransitionRemovalOutcome, arm_after_exact_fresh_transition_removal_attempt_before_reconciliation,
    arm_exact_fresh_transition_removal_fault, assert_exact_fresh_transition_removal_fault_consumed,
    exact_fresh_transition_removal_transaction_attempts,
};
use super::support::{
    RelationCounts, delete_complete_transition, fixture, normalize_created, present, relation_counts,
    replace_with_changed_preimage, transition,
};

#[test]
fn exact_fresh_preimage_and_absence_are_bound_to_their_database_capability() {
    let mut first = fixture('6', "database-lookalike");
    let mut second = fixture('6', "database-lookalike");
    normalize_created(&mut first, 1_700_000_000);
    normalize_created(&mut second, 1_700_000_000);
    assert_eq!(first.state, second.state);
    assert_eq!(first.transition, second.transition);
    assert_eq!(first.provenance, second.provenance);

    let first_preimage = present(&first);
    let error = second
        .database
        .remove_exact_fresh_transition(first_preimage)
        .unwrap_err();

    assert!(error.definitely_not_applied());
    assert_eq!(
        error.outcome(),
        ExactFreshTransitionRemovalOutcome::DefinitelyNotApplied
    );
    assert_eq!(exact_fresh_transition_removal_transaction_attempts(), 0);
    let first_actual = present(&first);
    let second_actual = present(&second);
    assert_eq!(first_actual.state(), second_actual.state());
    assert_eq!(first_actual.transition_id(), second_actual.transition_id());
    assert_eq!(first_actual.metadata_provenance(), second_actual.metadata_provenance());
    assert_ne!(first_actual, second_actual);

    let absent_id = crate::state::Id::from(88_888);
    let absent_transition = transition('7');
    let first_empty = super::super::super::Database::new(":memory:").unwrap();
    let second_empty = super::super::super::Database::new(":memory:").unwrap();
    let first_absence = first_empty
        .inspect_exact_fresh_transition(absent_id, &absent_transition)
        .unwrap();
    let second_absence = second_empty
        .inspect_exact_fresh_transition(absent_id, &absent_transition)
        .unwrap();
    assert_ne!(first_absence, second_absence);
}

#[test]
fn exact_fresh_removal_after_commit_error_reconciles_joint_absence_as_success() {
    let fixture = fixture('1', "after-commit");
    let preimage = present(&fixture);
    arm_exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::AfterCommit);

    let absence = fixture.database.remove_exact_fresh_transition(preimage).unwrap();

    assert_exact_fresh_transition_removal_fault_consumed();
    assert_eq!(exact_fresh_transition_removal_transaction_attempts(), 1);
    assert_eq!(absence.state_id(), fixture.state.id);
    assert_eq!(absence.transition_id(), &fixture.transition);
    assert!(matches!(
        fixture
            .database
            .inspect_exact_fresh_transition(fixture.state.id, &fixture.transition),
        Ok(ExactFreshTransitionObservation::JointlyAbsent(actual)) if actual == absence
    ));
}

#[test]
fn exact_fresh_removal_rolled_back_then_external_absence_is_definitely_not_applied() {
    let fixture = fixture('8', "rolled-back-external-absence");
    let preimage = present(&fixture);
    let external_database = fixture.database.clone();
    let state_id = fixture.state.id;
    arm_exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::BeforeCommit);
    arm_after_exact_fresh_transition_removal_attempt_before_reconciliation(move || {
        delete_complete_transition(&external_database, state_id);
    });

    let error = fixture.database.remove_exact_fresh_transition(preimage).unwrap_err();

    assert_exact_fresh_transition_removal_fault_consumed();
    assert!(error.definitely_not_applied());
    assert_eq!(
        error.outcome(),
        ExactFreshTransitionRemovalOutcome::DefinitelyNotApplied
    );
    assert_eq!(exact_fresh_transition_removal_transaction_attempts(), 1);
    assert_eq!(
        relation_counts(&fixture.database, fixture.state.id),
        RelationCounts {
            states: 0,
            selections: 0,
            provenance: 0,
        }
    );
    assert!(matches!(
        fixture
            .database
            .inspect_exact_fresh_transition(fixture.state.id, &fixture.transition),
        Ok(ExactFreshTransitionObservation::JointlyAbsent(_))
    ));
}

#[test]
fn exact_fresh_removal_uncertain_report_with_joint_absence_is_ambiguous() {
    let fixture = fixture('9', "uncertain-report-joint-absence");
    let preimage = present(&fixture);
    arm_exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::AfterCommitWithUncertainReport);

    let error = fixture.database.remove_exact_fresh_transition(preimage).unwrap_err();

    assert_exact_fresh_transition_removal_fault_consumed();
    assert_eq!(error.outcome(), ExactFreshTransitionRemovalOutcome::Ambiguous);
    assert!(!error.definitely_not_applied());
    assert!(
        error
            .to_string()
            .contains("does not prove this invocation committed their removal")
    );
    assert_eq!(exact_fresh_transition_removal_transaction_attempts(), 1);
    assert_eq!(
        relation_counts(&fixture.database, fixture.state.id),
        RelationCounts {
            states: 0,
            selections: 0,
            provenance: 0,
        }
    );
    assert!(matches!(
        fixture
            .database
            .inspect_exact_fresh_transition(fixture.state.id, &fixture.transition),
        Ok(ExactFreshTransitionObservation::JointlyAbsent(_))
    ));
}

#[test]
fn exact_fresh_removal_partial_and_changed_post_error_states_are_ambiguous() {
    for (digit, fault) in [
        ('2', ExactFreshTransitionRemovalFault::AfterCommitWithPartialRestoration),
        ('3', ExactFreshTransitionRemovalFault::AfterCommitWithChangedRestoration),
    ] {
        let fixture = fixture(digit, &format!("ambiguous-{fault:?}"));
        let preimage = present(&fixture);
        arm_exact_fresh_transition_removal_fault(fault);

        let error = fixture.database.remove_exact_fresh_transition(preimage).unwrap_err();

        assert_exact_fresh_transition_removal_fault_consumed();
        assert_eq!(error.outcome(), ExactFreshTransitionRemovalOutcome::Ambiguous);
        assert!(!error.definitely_not_applied());
        assert_eq!(exact_fresh_transition_removal_transaction_attempts(), 1);
        match fault {
            ExactFreshTransitionRemovalFault::AfterCommitWithPartialRestoration => {
                assert!(matches!(
                    fixture
                        .database
                        .inspect_exact_fresh_transition(fixture.state.id, &fixture.transition),
                    Err(ExactFreshTransitionInspectionError::MissingProvenance { .. })
                ));
            }
            ExactFreshTransitionRemovalFault::AfterCommitWithChangedRestoration => {
                assert!(matches!(
                    fixture
                        .database
                        .inspect_exact_fresh_transition(fixture.state.id, &fixture.transition),
                    Ok(ExactFreshTransitionObservation::Present(actual))
                        if actual.state() != &fixture.state
                ));
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn exact_fresh_removal_exact_post_commit_restoration_is_ambiguous_aba() {
    let fixture = fixture('5', "exact-aba-restoration");
    let preimage = present(&fixture);
    arm_exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::AfterCommitWithExactRestoration);

    let error = fixture.database.remove_exact_fresh_transition(preimage).unwrap_err();

    assert_exact_fresh_transition_removal_fault_consumed();
    assert_eq!(error.outcome(), ExactFreshTransitionRemovalOutcome::Ambiguous);
    assert!(!error.definitely_not_applied());
    assert_eq!(exact_fresh_transition_removal_transaction_attempts(), 1);
    let observation = fixture
        .database
        .inspect_exact_fresh_transition(fixture.state.id, &fixture.transition)
        .unwrap();
    let ExactFreshTransitionObservation::Present(actual) = observation else {
        panic!("exact ABA restoration did not retain a complete present preimage");
    };
    assert_eq!(actual.state(), &fixture.state);
    assert_eq!(actual.transition_id(), &fixture.transition);
    assert_eq!(actual.metadata_provenance(), &fixture.provenance);
}

#[test]
fn exact_fresh_removal_stale_preimage_cannot_delete_an_independent_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let database = super::super::super::Database::new(path.to_str().unwrap()).unwrap();
    let fixture = super::support::fixture_in(database, '4', "original");
    let preimage = present(&fixture);

    let independent = super::super::super::Database::new(path.to_str().unwrap()).unwrap();
    replace_with_changed_preimage(&independent, &preimage);
    let expected_counts = relation_counts(&independent, fixture.state.id);

    let error = fixture.database.remove_exact_fresh_transition(preimage).unwrap_err();

    assert_eq!(error.outcome(), ExactFreshTransitionRemovalOutcome::Ambiguous);
    assert_eq!(relation_counts(&independent, fixture.state.id), expected_counts);
    assert!(matches!(
        independent.inspect_exact_fresh_transition(fixture.state.id, &fixture.transition),
        Ok(ExactFreshTransitionObservation::Present(actual))
            if actual.state().summary.as_deref() == Some("replacement state")
    ));
}
