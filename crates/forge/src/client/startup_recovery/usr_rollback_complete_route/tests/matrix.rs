use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::fresh_db_invalidation_removal_call_count,
        startup_recovery::persist_usr_rollback_complete_route_and_reopen,
    },
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::support::{CandidateResult, FreshDbOutcome, RouteFixture, Source};

fn exercise_success_matrix(origin: FreshDbOutcome) {
    for historical in [false, true] {
        for source in Source::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateResult::ALL {
                    let case = (origin, historical, source, usr_outcome, candidate_outcome);
                    let fixture = if historical {
                        RouteFixture::historical(origin, source, usr_outcome, candidate_outcome)
                    } else {
                        RouteFixture::new(origin, source, usr_outcome, candidate_outcome)
                    };
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = fixture.capture_ready(&journal, &reservation);
                    authority.revalidate(&journal).unwrap();
                    let expected = fixture.expected_successor();
                    let canonical_before = fixture.canonical_bytes();
                    let database_before = fixture.database_snapshot();
                    let namespace_before = fixture.namespace_snapshot();

                    let (reopened, actual) =
                        persist_usr_rollback_complete_route_and_reopen(journal, authority).unwrap();

                    assert_eq!(actual, expected, "{case:?}");
                    assert_eq!(actual.phase, Phase::RollbackComplete, "{case:?}");
                    assert_eq!(actual.generation, fixture.source.generation + 1, "{case:?}");
                    assert_eq!(actual.rollback, fixture.source.rollback, "{case:?}");
                    assert_eq!(actual.rollback.as_ref().unwrap().fresh_db, origin.action(), "{case:?}");
                    assert_ne!(fixture.canonical_bytes(), canonical_before, "{case:?}");
                    assert_eq!(reopened.load().unwrap(), Some(expected.clone()), "{case:?}");
                    assert_eq!(fixture.canonical_record(), expected, "{case:?}");
                    assert_eq!(fixture.database_snapshot(), database_before, "{case:?}");
                    assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case:?}");
                    assert_eq!(
                        fresh_db_invalidation_removal_call_count(),
                        origin.expected_removals(),
                        "{case:?}"
                    );
                    fixture.assert_no_second_removal();
                }
            }
        }
    }
}

#[test]
fn startup_usr_rollback_complete_route_applied_matrix_persists_exact_rollback_complete() {
    exercise_success_matrix(FreshDbOutcome::Applied);
}

#[test]
fn startup_usr_rollback_complete_route_already_satisfied_matrix_persists_exact_rollback_complete() {
    exercise_success_matrix(FreshDbOutcome::AlreadySatisfied);
}
