//! Shared production-leaf wiring for ActivateArchived candidate preservation.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, archived_candidate_preserve_move_attempt_count,
            reset_archived_candidate_preserve_move_attempt_count,
        },
        startup_recovery::{
            UsrRollbackCandidatePreserveDispatchError, UsrRollbackCandidatePreserveReady,
            dispatch_usr_rollback_candidate_preserve_and_reopen,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::{
    super::{
        candidate_test_support::{CandidateLayout, CandidatePreserveFixture, CandidateSource},
        test_fixture::OperationKind,
    },
    support::{CandidateOrigin, Epoch, expected_candidate_preserved, fixture_for_origin},
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

#[test]
fn startup_activate_archived_candidate_preserve_production_leaf_dispatches_all_exact_cases_once() {
    for epoch in Epoch::ALL {
        for origin in CandidateOrigin::ALL {
            for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
                for usr_outcome in USR_OUTCOMES {
                    let fixture = fixture_for_origin(epoch, origin, source, usr_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let ready = ready(&fixture, &journal, &reservation);
                    let expected = expected_candidate_preserved(&fixture, origin);
                    reset_archived_candidate_preserve_move_attempt_count();

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
                        archived_candidate_preserve_move_attempt_count(),
                        usize::from(origin == CandidateOrigin::Applied),
                    );
                }
            }
        }
    }
}

#[test]
fn startup_activate_archived_candidate_preserve_production_leaf_rejects_cross_operation_pairing() {
    for other_kind in [OperationKind::NewState, OperationKind::ActiveReblit] {
        for origin in CandidateOrigin::ALL {
            let archived = fixture_for_origin(
                Epoch::Current,
                origin,
                CandidateSource::Exchanged,
                RollbackActionOutcome::Applied,
            );
            let other = CandidatePreserveFixture::new(
                other_kind,
                CandidateSource::Exchanged,
                RollbackActionOutcome::Applied,
                layout(origin),
            );
            let archived_journal = archived.open_journal();
            let other_journal = other.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let archived_ready = ready(&archived, &archived_journal, &reservation);
            reset_archived_candidate_preserve_move_attempt_count();

            let wrong_source = dispatch_usr_rollback_candidate_preserve_and_reopen(
                other_journal,
                other.candidate_intent.clone(),
                archived_ready,
            );
            drop(reservation);
            drop(archived_journal);

            assert!(matches!(
                wrong_source,
                Err(UsrRollbackCandidatePreserveDispatchError::Authority(_))
            ));
            assert_eq!(archived_candidate_preserve_move_attempt_count(), 0);

            let archived_journal = archived.open_journal();
            let other_journal = other.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let other_ready = ready(&other, &other_journal, &reservation);

            let wrong_authority = dispatch_usr_rollback_candidate_preserve_and_reopen(
                archived_journal,
                archived.candidate_intent.clone(),
                other_ready,
            );
            drop(reservation);

            assert!(matches!(
                wrong_authority,
                Err(UsrRollbackCandidatePreserveDispatchError::Authority(_))
            ));
            assert_eq!(archived_candidate_preserve_move_attempt_count(), 0);
        }
    }
}

fn ready<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &crate::transition_journal::TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> UsrRollbackCandidatePreserveReady<'reservation> {
    match fixture.capture(journal, reservation) {
        UsrRollbackCandidatePreserveAdmission::Apply(authority) => UsrRollbackCandidatePreserveReady::Apply(authority),
        UsrRollbackCandidatePreserveAdmission::Finish(authority) => {
            UsrRollbackCandidatePreserveReady::Finish(authority)
        }
        UsrRollbackCandidatePreserveAdmission::NotApplicable | UsrRollbackCandidatePreserveAdmission::Deferred => {
            panic!("exact candidate-preservation evidence did not admit production dispatch")
        }
    }
}

fn layout(origin: CandidateOrigin) -> CandidateLayout {
    match origin {
        CandidateOrigin::Applied => CandidateLayout::Staged,
        CandidateOrigin::AlreadySatisfied => CandidateLayout::Preserved,
    }
}
