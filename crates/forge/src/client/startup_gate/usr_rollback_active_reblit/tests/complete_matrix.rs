//! Exact ActiveReblit CandidatePreserved completion matrix through startup.

use crate::transition_journal::{Phase, RollbackActionOutcome};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, active_wrapper_path, assert_no_candidate_effects, assert_pending_phase, build_active,
        enter_candidate, expected_rollback_complete, persist_candidate_preserved, reset_candidate_effect_observers,
    },
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

#[test]
fn startup_active_reblit_complete_route_covers_all_sixteen_exact_candidate_preserved_cases() {
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in USR_OUTCOMES {
                for candidate_outcome in CandidateOrigin::ALL {
                    // Completion always starts from the already-preserved
                    // namespace shape; the recorded outcome remains an
                    // independent part of the 16-case journal matrix.
                    let fixture = build_active(epoch, source, usr_outcome, CandidateOrigin::AlreadySatisfied);
                    let preserved = persist_candidate_preserved(&fixture, candidate_outcome);
                    let expected = expected_rollback_complete(&preserved);
                    let database_before = fixture.fixture.database_snapshot();
                    let namespace_before = fixture.fixture.namespace_snapshot();
                    reset_candidate_effect_observers();

                    let error = enter_candidate(&fixture);

                    assert_pending_phase(&error, Phase::RollbackComplete);
                    assert_eq!(fixture.fixture.canonical_record(), expected);
                    assert_eq!(fixture.fixture.database_snapshot(), database_before);
                    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
                    assert!(active_wrapper_path(&fixture).join("usr").is_dir());
                    assert_no_candidate_effects();
                }
            }
        }
    }
}
