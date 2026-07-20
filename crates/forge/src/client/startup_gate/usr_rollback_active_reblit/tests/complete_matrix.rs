//! Exact ActiveReblit CandidatePreserved completion matrix through startup.

use crate::transition_journal::{Phase, RollbackActionOutcome};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, active_wrapper_path, assert_complete_route_journal_only,
        assert_exact_no_boot_completion_plan, assert_pending_phase, build_active, enter_candidate,
        expected_rollback_complete, persist_candidate_preserved, reset_complete_route_effect_observers,
    },
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

#[test]
fn startup_active_reblit_complete_route_covers_all_twenty_four_exact_candidate_preserved_cases() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in USR_OUTCOMES {
                for candidate_outcome in CandidateOrigin::ALL {
                    // Completion always starts from the already-preserved
                    // namespace shape; the recorded outcome remains an
                    // independent part of the 24-case journal matrix.
                    let fixture = build_active(epoch, source, usr_outcome, CandidateOrigin::AlreadySatisfied);
                    let preserved = persist_candidate_preserved(&fixture, candidate_outcome);
                    let expected = expected_rollback_complete(&preserved);
                    let case = (epoch, source, usr_outcome, candidate_outcome);
                    let database_before = fixture.fixture.database_snapshot();
                    let namespace_before = fixture.fixture.namespace_snapshot();
                    assert_exact_no_boot_completion_plan(&preserved, source);
                    reset_complete_route_effect_observers();

                    let error = enter_candidate(&fixture);

                    assert_pending_phase(&error, Phase::RollbackComplete);
                    assert_eq!(expected.generation, preserved.generation + 1, "{case:?}");
                    assert_eq!(expected.rollback, preserved.rollback, "{case:?}");
                    assert_exact_no_boot_completion_plan(&expected, source);
                    assert_eq!(fixture.fixture.canonical_record(), expected, "{case:?}");
                    assert_eq!(fixture.fixture.database_snapshot(), database_before, "{case:?}");
                    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before, "{case:?}");
                    assert!(active_wrapper_path(&fixture).join("usr").is_dir());
                    assert_complete_route_journal_only();
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 24);
}
