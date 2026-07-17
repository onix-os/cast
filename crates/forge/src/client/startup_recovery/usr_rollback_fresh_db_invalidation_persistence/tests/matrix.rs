use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::fresh_db_invalidation_removal_call_count,
        startup_recovery::persist_usr_rollback_fresh_db_invalidation_and_reopen,
    },
    transition_journal::{Phase, RollbackAction, RollbackActionOutcome},
};

use super::support::{
    CandidateResult, FreshDbInvalidationOrigin, Source, database_snapshot, effect_authority,
    expected_fresh_db_invalidated, fixture_for_origin, non_journal_namespace_snapshot,
};

fn exercise_success_matrix(origin: FreshDbInvalidationOrigin) {
    for historical in [false, true] {
        for source in Source::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateResult::ALL {
                    let case = (origin, historical, source, usr_outcome, candidate_outcome);
                    let fixture = fixture_for_origin(origin, historical, source, usr_outcome, candidate_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = effect_authority(&fixture, &journal, &reservation, origin);
                    let expected = expected_fresh_db_invalidated(&fixture, origin);
                    let database_before = database_snapshot(&fixture);
                    let namespace_before = non_journal_namespace_snapshot(&fixture);

                    let (reopened, actual) =
                        persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap();

                    assert_eq!(actual, expected, "{case:?}");
                    assert_eq!(actual.phase, Phase::FreshDbInvalidated, "{case:?}");
                    assert_eq!(
                        actual.rollback.as_ref().unwrap().fresh_db,
                        match origin {
                            FreshDbInvalidationOrigin::Applied => RollbackAction::Applied,
                            FreshDbInvalidationOrigin::AlreadySatisfied => RollbackAction::AlreadySatisfied,
                        },
                        "{case:?}"
                    );
                    assert_eq!(reopened.load().unwrap(), Some(expected.clone()), "{case:?}");
                    assert_eq!(fixture.canonical_record(), expected, "{case:?}");
                    assert_eq!(database_snapshot(&fixture), database_before, "{case:?}");
                    assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before, "{case:?}");
                    assert_eq!(
                        fresh_db_invalidation_removal_call_count(),
                        if origin == FreshDbInvalidationOrigin::Applied {
                            1
                        } else {
                            0
                        },
                        "{case:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn startup_fresh_db_invalidation_persistence_applied_matrix_persists_exact_owned_successor() {
    exercise_success_matrix(FreshDbInvalidationOrigin::Applied);
}

#[test]
fn startup_fresh_db_invalidation_persistence_finish_matrix_persists_exact_owned_successor() {
    exercise_success_matrix(FreshDbInvalidationOrigin::AlreadySatisfied);
}

#[test]
fn startup_fresh_db_invalidation_persistence_changes_only_the_canonical_journal() {
    let fixture = fixture_for_origin(
        FreshDbInvalidationOrigin::Applied,
        false,
        Source::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateResult::AlreadySatisfied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = effect_authority(&fixture, &journal, &reservation, FreshDbInvalidationOrigin::Applied);
    let canonical_before = fixture.canonical_bytes();
    let database_before = database_snapshot(&fixture);
    let namespace_before = non_journal_namespace_snapshot(&fixture);

    let (reopened, actual) = persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap();

    assert_ne!(fixture.canonical_bytes(), canonical_before);
    assert_eq!(reopened.load().unwrap(), Some(actual));
    assert_eq!(database_snapshot(&fixture), database_before);
    assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
    assert_eq!(fresh_db_invalidation_removal_call_count(), 1);
}
