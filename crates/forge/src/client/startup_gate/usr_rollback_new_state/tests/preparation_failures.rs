use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::startup_reconciliation::{
        NewStateCandidatePreserveMoveFault, NewStateCandidatePreservePostMoveDurabilityFaultPoint,
        NewStateCandidatePreserveTargetDurabilityFaultPoint, NewStateTargetCreateFault,
        NewStateTargetNormalizeDurabilityFaultPoint, NewStateTargetNormalizeFault,
        arm_before_new_state_candidate_preserve_post_move_candidate_sync,
        arm_before_new_state_target_create_reconciliation_capture,
        arm_before_new_state_target_normalize_reconciliation_capture,
        arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture,
        arm_new_state_candidate_preserve_move_fault, arm_new_state_candidate_preserve_post_move_durability_fault,
        arm_new_state_candidate_preserve_target_durability_fault, arm_new_state_target_create_fault,
        arm_new_state_target_normalize_durability_fault, arm_new_state_target_normalize_fault,
    },
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        Epoch, TargetPrefix, assert_candidate_ambiguous, assert_candidate_not_applied, assert_pending_phase,
        assert_suffix_dispatch_error, build_candidate, effect_counts, enter_candidate, reset_namespace_effect_counts,
        target_path,
    },
};

#[test]
fn startup_new_state_suffix_target_creation_not_applied_retries_only_creation_on_a_fresh_entry() {
    let fixture = build_candidate(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        TargetPrefix::Absent,
    );
    let source = fixture.candidate_intent.clone();
    reset_namespace_effect_counts();
    arm_new_state_target_create_fault(NewStateTargetCreateFault::ErrorWithoutApply);

    let first = enter_candidate(&fixture);

    assert_candidate_not_applied(first);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert!(!target_path(&fixture).exists());
    assert_eq!(effect_counts().create, 1);
    assert_eq!(effect_counts().candidate_move, 0);

    let second = enter_candidate(&fixture);
    assert_pending_phase(&second, Phase::CandidatePreserveIntent);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert!(target_path(&fixture).is_dir());
    assert_eq!(effect_counts().create, 2);
    assert_eq!(effect_counts().candidate_move, 0);
}

#[test]
fn startup_new_state_suffix_target_creation_post_evidence_change_defers_to_normalization_without_move() {
    let fixture = build_candidate(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        TargetPrefix::Absent,
    );
    let source = fixture.candidate_intent.clone();
    let target = target_path(&fixture);
    reset_namespace_effect_counts();
    arm_before_new_state_target_create_reconciliation_capture({
        let target = target.clone();
        move || fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap()
    });

    let first = enter_candidate(&fixture);

    assert_pending_phase(&first, Phase::CandidatePreserveIntent);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o7777, 0o500);
    assert_eq!(effect_counts().create, 1);
    assert_eq!(effect_counts().normalize, 0);
    assert_eq!(effect_counts().candidate_move, 0);

    let second = enter_candidate(&fixture);
    assert_pending_phase(&second, Phase::CandidatePreserveIntent);
    assert_eq!(fs::metadata(target).unwrap().permissions().mode() & 0o7777, 0o700);
    assert_eq!(effect_counts().create, 1);
    assert_eq!(effect_counts().normalize, 1);
    assert_eq!(effect_counts().candidate_move, 0);
}

#[test]
fn startup_new_state_suffix_target_creation_post_evidence_failure_consumes_creation_before_fresh_recovery() {
    let fixture = build_candidate(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        TargetPrefix::Absent,
    );
    let source = fixture.candidate_intent.clone();
    let payload = target_path(&fixture).join("unexpected-payload");
    reset_namespace_effect_counts();
    arm_before_new_state_target_create_reconciliation_capture({
        let payload = payload.clone();
        move || fs::write(payload, b"ambiguous").unwrap()
    });

    let first = enter_candidate(&fixture);

    assert_candidate_ambiguous(first);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert!(payload.is_file());
    assert_eq!(effect_counts().create, 1);
    assert_eq!(effect_counts().candidate_move, 0);

    fs::remove_file(payload).unwrap();
    let second = enter_candidate(&fixture);
    assert_pending_phase(&second, Phase::CandidatePreserved);
    assert_eq!(effect_counts().create, 1);
    assert_eq!(effect_counts().candidate_move, 1);
}

#[test]
fn startup_new_state_suffix_target_normalization_not_applied_retries_only_normalization() {
    let fixture = build_candidate(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        TargetPrefix::Residue,
    );
    let source = fixture.candidate_intent.clone();
    let target = target_path(&fixture);
    reset_namespace_effect_counts();
    arm_new_state_target_normalize_fault(NewStateTargetNormalizeFault::ErrorWithoutApply);

    let first = enter_candidate(&fixture);

    assert_candidate_not_applied(first);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o7777, 0o500);
    assert_eq!(effect_counts().normalize, 1);
    assert_eq!(effect_counts().candidate_move, 0);

    let second = enter_candidate(&fixture);
    assert_pending_phase(&second, Phase::CandidatePreserveIntent);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fs::metadata(target).unwrap().permissions().mode() & 0o7777, 0o700);
    assert_eq!(effect_counts().normalize, 2);
    assert_eq!(effect_counts().candidate_move, 0);
}

#[test]
fn startup_new_state_suffix_normalization_durability_failure_restarts_from_canonical_target_without_second_chmod() {
    let fixture = build_candidate(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        TargetPrefix::Residue,
    );
    let source = fixture.candidate_intent.clone();
    reset_namespace_effect_counts();
    arm_new_state_target_normalize_durability_fault(NewStateTargetNormalizeDurabilityFaultPoint::TargetSync);

    let first = enter_candidate(&fixture);

    assert_candidate_ambiguous(first);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(
        fs::metadata(target_path(&fixture)).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    assert_eq!(effect_counts().normalize, 1);
    assert_eq!(effect_counts().candidate_move, 0);

    let second = enter_candidate(&fixture);
    assert_pending_phase(&second, Phase::CandidatePreserved);
    assert_eq!(effect_counts().normalize, 1);
    assert_eq!(effect_counts().candidate_move, 1);
}

#[test]
fn startup_new_state_suffix_normalization_post_evidence_failure_requires_a_fresh_repaired_entry() {
    let fixture = build_candidate(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        TargetPrefix::Residue,
    );
    let source = fixture.candidate_intent.clone();
    let target = target_path(&fixture);
    reset_namespace_effect_counts();
    arm_before_new_state_target_normalize_reconciliation_capture({
        let target = target.clone();
        move || fs::set_permissions(target, fs::Permissions::from_mode(0o755)).unwrap()
    });

    let first = enter_candidate(&fixture);

    assert_candidate_ambiguous(first);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(effect_counts().normalize, 1);
    assert_eq!(effect_counts().candidate_move, 0);

    fs::set_permissions(&target, fs::Permissions::from_mode(0o500)).unwrap();
    let second = enter_candidate(&fixture);
    assert_pending_phase(&second, Phase::CandidatePreserveIntent);
    assert_eq!(effect_counts().normalize, 2);
    assert_eq!(effect_counts().candidate_move, 0);

    let third = enter_candidate(&fixture);
    assert_pending_phase(&third, Phase::CandidatePreserved);
    assert_eq!(effect_counts().normalize, 2);
    assert_eq!(effect_counts().candidate_move, 1);
}

#[test]
fn startup_new_state_suffix_pre_move_durability_failure_makes_zero_move_and_retries_with_fresh_evidence() {
    let fixture = build_candidate(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        TargetPrefix::Canonical,
    );
    let source = fixture.candidate_intent.clone();
    reset_namespace_effect_counts();
    arm_new_state_candidate_preserve_target_durability_fault(
        NewStateCandidatePreserveTargetDurabilityFaultPoint::TargetSync,
    );

    let first = enter_candidate(&fixture);

    assert_suffix_dispatch_error(&first);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(effect_counts().candidate_move, 0);

    let second = enter_candidate(&fixture);
    assert_pending_phase(&second, Phase::CandidatePreserved);
    assert_eq!(effect_counts().candidate_move, 1);
}

#[test]
fn startup_new_state_suffix_pre_move_evidence_failure_makes_zero_move_until_repaired() {
    let fixture = build_candidate(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        TargetPrefix::Canonical,
    );
    let source = fixture.candidate_intent.clone();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    let provenance = database.metadata_provenance(candidate).unwrap().unwrap();
    reset_namespace_effect_counts();
    arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture({
        let database = database.clone();
        move || database.delete_metadata_provenance_for_test(candidate).unwrap()
    });

    let first = enter_candidate(&fixture);

    assert_suffix_dispatch_error(&first);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(effect_counts().candidate_move, 0);

    database
        .insert_fresh_metadata_provenance_if_transition_matches(
            candidate,
            &fixture.candidate_intent.transition_id,
            &provenance,
        )
        .unwrap();
    let second = enter_candidate(&fixture);
    assert_pending_phase(&second, Phase::CandidatePreserved);
    assert_eq!(effect_counts().candidate_move, 1);
}

#[test]
fn startup_new_state_suffix_post_move_durability_failure_finishes_without_second_move() {
    let fixture = build_candidate(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        TargetPrefix::Canonical,
    );
    let source = fixture.candidate_intent.clone();
    reset_namespace_effect_counts();
    arm_new_state_candidate_preserve_move_fault(NewStateCandidatePreserveMoveFault::ErrorAfterApply);
    arm_new_state_candidate_preserve_post_move_durability_fault(
        NewStateCandidatePreservePostMoveDurabilityFaultPoint::CandidateSync,
    );

    let first = enter_candidate(&fixture);

    assert_suffix_dispatch_error(&first);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(effect_counts().candidate_move, 1);

    let second = enter_candidate(&fixture);
    assert_pending_phase(&second, Phase::CandidatePreserved);
    assert_eq!(effect_counts().candidate_move, 1);
}

#[test]
fn startup_new_state_suffix_post_move_evidence_failure_reopens_preserved_layout_without_second_move() {
    let fixture = build_candidate(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        TargetPrefix::Canonical,
    );
    let source = fixture.candidate_intent.clone();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    let provenance = database.metadata_provenance(candidate).unwrap().unwrap();
    reset_namespace_effect_counts();
    arm_before_new_state_candidate_preserve_post_move_candidate_sync({
        let database = database.clone();
        move || database.delete_metadata_provenance_for_test(candidate).unwrap()
    });

    let first = enter_candidate(&fixture);

    assert_suffix_dispatch_error(&first);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(effect_counts().candidate_move, 1);

    database
        .insert_fresh_metadata_provenance_if_transition_matches(
            candidate,
            &fixture.candidate_intent.transition_id,
            &provenance,
        )
        .unwrap();
    let second = enter_candidate(&fixture);
    assert_pending_phase(&second, Phase::CandidatePreserved);
    assert_eq!(effect_counts().candidate_move, 1);
}
