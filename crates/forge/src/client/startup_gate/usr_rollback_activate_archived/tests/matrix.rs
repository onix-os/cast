//! Exact test-sealed ActivateArchived CandidatePreserved route matrix.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::persist_usr_rollback_activate_archived_complete_route_and_reopen,
    },
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::support::{CandidateOutcome, CandidateSource, Epoch, RouteFixture};

#[test]
fn startup_activate_archived_complete_route_covers_all_sixteen_exact_candidate_preserved_cases() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    let case = (epoch, source, usr_outcome, candidate_outcome);
                    let fixture = RouteFixture::new(epoch, source, usr_outcome, candidate_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = fixture.capture_ready(&journal, &reservation);
                    authority.revalidate(&journal).unwrap();
                    let expected = fixture.expected_successor();
                    let canonical_before = fixture.canonical_bytes();
                    let database_before = fixture.database_snapshot();
                    let namespace_before = fixture.namespace_snapshot();

                    let (reopened, actual) =
                        persist_usr_rollback_activate_archived_complete_route_and_reopen(journal, authority).unwrap();

                    assert_eq!(actual, expected, "{case:?}");
                    assert_eq!(actual.phase, Phase::RollbackComplete, "{case:?}");
                    assert_eq!(actual.generation, fixture.source.generation + 1, "{case:?}");
                    assert_eq!(actual.rollback, fixture.source.rollback, "{case:?}");
                    assert_ne!(fixture.canonical_bytes(), canonical_before, "{case:?}");
                    assert_eq!(reopened.load().unwrap(), Some(expected.clone()), "{case:?}");
                    assert_eq!(fixture.canonical_record(), expected, "{case:?}");
                    assert_eq!(fixture.database_snapshot(), database_before, "{case:?}");
                    assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case:?}");
                    fixture.assert_exact_database_pair();
                    fixture.assert_exact_archived_topology();
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 16);
}
