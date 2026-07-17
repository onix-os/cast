//! Direct negative proof for a capability pairing startup cannot construct.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackActiveReblitCompleteRouteSeal,
        startup_reconciliation::{
            UsrRollbackActiveReblitCompleteRouteAdmission, UsrRollbackActiveReblitCompleteRouteAuthority,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, WRAPPER_INDEX, assert_no_candidate_effects, build_active, persist_candidate_preserved,
        reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_complete_route_authority_rejects_reopened_and_cross_root_journal_bindings() {
    let fixture = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
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
    let seal = UsrRollbackActiveReblitCompleteRouteSeal::new_for_test();
    reset_candidate_effect_observers();

    let admission = UsrRollbackActiveReblitCompleteRouteAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        &journal,
        &fixture.fixture.database,
        &reservation,
        &record,
    )
    .unwrap();
    let UsrRollbackActiveReblitCompleteRouteAdmission::Ready(authority) = admission else {
        panic!("exact source-root CandidatePreserved evidence did not admit completion routing");
    };
    assert_eq!(authority.wrapper_index(), WRAPPER_INDEX);
    authority.revalidate(&journal).unwrap();
    drop(journal);

    let reopened_journal = fixture.open_journal();
    let reopened_error = authority.revalidate(&reopened_journal).unwrap_err();
    assert_eq!(
        reopened_error.to_string(),
        "ActiveReblit rollback-completion authority was paired with a different open journal store"
    );
    drop(reopened_journal);

    let other_journal = other.open_journal();
    let error = authority.revalidate(&other_journal).unwrap_err();

    assert_eq!(
        error.to_string(),
        "ActiveReblit rollback-completion authority was paired with a different open journal store"
    );
    assert_eq!(fixture.fixture.canonical_record(), record);
    assert_eq!(fixture.fixture.database_snapshot(), fixture_database);
    assert_eq!(fixture.fixture.namespace_snapshot(), fixture_namespace);
    assert_eq!(other.fixture.database_snapshot(), other_database);
    assert_eq!(other.fixture.namespace_snapshot(), other_namespace);
    assert_no_candidate_effects();
}
