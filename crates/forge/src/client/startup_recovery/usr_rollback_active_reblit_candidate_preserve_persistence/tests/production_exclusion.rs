use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_post_exchange_durability_events,
            take_active_reblit_candidate_preserve_post_exchange_durability_events,
        },
        startup_recovery::{
            UsrRollbackCandidatePreserveDispatchError, UsrRollbackCandidatePreserveReady,
            dispatch_usr_rollback_candidate_preserve_and_reopen,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::support::{CandidateOrigin, Source, fixture_for_origin};

#[test]
fn startup_active_reblit_candidate_preserve_persistence_production_dispatch_remains_unsupported_without_advance() {
    let staged_fixture = fixture_for_origin(
        CandidateOrigin::Applied,
        Source::Exchanged,
        RollbackActionOutcome::Applied,
    );
    let staged_before = staged_fixture.evidence_snapshots();
    let staged_journal = staged_fixture.open_journal();
    let staged_reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) =
        staged_fixture.capture(&staged_journal, &staged_reservation)
    else {
        panic!("exact staged ActiveReblit evidence did not admit Apply");
    };
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    reset_active_reblit_candidate_preserve_post_exchange_durability_events();

    let result = dispatch_usr_rollback_candidate_preserve_and_reopen(
        staged_journal,
        staged_fixture.candidate_intent.clone(),
        UsrRollbackCandidatePreserveReady::Apply(authority),
    );
    drop(staged_reservation);
    let error = result.unwrap_err();

    assert!(matches!(error, UsrRollbackCandidatePreserveDispatchError::Unsupported));
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    assert!(take_active_reblit_candidate_preserve_post_exchange_durability_events().is_empty());
    staged_fixture.assert_evidence_unchanged(&staged_before);
    let finish_fixture = fixture_for_origin(
        CandidateOrigin::AlreadySatisfied,
        Source::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
    );
    let finish_before = finish_fixture.evidence_snapshots();
    let finish_journal = finish_fixture.open_journal();
    let finish_reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Finish(authority) =
        finish_fixture.capture(&finish_journal, &finish_reservation)
    else {
        panic!("exact preserved ActiveReblit evidence did not admit Finish");
    };
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    reset_active_reblit_candidate_preserve_post_exchange_durability_events();

    let result = dispatch_usr_rollback_candidate_preserve_and_reopen(
        finish_journal,
        finish_fixture.candidate_intent.clone(),
        UsrRollbackCandidatePreserveReady::Finish(authority),
    );
    drop(finish_reservation);
    let error = result.unwrap_err();

    assert!(matches!(error, UsrRollbackCandidatePreserveDispatchError::Unsupported));
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
    assert!(take_active_reblit_candidate_preserve_post_exchange_durability_events().is_empty());
    finish_fixture.assert_evidence_unchanged(&finish_before);
}
