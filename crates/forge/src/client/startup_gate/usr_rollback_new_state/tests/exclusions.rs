//! Zero-effect exclusions for phases outside the exact NewState suffix.

use crate::transition_journal::{Phase, RollbackActionOutcome};

use super::{
    super::{candidate_test_support::CandidateSource, test_fixture::OperationKind},
    support::{
        CandidateOutcome, Epoch, FreshOutcome, assert_pending_phase, build_fresh_invalidation, build_non_new_state,
        effect_counts, enter_candidate, enter_invalidation, persist_fresh_invalidated, persist_rollback_complete,
        reset_namespace_effect_counts,
    },
};

#[test]
fn startup_new_state_suffix_leaves_every_non_new_state_operation_zero_effect() {
    for kind in [OperationKind::Archived, OperationKind::ActiveReblit] {
        let fixture = build_non_new_state(kind);
        let journal_before = fixture.fixture.canonical_bytes();
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        reset_namespace_effect_counts();
        let removal_before = effect_counts().fresh_removal;

        let error = enter_candidate(&fixture);

        assert_pending_phase(&error, Phase::CandidatePreserveIntent);
        assert_eq!(fixture.fixture.canonical_bytes(), journal_before, "{kind:?}");
        assert_eq!(fixture.fixture.database_snapshot(), database_before, "{kind:?}");
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before, "{kind:?}");
        assert_eq!(effect_counts().create, 0, "{kind:?}");
        assert_eq!(effect_counts().normalize, 0, "{kind:?}");
        assert_eq!(effect_counts().candidate_move, 0, "{kind:?}");
        assert_eq!(effect_counts().fresh_removal, removal_before, "{kind:?}");
    }
}

#[test]
fn startup_new_state_suffix_retains_rollback_complete_with_zero_suffix_effects() {
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for candidate_outcome in CandidateOutcome::ALL {
                for fresh_outcome in FreshOutcome::ALL {
                    let fixture = build_fresh_invalidation(
                        epoch,
                        source,
                        RollbackActionOutcome::AlreadySatisfied,
                        candidate_outcome,
                        FreshOutcome::AlreadySatisfied,
                    );
                    let invalidated = persist_fresh_invalidated(&fixture, fresh_outcome);
                    let complete = persist_rollback_complete(&fixture, &invalidated);
                    let journal_before = fixture.canonical_bytes();
                    let database_before = fixture.fixture.fixture.database_snapshot();
                    let namespace_before = fixture.namespace_snapshot();
                    reset_namespace_effect_counts();
                    let removal_before = effect_counts().fresh_removal;

                    let error = enter_invalidation(&fixture);

                    assert_pending_phase(&error, Phase::RollbackComplete);
                    assert_eq!(fixture.canonical_record(), complete);
                    assert_eq!(fixture.canonical_bytes(), journal_before);
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
