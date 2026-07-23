use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        boot::{boot_synchronize_attempt_count, reset_boot_synchronize_attempt_count},
        startup_reconciliation::{
            fresh_db_invalidation_removal_call_count, new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count,
        },
        startup_recovery::persist_usr_rollback_fresh_db_invalidation_route_and_reopen,
    },
    transition_identity::retained_exchange_syscall_count,
    transition_journal::{Operation, Phase, RollbackAction, RollbackActionOutcome},
};

use super::support::{CandidateOutcome, CandidateSource, RouteFixture};

fn exercise_success_matrix(selected_candidate_outcome: CandidateOutcome) {
    for historical in [false, true] {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    if candidate_outcome == selected_candidate_outcome {
                        exercise_success_case(historical, source, usr_outcome, candidate_outcome);
                    }
                }
            }
        }
    }
}

fn exercise_success_case(
    historical: bool,
    source: CandidateSource,
    usr_outcome: RollbackActionOutcome,
    candidate_outcome: CandidateOutcome,
) {
    let fixture = RouteFixture::at_epoch(historical, source, usr_outcome, candidate_outcome);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let expected = fixture.expected_successor();
    let canonical_before = fixture.canonical_bytes();
    let database_before = fixture.database_snapshot();
    let namespace_before = fixture.namespace_snapshot();
    reset_new_state_candidate_preserve_move_attempt_count();
    reset_boot_synchronize_attempt_count();
    let removal_before = fresh_db_invalidation_removal_call_count();
    let exchange_before = retained_exchange_syscall_count();

    let (reopened, actual) = persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority).unwrap();

    assert_eq!(actual, expected, "{source:?} {usr_outcome:?} {candidate_outcome:?}");
    assert_eq!(actual.phase, Phase::FreshDbInvalidationIntent);
    assert_eq!(actual.operation, Operation::NewState);
    assert_eq!(actual.generation, fixture.source.generation + 1);
    let rollback = actual.rollback.as_ref().unwrap();
    assert_eq!(
        rollback.usr_exchange,
        match usr_outcome {
            RollbackActionOutcome::Applied => RollbackAction::Applied,
            RollbackActionOutcome::AlreadySatisfied => RollbackAction::AlreadySatisfied,
        }
    );
    assert_eq!(
        rollback.candidate.action,
        match candidate_outcome {
            CandidateOutcome::Applied => RollbackAction::Applied,
            CandidateOutcome::AlreadySatisfied => RollbackAction::AlreadySatisfied,
        }
    );
    assert_eq!(rollback.fresh_db, RollbackAction::Pending);
    assert_ne!(fixture.canonical_bytes(), canonical_before);
    assert_eq!(reopened.load().unwrap(), Some(expected.clone()));
    assert_eq!(fixture.canonical_record(), expected);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
    assert_eq!(fresh_db_invalidation_removal_call_count(), removal_before);
    assert_eq!(retained_exchange_syscall_count(), exchange_before);
    assert_eq!(boot_synchronize_attempt_count(), 0);
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_applied_matrix_persists_exact_intent() {
    exercise_success_matrix(CandidateOutcome::Applied);
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_finish_matrix_persists_exact_intent() {
    exercise_success_matrix(CandidateOutcome::AlreadySatisfied);
}
