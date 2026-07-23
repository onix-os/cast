use crate::{
    client::{active_state_snapshot::ActiveStateReservation, startup_recovery::finalize_usr_rollback},
    transition_journal::RollbackActionOutcome,
};

use super::support::{CandidateResult, FinalizationFixture, FreshDbOutcome, Source};

#[test]
fn startup_usr_rollback_finalization_success_matrix_retains_exact_canonical_absence() {
    let mut cases = 0;
    for historical in [false, true] {
        for origin in FreshDbOutcome::ALL {
            for source in Source::THROUGH_ROLLBACK_COMPLETE {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateResult::ALL {
                        let case = (historical, origin, source, usr_outcome, candidate_outcome);
                        let fixture = if historical {
                            FinalizationFixture::historical(origin, source, usr_outcome, candidate_outcome)
                        } else {
                            FinalizationFixture::new(origin, source, usr_outcome, candidate_outcome)
                        };
                        let journal = fixture.open_journal();
                        let binding = journal.binding();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = fixture.capture_ready(&journal, &reservation);
                        let database_before = fixture.database_snapshot();
                        let namespace_before = fixture.namespace_snapshot();

                        let retained = finalize_usr_rollback(journal, authority).unwrap();

                        if source == Source::RootLinksComplete {
                            assert_eq!(fixture.source.generation, 18, "{case:?}");
                        }
                        assert!(retained.has_binding(&binding), "{case:?}");
                        assert_eq!(retained.load().unwrap(), None, "{case:?}");
                        assert_eq!(fixture.database_snapshot(), database_before, "{case:?}");
                        assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case:?}");
                        fixture.assert_no_second_removal();
                        cases += 1;
                    }
                }
            }
        }
    }
    assert_eq!(cases, 48);
}
