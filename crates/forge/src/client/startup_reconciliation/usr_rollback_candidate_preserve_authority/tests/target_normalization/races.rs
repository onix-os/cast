//! Binding, final-PRE, and post-attempt race contracts for normalization.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::{
        UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation,
        arm_before_new_state_target_normalize_reconciliation_capture,
        arm_before_usr_rollback_new_state_target_normalize_final_pre_capture,
    },
    startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
};

use super::support::{
    assert_effect_attempts, normal_fixture, normalize_target_lease, reset_effect_attempts, target_identity, target_path,
};

#[test]
fn startup_new_state_target_normalization_consumption_starts_with_the_open_journal_binding() {
    let fixture = normal_fixture(0o500);
    let target = target_path(&fixture);
    let before = target_identity(&target);
    let first = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = normalize_target_lease(&fixture, &first, &reservation);
    drop(first);
    let second = fixture.open_journal();
    reset_effect_attempts();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(lease.reconcile(&seal, &second).is_err());
    assert_effect_attempts(&fixture, 0);
    assert_eq!(target_identity(&target), before);
    fixture.assert_non_namespace_unchanged();
}

#[derive(Clone, Copy, Debug)]
enum FinalPreRace {
    Database,
    Journal,
    Namespace,
    TargetCanonicalized,
    QuarantineParentRebound,
}

#[test]
fn startup_new_state_target_normalization_final_pre_races_prevent_the_attempt() {
    for race in [
        FinalPreRace::Database,
        FinalPreRace::Journal,
        FinalPreRace::Namespace,
        FinalPreRace::TargetCanonicalized,
        FinalPreRace::QuarantineParentRebound,
    ] {
        let fixture = normal_fixture(0o500);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = normalize_target_lease(&fixture, &journal, &reservation);
        let target = target_path(&fixture);
        let hook: Box<dyn FnOnce()> = match race {
            FinalPreRace::Database => Box::new(fixture.candidate_transition_clear_hook()),
            FinalPreRace::Journal => Box::new(fixture.journal_change_hook()),
            FinalPreRace::Namespace => {
                Box::new(fixture.namespace_change_hook("candidate-target-normalize-final-pre-delta".to_owned()))
            }
            FinalPreRace::TargetCanonicalized => {
                let target = target.clone();
                Box::new(move || fs::set_permissions(target, fs::Permissions::from_mode(0o700)).unwrap())
            }
            FinalPreRace::QuarantineParentRebound => {
                let quarantine = fixture.fixture.installation.state_quarantine_dir();
                let displaced = quarantine.with_file_name("quarantine-displaced-before-target-normalize");
                Box::new(move || {
                    fs::rename(&quarantine, displaced).unwrap();
                    fs::create_dir(&quarantine).unwrap();
                    fs::set_permissions(quarantine, fs::Permissions::from_mode(0o700)).unwrap();
                })
            }
        };
        reset_effect_attempts();
        arm_before_usr_rollback_new_state_target_normalize_final_pre_capture(hook);
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

        assert!(lease.reconcile(&seal, &journal).is_err(), "{race:?}");
        assert_effect_attempts(&fixture, 0);
        match race {
            FinalPreRace::Database | FinalPreRace::Journal | FinalPreRace::Namespace => {
                assert_eq!(target_identity(&target).mode, 0o500, "{race:?}");
            }
            FinalPreRace::TargetCanonicalized => assert_eq!(target_identity(&target).mode, 0o700),
            FinalPreRace::QuarantineParentRebound => assert!(!target.exists()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PostAttemptChange {
    UnsafeMode,
    TargetRemoved,
    CanonicalReplacement,
    TransientParentMetadata,
    UnrelatedNamespace,
    QuarantineParentRebound,
}

#[test]
fn startup_new_state_target_normalization_post_attempt_ambiguity_consumes_all_retry_capability() {
    for change in [
        PostAttemptChange::UnsafeMode,
        PostAttemptChange::TargetRemoved,
        PostAttemptChange::CanonicalReplacement,
        PostAttemptChange::TransientParentMetadata,
        PostAttemptChange::UnrelatedNamespace,
        PostAttemptChange::QuarantineParentRebound,
    ] {
        let fixture = normal_fixture(0o500);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = normalize_target_lease(&fixture, &journal, &reservation);
        let target = target_path(&fixture);
        let hook: Box<dyn FnOnce()> = match change {
            PostAttemptChange::UnsafeMode => {
                let target = target.clone();
                Box::new(move || fs::set_permissions(target, fs::Permissions::from_mode(0o755)).unwrap())
            }
            PostAttemptChange::TargetRemoved => {
                let target = target.clone();
                Box::new(move || fs::remove_dir(target).unwrap())
            }
            PostAttemptChange::CanonicalReplacement => {
                let target = target.clone();
                let displaced = target.with_file_name("candidate-target-normalize-post-attempt-retained");
                Box::new(move || {
                    fs::rename(&target, displaced).unwrap();
                    fs::create_dir(&target).unwrap();
                    fs::set_permissions(target, fs::Permissions::from_mode(0o700)).unwrap();
                })
            }
            PostAttemptChange::TransientParentMetadata => {
                let transient = fixture
                    .fixture
                    .installation
                    .state_quarantine_dir()
                    .join("candidate-target-normalize-transient-parent-delta");
                Box::new(move || {
                    fs::create_dir(&transient).unwrap();
                    fs::remove_dir(transient).unwrap();
                })
            }
            PostAttemptChange::UnrelatedNamespace => {
                Box::new(fixture.namespace_change_hook("candidate-target-normalize-post-attempt-delta".to_owned()))
            }
            PostAttemptChange::QuarantineParentRebound => {
                let quarantine = fixture.fixture.installation.state_quarantine_dir();
                let displaced = quarantine.with_file_name("quarantine-displaced-after-target-normalize");
                Box::new(move || {
                    fs::rename(&quarantine, displaced).unwrap();
                    fs::create_dir(&quarantine).unwrap();
                    fs::set_permissions(quarantine, fs::Permissions::from_mode(0o700)).unwrap();
                })
            }
        };
        reset_effect_attempts();
        arm_before_new_state_target_normalize_reconciliation_capture(hook);
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

        assert!(matches!(
            lease.reconcile(&seal, &journal).unwrap(),
            UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::Ambiguous
        ));
        assert_effect_attempts(&fixture, 1);
        fixture.assert_non_namespace_unchanged();
    }
}

#[derive(Clone, Copy, Debug)]
enum TrailingEvidenceRace {
    Database,
    Journal,
}

#[test]
fn startup_new_state_target_normalization_rechecks_database_and_journal_after_the_attempt() {
    for race in [TrailingEvidenceRace::Database, TrailingEvidenceRace::Journal] {
        let fixture = normal_fixture(0o500);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = normalize_target_lease(&fixture, &journal, &reservation);
        let target = target_path(&fixture);
        let hook: Box<dyn FnOnce()> = match race {
            TrailingEvidenceRace::Database => Box::new(fixture.candidate_transition_clear_hook()),
            TrailingEvidenceRace::Journal => Box::new(fixture.journal_change_hook()),
        };
        reset_effect_attempts();
        arm_before_new_state_target_normalize_reconciliation_capture(hook);
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

        assert!(lease.reconcile(&seal, &journal).is_err(), "{race:?}");
        assert_effect_attempts(&fixture, 1);
        assert_eq!(target_identity(&target).mode, 0o700, "{race:?}");
    }
}
