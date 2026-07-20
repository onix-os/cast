//! Direct negative proof for a capability pairing startup cannot construct.

use crate::{
    client::active_state_snapshot::ActiveStateReservation,
    transition_journal::RollbackActionOutcome,
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, WRAPPER_INDEX, assert_complete_route_journal_only,
        assert_exact_no_boot_completion_plan, build_active, capture_complete_route_ready,
        persist_candidate_preserved, reset_complete_route_effect_observers,
    },
};

#[test]
fn startup_active_reblit_complete_route_authority_rejects_reopened_and_cross_root_journal_bindings() {
    let fixture = build_active(
        Epoch::Current,
        CandidateSource::RootLinksComplete,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let record = persist_candidate_preserved(&fixture, CandidateOrigin::Applied);
    let other = build_active(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let _other_record = persist_candidate_preserved(&other, CandidateOrigin::AlreadySatisfied);
    let fixture_database = fixture.fixture.database_snapshot();
    let fixture_namespace = fixture.fixture.namespace_snapshot();
    let other_database = other.fixture.database_snapshot();
    let other_namespace = other.fixture.namespace_snapshot();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert_exact_no_boot_completion_plan(&record, CandidateSource::RootLinksComplete);
    reset_complete_route_effect_observers();

    let authority = capture_complete_route_ready(&fixture, &journal, &reservation, &record);
    assert_eq!(authority.wrapper_index(), WRAPPER_INDEX);
    authority.revalidate(&journal).unwrap();
    drop(journal);

    let reopened_journal = fixture.open_journal();
    let reopened_error = authority.revalidate(&reopened_journal).unwrap_err();
    assert_eq!(
        reopened_error.to_string(),
        "ActiveReblit rollback-completion authority lost its exact journal record binding"
    );
    drop(reopened_journal);

    let other_journal = other.open_journal();
    let error = authority.revalidate(&other_journal).unwrap_err();

    assert_eq!(
        error.to_string(),
        "ActiveReblit rollback-completion authority lost its exact journal record binding"
    );
    assert_eq!(fixture.fixture.canonical_record(), record);
    assert_eq!(fixture.fixture.database_snapshot(), fixture_database);
    assert_eq!(fixture.fixture.namespace_snapshot(), fixture_namespace);
    assert_eq!(other.fixture.database_snapshot(), other_database);
    assert_eq!(other.fixture.namespace_snapshot(), other_namespace);
    assert_complete_route_journal_only();
}
