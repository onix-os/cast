//! Ordered empty-target durability before the one-shot preservation move.

use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            NewStateCandidatePreserveTargetDurabilityEvent, NewStateCandidatePreserveTargetDurabilityFaultPoint,
            UsrRollbackNewStateCandidatePreserveApplyReconciliation,
            arm_before_new_state_candidate_preserve_quarantine_parent_sync,
            arm_before_new_state_candidate_preserve_target_durability_final_pre_capture,
            arm_before_new_state_candidate_preserve_target_durability_pre_move_revalidation,
            arm_before_new_state_candidate_preserve_target_sync,
            arm_new_state_candidate_preserve_target_durability_fault, new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_target_durability_events,
            take_new_state_candidate_preserve_target_durability_events,
        },
        startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
    },
    transition_journal::RollbackActionOutcome,
};

use super::{CandidatePreserveFixture, CandidateSource, move_lease, transition_quarantine_path};

pub(super) fn expected_events(
    fixture: &CandidatePreserveFixture,
) -> Vec<NewStateCandidatePreserveTargetDurabilityEvent> {
    let target = identity(&transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent));
    let quarantine_parent = identity(&fixture.fixture.installation.state_quarantine_dir());
    vec![
        NewStateCandidatePreserveTargetDurabilityEvent::TargetSynced {
            device: target.0,
            inode: target.1,
        },
        NewStateCandidatePreserveTargetDurabilityEvent::QuarantineParentSynced {
            device: quarantine_parent.0,
            inode: quarantine_parent.1,
        },
        NewStateCandidatePreserveTargetDurabilityEvent::FinalPreProven,
    ]
}

fn reset_observations() {
    reset_new_state_candidate_preserve_move_attempt_count();
    reset_new_state_candidate_preserve_target_durability_events();
}

fn assert_move_not_attempted(fixture: &CandidatePreserveFixture) {
    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
    assert!(fixture.fixture.installation.staging_dir().join("usr").is_dir());
}

fn assert_applied(result: UsrRollbackNewStateCandidatePreserveApplyReconciliation<'_>) {
    let UsrRollbackNewStateCandidatePreserveApplyReconciliation::Applied(authority) = result else {
        panic!("exact durable target PRE did not apply the candidate-preservation move");
    };
    drop(authority);
}

#[test]
fn startup_new_state_candidate_preserve_target_durability_orders_barriers_for_every_origin_and_outcome() {
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(source, usr_outcome);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let lease = move_lease(&fixture, &journal, &reservation);
            let expected = expected_events(&fixture);
            reset_observations();
            let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

            assert_applied(lease.reconcile(&seal, &journal).unwrap());

            assert_eq!(take_new_state_candidate_preserve_target_durability_events(), expected);
            assert_eq!(new_state_candidate_preserve_move_attempt_count(), 1);
            assert!(!fixture.fixture.installation.staging_dir().join("usr").exists());
            fixture.assert_non_namespace_unchanged();
        }
    }
}

#[test]
fn startup_new_state_candidate_preserve_target_durability_faults_stop_at_exact_prefixes_before_move() {
    let cases = [
        (NewStateCandidatePreserveTargetDurabilityFaultPoint::TargetSync, 0),
        (
            NewStateCandidatePreserveTargetDurabilityFaultPoint::QuarantineParentSync,
            1,
        ),
        (NewStateCandidatePreserveTargetDurabilityFaultPoint::FinalPreCapture, 2),
    ];
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for (fault, expected_prefix_len) in cases {
                let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(source, usr_outcome);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let lease = move_lease(&fixture, &journal, &reservation);
                let expected = expected_events(&fixture);
                reset_observations();
                arm_new_state_candidate_preserve_target_durability_fault(fault);
                let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

                assert!(lease.reconcile(&seal, &journal).is_err());

                assert_eq!(
                    take_new_state_candidate_preserve_target_durability_events(),
                    expected[..expected_prefix_len]
                );
                assert_move_not_attempted(&fixture);
                fixture.assert_non_namespace_unchanged();
            }
        }
    }
}

#[derive(Clone, Copy)]
enum DurabilityRace {
    BeforeTargetSyncReplacement,
    BetweenSyncsTargetReplacement,
    BetweenSyncsParentRebind,
    FinalPreNamespaceChange,
}

#[test]
fn startup_new_state_candidate_preserve_target_durability_namespace_races_fail_at_exact_prefixes() {
    let cases = [
        (DurabilityRace::BeforeTargetSyncReplacement, 0),
        (DurabilityRace::BetweenSyncsTargetReplacement, 1),
        (DurabilityRace::BetweenSyncsParentRebind, 1),
        (DurabilityRace::FinalPreNamespaceChange, 2),
    ];
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for (race, expected_prefix_len) in cases {
                let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(source, usr_outcome);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let lease = move_lease(&fixture, &journal, &reservation);
                let expected = expected_events(&fixture);
                let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
                reset_observations();
                match race {
                    DurabilityRace::BeforeTargetSyncReplacement => {
                        let displaced = target.with_file_name("candidate-target-before-durability-retained");
                        arm_before_new_state_candidate_preserve_target_sync(move || {
                            replace_public_target(&target, &displaced);
                        });
                    }
                    DurabilityRace::BetweenSyncsTargetReplacement => {
                        let displaced = target.with_file_name("candidate-target-between-durability-retained");
                        arm_before_new_state_candidate_preserve_quarantine_parent_sync(move || {
                            replace_public_target(&target, &displaced);
                        });
                    }
                    DurabilityRace::BetweenSyncsParentRebind => {
                        let quarantine = fixture.fixture.installation.state_quarantine_dir();
                        let displaced = quarantine.with_file_name("quarantine-between-move-durability-retained");
                        arm_before_new_state_candidate_preserve_quarantine_parent_sync(move || {
                            fs::rename(&quarantine, displaced).unwrap();
                            fs::create_dir(&quarantine).unwrap();
                            fs::set_permissions(quarantine, fs::Permissions::from_mode(0o700)).unwrap();
                        });
                    }
                    DurabilityRace::FinalPreNamespaceChange => {
                        arm_before_new_state_candidate_preserve_target_durability_final_pre_capture(
                            fixture.namespace_change_hook("candidate-target-durability-final-pre-race".to_owned()),
                        );
                    }
                }
                let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

                assert!(lease.reconcile(&seal, &journal).is_err());

                assert_eq!(
                    take_new_state_candidate_preserve_target_durability_events(),
                    expected[..expected_prefix_len]
                );
                assert_move_not_attempted(&fixture);
                fixture.assert_non_namespace_unchanged();
            }
        }
    }
}

#[test]
fn startup_new_state_candidate_preserve_fresh_move_lease_repeats_target_durability_after_failure() {
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(source, usr_outcome);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let first = move_lease(&fixture, &journal, &reservation);
            let expected = expected_events(&fixture);
            let transient = fixture
                .fixture
                .installation
                .state_quarantine_dir()
                .join("candidate-target-durability-pre-move-transient");
            reset_observations();
            arm_before_new_state_candidate_preserve_target_durability_pre_move_revalidation(move || {
                fs::create_dir(&transient).unwrap();
                fs::set_permissions(&transient, fs::Permissions::from_mode(0o700)).unwrap();
                fs::remove_dir(transient).unwrap();
            });
            let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

            assert!(first.reconcile(&seal, &journal).is_err());
            assert_eq!(take_new_state_candidate_preserve_target_durability_events(), expected);
            assert_move_not_attempted(&fixture);

            let second = move_lease(&fixture, &journal, &reservation);
            let expected = expected_events(&fixture);
            reset_observations();
            assert_applied(second.reconcile(&seal, &journal).unwrap());

            assert_eq!(take_new_state_candidate_preserve_target_durability_events(), expected);
            assert_eq!(new_state_candidate_preserve_move_attempt_count(), 1);
            assert!(!fixture.fixture.installation.staging_dir().join("usr").exists());
            fixture.assert_non_namespace_unchanged();
        }
    }
}

fn replace_public_target(target: &Path, displaced: &Path) {
    fs::rename(target, displaced).unwrap();
    fs::create_dir(target).unwrap();
    fs::set_permissions(target, fs::Permissions::from_mode(0o700)).unwrap();
}

fn identity(path: &Path) -> (u64, u64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}
