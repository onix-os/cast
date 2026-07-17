//! Restart contracts for effect, evidence, and storage failures.

use crate::{
    client::startup_reconciliation::{
        NewStateCandidatePreserveMoveFault, arm_between_usr_rollback_fresh_db_invalidation_route_database_captures,
        arm_new_state_candidate_preserve_move_fault,
    },
    db::state::{
        ExactFreshTransitionRemovalFault, arm_exact_fresh_transition_removal_fault,
        assert_exact_fresh_transition_removal_fault_consumed,
    },
    transition_journal::{
        Phase, RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::super::candidate_test_support::CandidateSource;
use super::support::{
    CandidateOutcome, Epoch, FreshOutcome, TargetPrefix, assert_candidate_not_applied, assert_fresh_ambiguous,
    assert_fresh_not_applied, assert_pending_phase, assert_suffix_dispatch_error, build_candidate,
    build_fresh_invalidation, effect_counts, enter_candidate, enter_invalidation, persist_candidate_preserved,
    reset_namespace_effect_counts,
};

#[test]
fn startup_new_state_suffix_candidate_effect_failure_retries_once_on_a_fresh_entry() {
    let fixture = build_candidate(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        TargetPrefix::Canonical,
    );
    let source = fixture.candidate_intent.clone();
    reset_namespace_effect_counts();
    arm_new_state_candidate_preserve_move_fault(NewStateCandidatePreserveMoveFault::ErrorWithoutApply);

    let first = enter_candidate(&fixture);

    assert_candidate_not_applied(first);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(effect_counts().candidate_move, 1);

    let second = enter_candidate(&fixture);
    let expected = source.rollback_successor(Some(RollbackActionOutcome::Applied)).unwrap();
    assert_pending_phase(&second, Phase::CandidatePreserved);
    assert_eq!(fixture.fixture.canonical_record(), expected);
    assert_eq!(effect_counts().candidate_move, 2);
}

#[test]
fn startup_new_state_suffix_database_effect_failures_restart_from_exact_present_or_joint_absence() {
    let present = build_fresh_invalidation(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::Applied,
        FreshOutcome::Applied,
    );
    let source = present.record.clone();
    arm_exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::BeforeTransaction);

    let first = enter_invalidation(&present);

    assert_exact_fresh_transition_removal_fault_consumed();
    assert_fresh_not_applied(first);
    assert_eq!(present.canonical_record(), source);
    present.assert_exact_present();
    assert_eq!(effect_counts().fresh_removal, 1);

    let second = enter_invalidation(&present);
    let applied = source.rollback_successor(Some(RollbackActionOutcome::Applied)).unwrap();
    assert_pending_phase(&second, Phase::FreshDbInvalidated);
    assert_eq!(present.canonical_record(), applied);
    present.assert_exact_joint_absence();
    assert_eq!(effect_counts().fresh_removal, 1);

    let ambiguous = build_fresh_invalidation(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
        FreshOutcome::Applied,
    );
    let source = ambiguous.record.clone();
    arm_exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::AfterCommitWithUncertainReport);

    let first = enter_invalidation(&ambiguous);

    assert_exact_fresh_transition_removal_fault_consumed();
    assert_fresh_ambiguous(first);
    assert_eq!(ambiguous.canonical_record(), source);
    ambiguous.assert_exact_joint_absence();
    assert_eq!(effect_counts().fresh_removal, 1);

    let second = enter_invalidation(&ambiguous);
    let already_satisfied = source
        .rollback_successor(Some(RollbackActionOutcome::AlreadySatisfied))
        .unwrap();
    assert_pending_phase(&second, Phase::FreshDbInvalidated);
    assert_eq!(ambiguous.canonical_record(), already_satisfied);
    assert_eq!(effect_counts().fresh_removal, 0);
}

#[test]
fn startup_new_state_suffix_source_durable_storage_failures_repeat_no_candidate_or_database_effect() {
    let candidate = build_candidate(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        TargetPrefix::Canonical,
    );
    let source = candidate.candidate_intent.clone();
    reset_namespace_effect_counts();
    arm_next_temporary_sync_fault();

    let first = enter_candidate(&candidate);

    assert_temporary_sync_fault_consumed();
    assert_suffix_dispatch_error(&first);
    assert_eq!(candidate.fixture.canonical_record(), source);
    assert_eq!(effect_counts().candidate_move, 1);

    let second = enter_candidate(&candidate);
    let preserved = source
        .rollback_successor(Some(RollbackActionOutcome::AlreadySatisfied))
        .unwrap();
    assert_pending_phase(&second, Phase::CandidatePreserved);
    assert_eq!(candidate.fixture.canonical_record(), preserved);
    assert_eq!(effect_counts().candidate_move, 1);

    let fresh = build_fresh_invalidation(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::Applied,
        FreshOutcome::Applied,
    );
    let source = fresh.record.clone();
    arm_next_temporary_sync_fault();

    let first = enter_invalidation(&fresh);

    assert_temporary_sync_fault_consumed();
    assert_suffix_dispatch_error(&first);
    assert_eq!(fresh.canonical_record(), source);
    fresh.assert_exact_joint_absence();
    assert_eq!(effect_counts().fresh_removal, 1);

    let second = enter_invalidation(&fresh);
    let invalidated = source
        .rollback_successor(Some(RollbackActionOutcome::AlreadySatisfied))
        .unwrap();
    assert_pending_phase(&second, Phase::FreshDbInvalidated);
    assert_eq!(fresh.canonical_record(), invalidated);
    assert_eq!(effect_counts().fresh_removal, 0);
}

#[test]
fn startup_new_state_suffix_successor_durable_storage_failure_never_redispatches_the_completed_phase() {
    let fixture = build_fresh_invalidation(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
        FreshOutcome::Applied,
    );
    let source = fixture.record.clone();
    let invalidated = source.rollback_successor(Some(RollbackActionOutcome::Applied)).unwrap();
    arm_next_update_first_directory_sync_fault();

    let first = enter_invalidation(&fixture);

    assert_update_first_directory_sync_fault_consumed();
    assert_suffix_dispatch_error(&first);
    assert_eq!(fixture.canonical_record(), invalidated);
    assert_eq!(effect_counts().fresh_removal, 1);

    let second = enter_invalidation(&fixture);
    let complete = invalidated.rollback_successor(None).unwrap();
    assert_pending_phase(&second, Phase::RollbackComplete);
    assert_eq!(fixture.canonical_record(), complete);
    assert_eq!(effect_counts().fresh_removal, 1);
}

#[test]
fn startup_new_state_suffix_reloads_an_overwritten_durable_successor_before_dispatch() {
    let fixture = build_fresh_invalidation(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::AlreadySatisfied,
        FreshOutcome::AlreadySatisfied,
    );
    let invalidated = fixture
        .record
        .rollback_successor(Some(RollbackActionOutcome::AlreadySatisfied))
        .unwrap();
    fixture.overwrite_canonical(&invalidated);
    let removal_before = effect_counts().fresh_removal;

    let result = enter_invalidation(&fixture);

    let complete = invalidated.rollback_successor(None).unwrap();
    assert_pending_phase(&result, Phase::RollbackComplete);
    assert_eq!(fixture.canonical_record(), complete);
    assert_eq!(effect_counts().fresh_removal, removal_before);
}

#[test]
fn startup_new_state_suffix_evidence_change_defers_before_route_or_later_effects() {
    let fixture = build_candidate(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        TargetPrefix::Preserved,
    );
    let source = persist_candidate_preserved(&fixture, CandidateOutcome::Applied);
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    let transition = source.transition_id.clone();
    let namespace_before = fixture.fixture.namespace_snapshot();
    reset_namespace_effect_counts();
    let removal_before = effect_counts().fresh_removal;
    arm_between_usr_rollback_fresh_db_invalidation_route_database_captures(move || {
        database.clear_transition_if_matches(candidate, &transition).unwrap();
    });

    let error = enter_candidate(&fixture);

    assert_pending_phase(&error, Phase::CandidatePreserved);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_eq!(effect_counts().create, 0);
    assert_eq!(effect_counts().normalize, 0);
    assert_eq!(effect_counts().candidate_move, 0);
    assert_eq!(effect_counts().fresh_removal, removal_before);
}
