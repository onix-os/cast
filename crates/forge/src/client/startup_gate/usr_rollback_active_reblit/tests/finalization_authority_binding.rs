//! Direct negative proof for authority/store pairings startup cannot construct.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackActiveReblitFinalizationSeal,
        startup_reconciliation::{
            UsrRollbackActiveReblitFinalizationAdmission, UsrRollbackActiveReblitFinalizationAuthority,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, WRAPPER_INDICES, assert_no_candidate_effects, build_active_at_wrapper_index,
        persist_rollback_complete, reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_finalization_authority_covers_both_indices_in_both_epochs_and_rejects_wrong_bindings() {
    for epoch in Epoch::ALL {
        for wrapper_index in WRAPPER_INDICES {
            let fixture = build_active_at_wrapper_index(
                epoch,
                CandidateSource::Exchanged,
                RollbackActionOutcome::Applied,
                CandidateOrigin::AlreadySatisfied,
                wrapper_index,
            );
            let terminal = persist_rollback_complete(&fixture, CandidateOrigin::Applied);
            let other = build_active_at_wrapper_index(
                Epoch::Historical,
                CandidateSource::Intent,
                RollbackActionOutcome::AlreadySatisfied,
                CandidateOrigin::AlreadySatisfied,
                wrapper_index,
            );
            let _other_terminal = persist_rollback_complete(&other, CandidateOrigin::AlreadySatisfied);
            let database_before = fixture.fixture.database_snapshot();
            let namespace_before = fixture.fixture.namespace_snapshot();
            let other_database = other.fixture.database_snapshot();
            let other_namespace = other.fixture.namespace_snapshot();
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let seal = UsrRollbackActiveReblitFinalizationSeal::new_for_test();
            reset_candidate_effect_observers();

            let admission = UsrRollbackActiveReblitFinalizationAuthority::capture(
                &seal,
                &fixture.fixture.installation,
                &journal,
                &fixture.fixture.database,
                &reservation,
                &terminal,
            )
            .unwrap();
            let UsrRollbackActiveReblitFinalizationAdmission::Ready(authority) = admission else {
                panic!("exact terminal ActiveReblit evidence did not admit finalization");
            };
            assert_eq!(authority.wrapper_index(), wrapper_index);
            authority.revalidate(&journal).unwrap();
            drop(journal);

            let reopened = fixture.open_journal();
            let reopened_error = authority.revalidate(&reopened).unwrap_err();
            assert_eq!(
                reopened_error.to_string(),
                "ActiveReblit rollback-finalization authority was paired with a different open journal store"
            );
            drop(reopened);

            let other_journal = other.open_journal();
            let cross_root_error = authority.revalidate(&other_journal).unwrap_err();
            assert_eq!(
                cross_root_error.to_string(),
                "ActiveReblit rollback-finalization authority was paired with a different open journal store"
            );
            assert_eq!(fixture.fixture.canonical_record(), terminal);
            assert_eq!(fixture.fixture.database_snapshot(), database_before);
            assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
            assert_eq!(other.fixture.database_snapshot(), other_database);
            assert_eq!(other.fixture.namespace_snapshot(), other_namespace);
            assert_no_candidate_effects();
        }
    }
}

#[test]
fn startup_active_reblit_finalization_authority_refuses_terminal_candidate_without_exact_previous_identity() {
    let fixture = build_active_at_wrapper_index(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
        0,
    );
    let terminal = persist_rollback_complete(&fixture, CandidateOrigin::Applied);
    let mut wrong_candidate = terminal.clone();
    wrong_candidate.candidate.id = None;
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackActiveReblitFinalizationSeal::new_for_test();
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let admission = UsrRollbackActiveReblitFinalizationAuthority::capture(
        &seal,
        &fixture.fixture.installation,
        &journal,
        &fixture.fixture.database,
        &reservation,
        &wrong_candidate,
    )
    .unwrap();

    assert!(matches!(
        admission,
        UsrRollbackActiveReblitFinalizationAdmission::Deferred
    ));
    assert_eq!(fixture.fixture.canonical_record(), terminal);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();
}
