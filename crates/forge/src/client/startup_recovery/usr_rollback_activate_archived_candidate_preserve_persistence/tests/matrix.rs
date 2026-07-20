//! Exact persistence matrix across epochs, rollback sources, and outcomes.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            archived_candidate_preserve_move_attempt_count, reset_archived_candidate_preserve_move_attempt_count,
        },
        startup_recovery::persist_usr_rollback_archived_candidate_preserve_and_reopen,
    },
    transition_journal::{Phase, RollbackAction, RollbackActionOutcome},
};

use super::super::candidate_test_support::CandidateSource;
use super::support::{
    CandidateOrigin, Epoch, assert_preserved, durable_authority, expected_candidate_preserved, fixture_for_origin,
    non_journal_namespace_snapshot,
};

fn exercise_matrix(origin: CandidateOrigin) {
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                let fixture = fixture_for_origin(epoch, origin, source, usr_outcome);
                let database_before = fixture.fixture.database_snapshot();
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_archived_candidate_preserve_move_attempt_count();
                let authority = durable_authority(&fixture, &journal, &reservation, origin);
                let namespace_before = non_journal_namespace_snapshot(&fixture);
                let expected = expected_candidate_preserved(&fixture, origin);

                let result = persist_usr_rollback_archived_candidate_preserve_and_reopen(journal, authority);
                drop(reservation);
                let (reopened, actual) = result.unwrap();

                assert_eq!(actual, expected, "{epoch:?} {origin:?} {source:?} {usr_outcome:?}");
                assert_eq!(actual.phase, Phase::CandidatePreserved);
                assert_eq!(
                    actual.rollback.as_ref().unwrap().candidate.action,
                    match origin {
                        CandidateOrigin::Applied => RollbackAction::Applied,
                        CandidateOrigin::AlreadySatisfied => RollbackAction::AlreadySatisfied,
                    }
                );
                assert_eq!(
                    actual.rollback.as_ref().unwrap().usr_exchange,
                    match usr_outcome {
                        RollbackActionOutcome::Applied => RollbackAction::Applied,
                        RollbackActionOutcome::AlreadySatisfied => RollbackAction::AlreadySatisfied,
                    }
                );
                assert_eq!(reopened.load().unwrap(), Some(expected.clone()));
                assert_eq!(fixture.fixture.canonical_record(), expected);
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
                assert_eq!(
                    archived_candidate_preserve_move_attempt_count(),
                    usize::from(origin == CandidateOrigin::Applied)
                );
                assert_preserved(&fixture);
            }
        }
    }
}

#[test]
fn startup_archived_candidate_preserve_persistence_applied_matrix_persists_exact_successor() {
    exercise_matrix(CandidateOrigin::Applied);
}

#[test]
fn startup_archived_candidate_preserve_persistence_finish_matrix_persists_exact_successor() {
    exercise_matrix(CandidateOrigin::AlreadySatisfied);
}

#[test]
fn startup_archived_candidate_preserve_persistence_changes_only_journal_after_durability() {
    let fixture = fixture_for_origin(
        Epoch::Historical,
        CandidateOrigin::Applied,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = durable_authority(&fixture, &journal, &reservation, CandidateOrigin::Applied);
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = non_journal_namespace_snapshot(&fixture);
    let canonical_before = fixture.fixture.canonical_bytes();

    let result = persist_usr_rollback_archived_candidate_preserve_and_reopen(journal, authority);
    drop(reservation);
    let (reopened, actual) = result.unwrap();

    assert_ne!(fixture.fixture.canonical_bytes(), canonical_before);
    assert_eq!(reopened.load().unwrap(), Some(actual));
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
    assert_preserved(&fixture);
}
