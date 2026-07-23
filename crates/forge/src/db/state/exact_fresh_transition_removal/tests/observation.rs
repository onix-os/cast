use super::super::super::{Database, ExactFreshTransitionInspectionError, ExactFreshTransitionObservation};
use super::support::{
    corrupt_transition_text, delete_complete_transition, delete_provenance,
    delete_state_and_provenance_but_leave_selections, delete_state_and_selections_but_leave_provenance, fixture,
    present, transition,
};
use crate::state::Id;

#[test]
fn exact_fresh_inspection_returns_the_complete_present_preimage() {
    let fixture = fixture('1', "complete-present");
    let preimage = present(&fixture);

    assert_eq!(preimage.state(), &fixture.state);
    assert_eq!(preimage.transition_id(), &fixture.transition);
    assert_eq!(preimage.metadata_provenance(), &fixture.provenance);
    assert_eq!(
        fixture
            .database
            .inspect_exact_fresh_transition(fixture.state.id, &fixture.transition)
            .unwrap(),
        ExactFreshTransitionObservation::Present(preimage)
    );
}

#[test]
fn exact_fresh_inspection_returns_only_exact_joint_absence() {
    let database = Database::new(":memory:").unwrap();
    let absent_id = Id::from(99_999);
    let requested = transition('3');

    let observation = database.inspect_exact_fresh_transition(absent_id, &requested).unwrap();
    let ExactFreshTransitionObservation::JointlyAbsent(absence) = observation else {
        panic!("missing state and provenance were not reported jointly absent");
    };
    assert_eq!(absence.state_id(), absent_id);
    assert_eq!(absence.transition_id(), &requested);
}

#[test]
fn exact_fresh_inspection_refuses_orphan_selections_token_rebinding_and_multiple_inflight_rows() {
    let orphan = fixture('a', "orphan-selections");
    delete_state_and_provenance_but_leave_selections(&orphan.database, orphan.state.id);
    assert!(matches!(
        orphan
            .database
            .inspect_exact_fresh_transition(orphan.state.id, &orphan.transition),
        Err(ExactFreshTransitionInspectionError::OrphanSelections { state_id, count })
            if state_id == i32::from(orphan.state.id) && count == 2
    ));

    let rebound = fixture('b', "rebound-original");
    let original_id = rebound.state.id;
    delete_complete_transition(&rebound.database, original_id);
    let rebound_state = rebound
        .database
        .add_with_transition(&rebound.transition, &[], Some("rebound owner"), None)
        .unwrap();
    assert!(matches!(
        rebound
            .database
            .inspect_exact_fresh_transition(original_id, &rebound.transition),
        Err(ExactFreshTransitionInspectionError::UnexpectedInFlightTransition {
            expected_state_id,
            actual_state_id,
            ..
        }) if expected_state_id == i32::from(original_id)
            && actual_state_id == i32::from(rebound_state.id)
    ));

    let database = Database::new(":memory:").unwrap();
    let target = super::support::fixture_in(database, 'c', "multiple-target");
    let _other = super::support::fixture_in(target.database.clone(), 'd', "multiple-other");
    assert!(matches!(
        target
            .database
            .inspect_exact_fresh_transition(target.state.id, &target.transition),
        Err(ExactFreshTransitionInspectionError::MultipleInFlightTransitions { .. })
    ));
}

#[test]
fn exact_fresh_inspection_refuses_cleared_foreign_and_split_provenance_states() {
    let cleared = fixture('4', "cleared");
    cleared
        .database
        .clear_transition_if_matches(cleared.state.id, &cleared.transition)
        .unwrap();
    assert!(matches!(
        cleared
            .database
            .inspect_exact_fresh_transition(cleared.state.id, &cleared.transition),
        Err(ExactFreshTransitionInspectionError::ClearedTransition { state_id })
            if state_id == i32::from(cleared.state.id)
    ));

    let foreign = fixture('5', "foreign");
    let requested = transition('6');
    assert!(matches!(
        foreign
            .database
            .inspect_exact_fresh_transition(foreign.state.id, &requested),
        Err(ExactFreshTransitionInspectionError::ForeignTransition {
            state_id,
            expected,
            actual,
        }) if state_id == i32::from(foreign.state.id)
            && expected == requested
            && actual == foreign.transition
    ));

    let missing = fixture('7', "missing-provenance");
    delete_provenance(&missing.database, missing.state.id);
    assert!(matches!(
        missing
            .database
            .inspect_exact_fresh_transition(missing.state.id, &missing.transition),
        Err(ExactFreshTransitionInspectionError::MissingProvenance { state_id })
            if state_id == i32::from(missing.state.id)
    ));

    let orphan = fixture('8', "orphan-provenance");
    delete_state_and_selections_but_leave_provenance(&orphan.database, orphan.state.id);
    assert!(matches!(
        orphan
            .database
            .inspect_exact_fresh_transition(orphan.state.id, &orphan.transition),
        Err(ExactFreshTransitionInspectionError::OrphanProvenance { state_id })
            if state_id == i32::from(orphan.state.id)
    ));
}

#[test]
fn exact_fresh_inspection_rejects_malformed_transition_evidence() {
    let fixture = fixture('9', "malformed-transition");
    corrupt_transition_text(&fixture.database, fixture.state.id);

    assert!(matches!(
        fixture
            .database
            .inspect_exact_fresh_transition(fixture.state.id, &fixture.transition),
        Err(ExactFreshTransitionInspectionError::TransitionEvidence(_))
    ));
}
