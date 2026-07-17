use std::os::unix::fs::PermissionsExt as _;

use crate::transition_journal::{Phase, RollbackActionOutcome};

use super::super::candidate_test_support::CandidateSource;
use super::support::{
    CandidateOutcome, EffectCounts, Epoch, FreshOutcome, TargetPrefix, assert_pending_phase, build_candidate,
    canonical_record, effect_counts, enter_candidate, enter_fresh_handles, install_persistent_database,
    release_candidate_handles, reset_namespace_effect_counts, target_path,
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

#[test]
fn startup_new_state_suffix_consumes_exactly_one_checkpoint_per_entry_for_every_target_prefix() {
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in USR_OUTCOMES {
                for prefix in TargetPrefix::ALL {
                    let fixture = build_candidate(epoch, source, usr_outcome, prefix);
                    let source_record = fixture.candidate_intent.clone();
                    let source_bytes = fixture.fixture.canonical_bytes();
                    reset_namespace_effect_counts();
                    let removal_before = effect_counts().fresh_removal;

                    let error = enter_candidate(&fixture);
                    let counts = effect_counts();
                    assert_eq!(
                        counts.fresh_removal, removal_before,
                        "{epoch:?} {source:?} {usr_outcome:?} {prefix:?}"
                    );

                    match prefix {
                        TargetPrefix::Absent => {
                            assert_pending_phase(&error, Phase::CandidatePreserveIntent);
                            assert_eq!(fixture.fixture.canonical_bytes(), source_bytes);
                            assert_eq!(counts.create, 1);
                            assert_eq!(counts.normalize, 0);
                            assert_eq!(counts.candidate_move, 0);
                            assert_eq!(
                                std::fs::metadata(target_path(&fixture)).unwrap().permissions().mode() & 0o7777,
                                0o700
                            );
                        }
                        TargetPrefix::Residue => {
                            assert_pending_phase(&error, Phase::CandidatePreserveIntent);
                            assert_eq!(fixture.fixture.canonical_bytes(), source_bytes);
                            assert_eq!(counts.create, 0);
                            assert_eq!(counts.normalize, 1);
                            assert_eq!(counts.candidate_move, 0);
                            assert_eq!(
                                std::fs::metadata(target_path(&fixture)).unwrap().permissions().mode() & 0o7777,
                                0o700
                            );
                        }
                        TargetPrefix::Canonical | TargetPrefix::Preserved => {
                            assert_pending_phase(&error, Phase::CandidatePreserved);
                            let outcome = if prefix == TargetPrefix::Canonical {
                                CandidateOutcome::Applied
                            } else {
                                CandidateOutcome::AlreadySatisfied
                            };
                            let expected = source_record.rollback_successor(Some(outcome.journal())).unwrap();
                            assert_eq!(fixture.fixture.canonical_record(), expected);
                            assert_eq!(counts.create, 0);
                            assert_eq!(counts.normalize, 0);
                            assert_eq!(counts.candidate_move, usize::from(prefix == TargetPrefix::Canonical));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn startup_new_state_suffix_runs_the_exact_multi_entry_sequence_without_same_entry_fallthrough() {
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in USR_OUTCOMES {
                let fixture = build_candidate(epoch, source, usr_outcome, TargetPrefix::Absent);
                let candidate_intent = fixture.candidate_intent.clone();
                reset_namespace_effect_counts();
                let removal_before = effect_counts().fresh_removal;

                let first = enter_candidate(&fixture);
                assert_pending_phase(&first, Phase::CandidatePreserveIntent);
                assert_eq!(fixture.fixture.canonical_record(), candidate_intent);
                assert_eq!(
                    effect_counts(),
                    EffectCounts {
                        create: 1,
                        normalize: 0,
                        candidate_move: 0,
                        fresh_removal: removal_before,
                    }
                );

                let second = enter_candidate(&fixture);
                let candidate_preserved = candidate_intent
                    .rollback_successor(Some(RollbackActionOutcome::Applied))
                    .unwrap();
                assert_pending_phase(&second, Phase::CandidatePreserved);
                assert_eq!(fixture.fixture.canonical_record(), candidate_preserved);
                assert_eq!(effect_counts().candidate_move, 1);

                let third = enter_candidate(&fixture);
                let invalidation_intent = candidate_preserved.rollback_successor(None).unwrap();
                assert_pending_phase(&third, Phase::FreshDbInvalidationIntent);
                assert_eq!(fixture.fixture.canonical_record(), invalidation_intent);
                assert_eq!(effect_counts().candidate_move, 1);

                let fourth = enter_candidate(&fixture);
                let invalidated = invalidation_intent
                    .rollback_successor(Some(RollbackActionOutcome::Applied))
                    .unwrap();
                assert_pending_phase(&fourth, Phase::FreshDbInvalidated);
                assert_eq!(fixture.fixture.canonical_record(), invalidated);
                assert_eq!(effect_counts().candidate_move, 1);
                assert_eq!(effect_counts().fresh_removal, 1);

                let fifth = enter_candidate(&fixture);
                let complete = invalidated.rollback_successor(None).unwrap();
                assert_pending_phase(&fifth, Phase::RollbackComplete);
                assert_eq!(fixture.fixture.canonical_record(), complete);
                assert_eq!(effect_counts().candidate_move, 1);
                assert_eq!(effect_counts().fresh_removal, 1);

                let sixth = enter_candidate(&fixture);
                assert_pending_phase(&sixth, Phase::RollbackComplete);
                assert_eq!(fixture.fixture.canonical_record(), complete);
                assert_eq!(effect_counts().candidate_move, 1);
                assert_eq!(effect_counts().fresh_removal, 1);
            }
        }
    }
}

#[test]
fn startup_new_state_suffix_reacquires_fresh_installation_database_journal_and_reservation_handles() {
    let mut fixture = build_candidate(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        TargetPrefix::Absent,
    );
    install_persistent_database(&mut fixture);
    let candidate_intent = fixture.candidate_intent.clone();
    let retained_root = release_candidate_handles(fixture);
    let root = retained_root.path();
    reset_namespace_effect_counts();

    let first = enter_fresh_handles(root);
    assert_pending_phase(&first, Phase::CandidatePreserveIntent);
    assert_eq!(canonical_record(root), candidate_intent);

    let second = enter_fresh_handles(root);
    let candidate_preserved = candidate_intent
        .rollback_successor(Some(CandidateOutcome::Applied.journal()))
        .unwrap();
    assert_pending_phase(&second, Phase::CandidatePreserved);
    assert_eq!(canonical_record(root), candidate_preserved);

    let third = enter_fresh_handles(root);
    let invalidation_intent = candidate_preserved.rollback_successor(None).unwrap();
    assert_pending_phase(&third, Phase::FreshDbInvalidationIntent);
    assert_eq!(canonical_record(root), invalidation_intent);

    let fourth = enter_fresh_handles(root);
    let invalidated = invalidation_intent
        .rollback_successor(Some(FreshOutcome::Applied.journal()))
        .unwrap();
    assert_pending_phase(&fourth, Phase::FreshDbInvalidated);
    assert_eq!(canonical_record(root), invalidated);
    assert_eq!(effect_counts().fresh_removal, 1);

    let fifth = enter_fresh_handles(root);
    let complete = invalidated.rollback_successor(None).unwrap();
    assert_pending_phase(&fifth, Phase::RollbackComplete);
    assert_eq!(canonical_record(root), complete);
    assert_eq!(effect_counts().candidate_move, 1);
    assert_eq!(effect_counts().fresh_removal, 1);
}
