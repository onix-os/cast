use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::persist_usr_rollback_candidate_preserve_and_reopen,
    },
    transition_journal::{Phase, RollbackAction, RollbackActionOutcome},
};

use super::support::{
    CandidateOrigin, Source, durable_authority, expected_candidate_preserved, fixture_for_origin,
    fixture_for_origin_at_epoch,
    non_journal_namespace_snapshot,
};

fn exercise_success_matrix(origin: CandidateOrigin) {
    for historical in [false, true] {
        for source in Source::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                let fixture = fixture_for_origin_at_epoch(historical, origin, source, usr_outcome);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let authority = durable_authority(&fixture, &journal, &reservation, origin);
                let expected = expected_candidate_preserved(&fixture, origin);
                let database_before = fixture.fixture.database_snapshot();
                let namespace_before = non_journal_namespace_snapshot(&fixture);

                let (reopened, actual) =
                    persist_usr_rollback_candidate_preserve_and_reopen(journal, authority).unwrap();

                assert_eq!(
                    actual, expected,
                    "{origin:?} {source:?} {usr_outcome:?} historical={historical}"
                );
                assert_eq!(actual.phase, Phase::CandidatePreserved);
                assert_eq!(
                    actual.rollback.as_ref().unwrap().candidate.action,
                    match origin {
                        CandidateOrigin::Applied => RollbackAction::Applied,
                        CandidateOrigin::AlreadySatisfied => RollbackAction::AlreadySatisfied,
                    }
                );
                assert_eq!(actual.rollback.as_ref().unwrap().fresh_db, RollbackAction::Pending);
                assert_eq!(reopened.load().unwrap(), Some(expected.clone()));
                assert_eq!(fixture.fixture.canonical_record(), expected);
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
            }
        }
    }
}

#[test]
fn startup_usr_rollback_candidate_preserve_persistence_applied_matrix_persists_exact_successor() {
    exercise_success_matrix(CandidateOrigin::Applied);
}

#[test]
fn startup_usr_rollback_candidate_preserve_persistence_finish_matrix_persists_exact_successor() {
    exercise_success_matrix(CandidateOrigin::AlreadySatisfied);
}

#[test]
fn startup_usr_rollback_candidate_preserve_persistence_changes_only_the_canonical_journal() {
    let fixture = fixture_for_origin(
        CandidateOrigin::Applied,
        Source::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = durable_authority(&fixture, &journal, &reservation, CandidateOrigin::Applied);
    let canonical_before = fixture.fixture.canonical_bytes();
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = non_journal_namespace_snapshot(&fixture);

    let (reopened, actual) = persist_usr_rollback_candidate_preserve_and_reopen(journal, authority).unwrap();

    assert_ne!(fixture.fixture.canonical_bytes(), canonical_before);
    assert_eq!(reopened.load().unwrap(), Some(actual));
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
}
