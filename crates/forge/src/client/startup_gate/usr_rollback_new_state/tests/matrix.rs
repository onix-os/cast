//! Current and historical source/outcome matrices through the real startup gate.

use crate::transition_journal::{Phase, RollbackActionOutcome};

use super::super::candidate_test_support::CandidateSource;
use super::support::{
    CandidateOutcome, Epoch, FreshOutcome, TargetPrefix, assert_pending_phase, build_candidate,
    build_fresh_invalidation, effect_counts, enter_candidate, enter_invalidation, persist_candidate_preserved,
    persist_fresh_invalidated, reset_namespace_effect_counts,
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

#[test]
fn startup_new_state_suffix_routes_every_exact_candidate_preserved_matrix_without_later_effects() {
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in USR_OUTCOMES {
                for candidate_outcome in CandidateOutcome::ALL {
                    let fixture = build_candidate(epoch, source, usr_outcome, TargetPrefix::Preserved);
                    let candidate_preserved = persist_candidate_preserved(&fixture, candidate_outcome);
                    let expected = candidate_preserved.rollback_successor(None).unwrap();
                    let database_before = fixture.fixture.database_snapshot();
                    let namespace_before = fixture.fixture.namespace_snapshot();
                    reset_namespace_effect_counts();
                    let removal_before = effect_counts().fresh_removal;

                    let error = enter_candidate(&fixture);

                    assert_pending_phase(&error, Phase::FreshDbInvalidationIntent);
                    assert_eq!(fixture.fixture.canonical_record(), expected);
                    assert_eq!(fixture.fixture.database_snapshot(), database_before);
                    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
                    assert_eq!(effect_counts().create, 0);
                    assert_eq!(effect_counts().normalize, 0);
                    assert_eq!(effect_counts().candidate_move, 0);
                    assert_eq!(effect_counts().fresh_removal, removal_before);
                }
            }
        }
    }
}

#[test]
fn startup_new_state_suffix_invalidates_present_or_accepts_joint_absence_for_every_exact_matrix() {
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in USR_OUTCOMES {
                for candidate_outcome in CandidateOutcome::ALL {
                    for fresh_outcome in FreshOutcome::ALL {
                        let fixture =
                            build_fresh_invalidation(epoch, source, usr_outcome, candidate_outcome, fresh_outcome);
                        let expected = fixture
                            .record
                            .rollback_successor(Some(fresh_outcome.journal()))
                            .unwrap();
                        let namespace_before = fixture.namespace_snapshot();
                        reset_namespace_effect_counts();

                        let error = enter_invalidation(&fixture);

                        assert_pending_phase(&error, Phase::FreshDbInvalidated);
                        assert_eq!(fixture.canonical_record(), expected);
                        fixture.assert_exact_joint_absence();
                        assert_eq!(fixture.namespace_snapshot(), namespace_before);
                        assert_eq!(effect_counts().create, 0);
                        assert_eq!(effect_counts().normalize, 0);
                        assert_eq!(effect_counts().candidate_move, 0);
                        assert_eq!(
                            effect_counts().fresh_removal,
                            usize::from(fresh_outcome == FreshOutcome::Applied)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn startup_new_state_suffix_completes_every_exact_invalidated_outcome_without_repeating_effects() {
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in USR_OUTCOMES {
                for candidate_outcome in CandidateOutcome::ALL {
                    for fresh_outcome in FreshOutcome::ALL {
                        let fixture = build_fresh_invalidation(
                            epoch,
                            source,
                            usr_outcome,
                            candidate_outcome,
                            FreshOutcome::AlreadySatisfied,
                        );
                        let invalidated = persist_fresh_invalidated(&fixture, fresh_outcome);
                        let expected = invalidated.rollback_successor(None).unwrap();
                        let database_before = fixture.fixture.fixture.database_snapshot();
                        let namespace_before = fixture.namespace_snapshot();
                        reset_namespace_effect_counts();
                        let removal_before = effect_counts().fresh_removal;

                        let error = enter_invalidation(&fixture);

                        assert_pending_phase(&error, Phase::RollbackComplete);
                        assert_eq!(fixture.canonical_record(), expected);
                        assert_eq!(fixture.fixture.fixture.database_snapshot(), database_before);
                        assert_eq!(fixture.namespace_snapshot(), namespace_before);
                        assert_eq!(effect_counts().create, 0);
                        assert_eq!(effect_counts().normalize, 0);
                        assert_eq!(effect_counts().candidate_move, 0);
                        assert_eq!(effect_counts().fresh_removal, removal_before);
                    }
                }
            }
        }
    }
}
