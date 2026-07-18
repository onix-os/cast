//! Exact terminal ActiveReblit matrix through the production startup gate.

use crate::transition_journal::RollbackActionOutcome;

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, WRAPPER_INDEX, WRAPPER_INDICES, active_wrapper_path_at, assert_canonical_absent,
        assert_no_candidate_effects, build_active_at_wrapper_index, enter_clean_candidate, persist_rollback_complete,
        reset_candidate_effect_observers,
    },
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

#[test]
fn startup_active_reblit_finalization_covers_all_sixteen_exact_terminal_cases_and_both_wrapper_indices() {
    assert_eq!(Epoch::ALL.len(), 2);
    assert_eq!(CandidateSource::ALL.len(), 2);
    assert_eq!(USR_OUTCOMES.len(), 2);
    assert_eq!(CandidateOrigin::ALL.len(), 2);
    assert_eq!(WRAPPER_INDICES, [0, WRAPPER_INDEX]);
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in USR_OUTCOMES {
                for candidate_outcome in CandidateOrigin::ALL {
                    let wrapper_index = match epoch {
                        Epoch::Current => 0,
                        Epoch::Historical => WRAPPER_INDEX,
                    };
                    let fixture = build_active_at_wrapper_index(
                        epoch,
                        source,
                        usr_outcome,
                        CandidateOrigin::AlreadySatisfied,
                        wrapper_index,
                    );
                    let terminal = persist_rollback_complete(&fixture, candidate_outcome);
                    let database_before = fixture.fixture.database_snapshot();
                    let namespace_before = fixture.fixture.namespace_snapshot();
                    reset_candidate_effect_observers();

                    let clean = enter_clean_candidate(&fixture);

                    assert_canonical_absent(&fixture.fixture.installation.root);
                    assert_eq!(terminal.operation, crate::transition_journal::Operation::ActiveReblit);
                    assert_eq!(fixture.fixture.database_snapshot(), database_before);
                    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
                    assert!(active_wrapper_path_at(&fixture, wrapper_index).join("usr").is_dir());
                    assert_no_candidate_effects();
                    drop(clean);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 16);
}
