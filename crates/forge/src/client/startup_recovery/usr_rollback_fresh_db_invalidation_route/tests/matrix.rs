use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::persist_usr_rollback_fresh_db_invalidation_route_and_reopen,
    },
    transition_journal::{Phase, RollbackAction, RollbackActionOutcome},
};

use super::support::{CandidateOutcome, CandidateSource, RouteFixture};

fn exercise_success_matrix(candidate_outcome: CandidateOutcome) {
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            let fixture = RouteFixture::new(source, usr_outcome, candidate_outcome);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = fixture.capture_ready(&journal, &reservation);
            let expected = fixture.expected_successor();
            let canonical_before = fixture.canonical_bytes();
            let database_before = fixture.database_snapshot();
            let namespace_before = fixture.namespace_snapshot();

            let (reopened, actual) =
                persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority).unwrap();

            assert_eq!(actual, expected, "{source:?} {usr_outcome:?} {candidate_outcome:?}");
            assert_eq!(actual.phase, Phase::FreshDbInvalidationIntent);
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
        }
    }
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_applied_matrix_persists_exact_intent() {
    exercise_success_matrix(CandidateOutcome::Applied);
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_finish_matrix_persists_exact_intent() {
    exercise_success_matrix(CandidateOutcome::AlreadySatisfied);
}
