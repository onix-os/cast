//! Production candidate-preserve leaf wiring for ActiveReblit.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
        },
        startup_recovery::{
            DurableUsrRollbackActiveReblitCandidatePreserveRecord,
            UsrRollbackActiveReblitCandidatePreservePersistenceError, UsrRollbackCandidatePreserveDispatchError,
            UsrRollbackCandidatePreserveReady, dispatch_usr_rollback_candidate_preserve_and_reopen,
        },
    },
    transition_journal::{RollbackActionOutcome, arm_next_temporary_sync_fault, assert_temporary_sync_fault_consumed},
};

use super::support::{CandidateOrigin, Epoch, Source, expected_candidate_preserved, fixture_for_origin};

#[test]
fn startup_active_reblit_candidate_preserve_production_leaf_dispatches_applied_and_finish_exactly_once() {
    let mut exercised = 0;
    for epoch in Epoch::ALL {
        for origin in CandidateOrigin::ALL {
            for source in Source::ALL {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                let fixture = fixture_for_origin(epoch, origin, source, usr_outcome);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let ready = match fixture.capture(&journal, &reservation) {
                    UsrRollbackCandidatePreserveAdmission::Apply(authority) => {
                        UsrRollbackCandidatePreserveReady::Apply(authority)
                    }
                    UsrRollbackCandidatePreserveAdmission::Finish(authority) => {
                        UsrRollbackCandidatePreserveReady::Finish(authority)
                    }
                    UsrRollbackCandidatePreserveAdmission::NotApplicable
                    | UsrRollbackCandidatePreserveAdmission::Deferred => {
                        panic!("exact ActiveReblit evidence did not admit candidate preservation")
                    }
                };
                reset_active_reblit_candidate_preserve_exchange_attempt_count();
                let expected = expected_candidate_preserved(&fixture, origin);

                let result = dispatch_usr_rollback_candidate_preserve_and_reopen(
                    journal,
                    fixture.candidate_intent.clone(),
                    ready,
                );
                drop(reservation);
                let (reopened, actual) = result.unwrap();

                assert_eq!(actual, expected);
                assert_eq!(reopened.load().unwrap(), Some(expected));
                assert_eq!(
                    active_reblit_candidate_preserve_exchange_attempt_count(),
                    usize::from(origin == CandidateOrigin::Applied)
                );
                exercised += 1;
                }
            }
        }
    }
    assert_eq!(exercised, 24);
}

#[test]
fn startup_active_reblit_candidate_preserve_production_leaf_source_fault_restarts_finish_without_second_exchange() {
    let fixture = fixture_for_origin(
        Epoch::Current,
        CandidateOrigin::Applied,
        Source::Exchanged,
        RollbackActionOutcome::Applied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("exact staged ActiveReblit evidence did not admit Apply");
    };
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    arm_next_temporary_sync_fault();

    let first = dispatch_usr_rollback_candidate_preserve_and_reopen(
        journal,
        fixture.candidate_intent.clone(),
        UsrRollbackCandidatePreserveReady::Apply(authority),
    );
    drop(reservation);
    let error = first.unwrap_err();

    assert_temporary_sync_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackCandidatePreserveDispatchError::ActiveReblitPersistence(
            UsrRollbackActiveReblitCandidatePreservePersistenceError::Advance {
                durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source,
                ..
            }
        )
    ));
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
    assert_eq!(fixture.fixture.canonical_record(), fixture.candidate_intent);

    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Finish(authority) = fixture.capture(&journal, &reservation) else {
        panic!("preserved ActiveReblit evidence did not admit Finish after restart");
    };
    let expected = expected_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);

    let result = dispatch_usr_rollback_candidate_preserve_and_reopen(
        journal,
        fixture.candidate_intent.clone(),
        UsrRollbackCandidatePreserveReady::Finish(authority),
    );
    drop(reservation);
    let (reopened, actual) = result.unwrap();

    assert_eq!(actual, expected);
    assert_eq!(reopened.load().unwrap(), Some(expected));
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
}
