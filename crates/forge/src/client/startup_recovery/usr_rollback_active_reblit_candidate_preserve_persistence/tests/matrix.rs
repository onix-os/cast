use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
        },
        startup_recovery::persist_usr_rollback_active_reblit_candidate_preserve_and_reopen,
    },
    transition_journal::{Phase, RollbackAction, RollbackActionOutcome},
};

use super::support::{
    CandidateOrigin, Epoch, Source, durable_authority, expected_candidate_preserved, fixture_for_origin,
    non_journal_namespace_snapshot,
};

fn exercise_success_matrix(origin: CandidateOrigin) {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in Source::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                let fixture = fixture_for_origin(epoch, origin, source, usr_outcome);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_active_reblit_candidate_preserve_exchange_attempt_count();
                let authority = durable_authority(&fixture, &journal, &reservation, origin);
                let effect_count_before = active_reblit_candidate_preserve_exchange_attempt_count();
                assert_eq!(effect_count_before, usize::from(origin == CandidateOrigin::Applied));
                let expected = expected_candidate_preserved(&fixture, origin);
                let database_before = fixture.fixture.database_snapshot();
                let namespace_before = non_journal_namespace_snapshot(&fixture);

                let result = persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(journal, authority);
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
                assert_eq!(actual.rollback.as_ref().unwrap().fresh_db, RollbackAction::NotRequired);
                assert_eq!(reopened.load().unwrap(), Some(expected.clone()));
                assert_eq!(fixture.fixture.canonical_record(), expected);
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
                assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), effect_count_before);
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 12);
}

#[test]
fn startup_active_reblit_candidate_preserve_persistence_applied_matrix_persists_exact_successor() {
    exercise_success_matrix(CandidateOrigin::Applied);
}

#[test]
fn startup_active_reblit_candidate_preserve_persistence_finish_matrix_persists_exact_successor() {
    exercise_success_matrix(CandidateOrigin::AlreadySatisfied);
}

#[test]
fn startup_active_reblit_candidate_preserve_persistence_changes_only_the_canonical_journal() {
    let fixture = fixture_for_origin(
        Epoch::Current,
        CandidateOrigin::Applied,
        Source::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    let authority = durable_authority(&fixture, &journal, &reservation, CandidateOrigin::Applied);
    let canonical_before = fixture.fixture.canonical_bytes();
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = non_journal_namespace_snapshot(&fixture);

    let result = persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(journal, authority);
    drop(reservation);
    let (reopened, actual) = result.unwrap();

    assert_ne!(fixture.fixture.canonical_bytes(), canonical_before);
    assert_eq!(reopened.load().unwrap(), Some(actual));
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
}
