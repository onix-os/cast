//! Ordered target and quarantine-parent durability for normalized targets.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            NewStateTargetNormalizeDurabilityEvent, NewStateTargetNormalizeDurabilityFaultPoint,
            UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation,
            arm_before_new_state_target_normalize_final_canonical_capture,
            arm_before_new_state_target_normalize_quarantine_parent_sync,
            arm_before_new_state_target_normalize_target_sync, arm_new_state_target_normalize_durability_fault,
            reset_new_state_target_normalize_durability_events, take_new_state_target_normalize_durability_events,
        },
        startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
    },
    transition_journal::RollbackActionOutcome,
};

use super::super::support::CandidateSource;
use super::support::{
    assert_effect_attempts, normalize_target_lease, reset_effect_attempts, residue_fixture, target_identity,
    target_path,
};

fn expected_events(
    fixture: &super::super::support::CandidatePreserveFixture,
) -> Vec<NewStateTargetNormalizeDurabilityEvent> {
    let target = target_identity(&target_path(fixture));
    let quarantine_parent = target_identity(&fixture.fixture.installation.state_quarantine_dir());
    vec![
        NewStateTargetNormalizeDurabilityEvent::TargetSynced {
            device: target.device,
            inode: target.inode,
        },
        NewStateTargetNormalizeDurabilityEvent::QuarantineParentSynced {
            device: quarantine_parent.device,
            inode: quarantine_parent.inode,
        },
        NewStateTargetNormalizeDurabilityEvent::FinalCanonicalProven,
    ]
}

#[test]
fn startup_new_state_target_normalization_syncs_target_then_parent_then_proves_canonical_for_every_origin_and_outcome()
{
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            let fixture = residue_fixture(source, usr_outcome, 0o500);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let lease = normalize_target_lease(&fixture, &journal, &reservation);
            let expected = expected_events(&fixture);
            reset_effect_attempts();
            reset_new_state_target_normalize_durability_events();
            let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

            assert!(matches!(
                lease.reconcile(&seal, &journal).unwrap(),
                UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::RestartRequired
            ));

            assert_eq!(take_new_state_target_normalize_durability_events(), expected);
            assert_effect_attempts(&fixture, 1);
            fixture.assert_non_namespace_unchanged();
        }
    }
}

#[test]
fn startup_new_state_target_normalization_durability_faults_stop_at_exact_prefixes_as_ambiguous() {
    let cases = [
        (NewStateTargetNormalizeDurabilityFaultPoint::TargetSync, 0),
        (NewStateTargetNormalizeDurabilityFaultPoint::QuarantineParentSync, 1),
        (NewStateTargetNormalizeDurabilityFaultPoint::FinalCanonicalCapture, 2),
    ];
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for (fault, expected_prefix_len) in cases {
                let fixture = residue_fixture(source, usr_outcome, 0o500);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let lease = normalize_target_lease(&fixture, &journal, &reservation);
                let expected = expected_events(&fixture);
                reset_effect_attempts();
                reset_new_state_target_normalize_durability_events();
                arm_new_state_target_normalize_durability_fault(fault);
                let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

                assert!(matches!(
                    lease.reconcile(&seal, &journal).unwrap(),
                    UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::Ambiguous
                ));

                assert_eq!(
                    take_new_state_target_normalize_durability_events(),
                    expected[..expected_prefix_len]
                );
                assert_effect_attempts(&fixture, 1);
                fixture.assert_non_namespace_unchanged();
            }
        }
    }
}

#[derive(Clone, Copy)]
enum DurabilityRace {
    BeforeTargetSyncPublicReplacement,
    BetweenSyncsPublicReplacement,
    BetweenSyncsParentRebind,
    FinalCanonicalNamespaceChange,
}

#[test]
fn startup_new_state_target_normalization_durability_namespace_races_fail_closed_at_exact_prefixes() {
    let cases = [
        (DurabilityRace::BeforeTargetSyncPublicReplacement, 0),
        (DurabilityRace::BetweenSyncsPublicReplacement, 1),
        (DurabilityRace::BetweenSyncsParentRebind, 1),
        (DurabilityRace::FinalCanonicalNamespaceChange, 2),
    ];
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for (race, expected_prefix_len) in cases {
                let fixture = residue_fixture(source, usr_outcome, 0o500);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let lease = normalize_target_lease(&fixture, &journal, &reservation);
                let expected = expected_events(&fixture);
                let target = target_path(&fixture);
                reset_effect_attempts();
                reset_new_state_target_normalize_durability_events();
                match race {
                    DurabilityRace::BeforeTargetSyncPublicReplacement => {
                        let displaced = target.with_file_name("candidate-target-before-sync-retained");
                        arm_before_new_state_target_normalize_target_sync(move || {
                            replace_public_target(&target, &displaced);
                        });
                    }
                    DurabilityRace::BetweenSyncsPublicReplacement => {
                        let displaced = target.with_file_name("candidate-target-between-syncs-retained");
                        arm_before_new_state_target_normalize_quarantine_parent_sync(move || {
                            replace_public_target(&target, &displaced);
                        });
                    }
                    DurabilityRace::BetweenSyncsParentRebind => {
                        let quarantine = fixture.fixture.installation.state_quarantine_dir();
                        let displaced = quarantine.with_file_name("quarantine-between-normalize-syncs-retained");
                        arm_before_new_state_target_normalize_quarantine_parent_sync(move || {
                            fs::rename(&quarantine, displaced).unwrap();
                            fs::create_dir(&quarantine).unwrap();
                            fs::set_permissions(quarantine, fs::Permissions::from_mode(0o700)).unwrap();
                        });
                    }
                    DurabilityRace::FinalCanonicalNamespaceChange => {
                        arm_before_new_state_target_normalize_final_canonical_capture(
                            fixture.namespace_change_hook("candidate-target-normalize-final-canonical-race".to_owned()),
                        );
                    }
                }
                let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

                assert!(matches!(
                    lease.reconcile(&seal, &journal).unwrap(),
                    UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::Ambiguous
                ));

                assert_eq!(
                    take_new_state_target_normalize_durability_events(),
                    expected[..expected_prefix_len]
                );
                assert_effect_attempts(&fixture, 1);
                fixture.assert_non_namespace_unchanged();
            }
        }
    }
}

fn replace_public_target(target: &std::path::Path, displaced: &std::path::Path) {
    fs::rename(target, displaced).unwrap();
    fs::create_dir(target).unwrap();
    fs::set_permissions(target, fs::Permissions::from_mode(0o700)).unwrap();
}
