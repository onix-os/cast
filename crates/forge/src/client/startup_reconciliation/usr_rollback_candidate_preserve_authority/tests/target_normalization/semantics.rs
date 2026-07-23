//! Semantic classification of the single descriptor-bound chmod attempt.

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            NewStateTargetNormalizeFault, UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation,
            arm_before_new_state_target_normalize_attempt,
            arm_before_new_state_target_normalize_reconciliation_capture, arm_new_state_target_normalize_fault,
        },
        startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
    },
    transition_journal::RollbackActionOutcome,
};

use super::super::support::CandidateSource;
use super::support::{
    RESTRICTIVE_RESIDUE_MODES, assert_effect_attempts, install_acl_or_payload, install_user_xattr, normal_fixture,
    normalize_target_lease, reset_effect_attempts, residue_fixture, target_identity, target_path,
};

#[test]
fn startup_new_state_target_normalization_reconciles_raw_reports_semantically_and_requires_restart() {
    let cases = [
        (None, true),
        (Some(NewStateTargetNormalizeFault::ErrorAfterApply), true),
        (Some(NewStateTargetNormalizeFault::ErrorWithoutApply), false),
        (Some(NewStateTargetNormalizeFault::SuccessWithoutApply), false),
    ];

    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for (fault, expect_restart) in cases {
                let fixture = residue_fixture(source, usr_outcome, 0o500);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let lease = normalize_target_lease(&fixture, &journal, &reservation);
                let target = target_path(&fixture);
                let before = target_identity(&target);
                reset_effect_attempts();
                if let Some(fault) = fault {
                    arm_new_state_target_normalize_fault(fault);
                }
                let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

                let result = lease.reconcile(&seal, &journal).unwrap();

                assert_effect_attempts(&fixture, 1);
                match (expect_restart, result) {
                    (true, UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::RestartRequired(_)) => {
                        assert_eq!(target_identity(&target).mode, 0o700)
                    }
                    (false, UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::NotApplied) => {
                        assert_eq!(target_identity(&target), before);
                    }
                    (_, UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::Ambiguous) => {
                        panic!("stable normalization evidence was ambiguous for {source:?} {usr_outcome:?} {fault:?}");
                    }
                    (true, UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::NotApplied) => {
                        panic!(
                            "applied normalization was classified NotApplied for {source:?} {usr_outcome:?} {fault:?}"
                        );
                    }
                    (false, UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::RestartRequired(_)) => {
                        panic!(
                            "unapplied normalization was classified RestartRequired for {source:?} {usr_outcome:?} {fault:?}"
                        );
                    }
                }
                let after = target_identity(&target);
                assert_eq!((after.device, after.inode), (before.device, before.inode));
                fixture.assert_non_namespace_unchanged();
            }
        }
    }
}

#[test]
fn startup_new_state_target_normalization_accepts_every_restrictive_mode_for_every_origin() {
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for mode in RESTRICTIVE_RESIDUE_MODES {
                let fixture = residue_fixture(source, usr_outcome, mode);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let lease = normalize_target_lease(&fixture, &journal, &reservation);
                let target = target_path(&fixture);
                let before = target_identity(&target);
                reset_effect_attempts();
                let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

                assert!(matches!(
                    lease.reconcile(&seal, &journal).unwrap(),
                    UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::RestartRequired(_)
                ));
                assert_effect_attempts(&fixture, 1);
                let after = target_identity(&target);
                assert_eq!((after.device, after.inode), (before.device, before.inode));
                assert_eq!(after.mode, 0o700, "{source:?} {usr_outcome:?} {mode:04o}");
                fixture.assert_non_namespace_unchanged();
            }
        }
    }
}

#[test]
fn startup_new_state_target_normalization_accepts_concurrent_same_inode_canonicalization_only_as_restart() {
    let fixture = normal_fixture(0o500);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = normalize_target_lease(&fixture, &journal, &reservation);
    let target = target_path(&fixture);
    let before = target_identity(&target);
    reset_effect_attempts();
    arm_new_state_target_normalize_fault(NewStateTargetNormalizeFault::ErrorWithoutApply);
    arm_before_new_state_target_normalize_attempt({
        let target = target.clone();
        move || fs::set_permissions(target, fs::Permissions::from_mode(0o700)).unwrap()
    });
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(matches!(
        lease.reconcile(&seal, &journal).unwrap(),
        UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::RestartRequired(_)
    ));
    assert_effect_attempts(&fixture, 1);
    let after = target_identity(&target);
    assert_eq!((after.device, after.inode), (before.device, before.inode));
    assert_eq!(after.mode, 0o700);
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_new_state_target_normalization_stays_bound_to_the_retained_inode_after_public_replacement() {
    let fixture = normal_fixture(0o300);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = normalize_target_lease(&fixture, &journal, &reservation);
    let target = target_path(&fixture);
    let displaced = target.with_file_name("candidate-target-normalize-retained");
    let before = target_identity(&target);
    reset_effect_attempts();
    arm_before_new_state_target_normalize_attempt({
        let target = target.clone();
        let displaced = displaced.clone();
        move || {
            fs::rename(&target, &displaced).unwrap();
            fs::create_dir(&target).unwrap();
            fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap();
        }
    });
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(matches!(
        lease.reconcile(&seal, &journal).unwrap(),
        UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::Ambiguous
    ));
    assert_effect_attempts(&fixture, 1);
    let retained = target_identity(&displaced);
    let public = target_identity(&target);
    assert_eq!((retained.device, retained.inode), (before.device, before.inode));
    assert_eq!(retained.mode, 0o700);
    assert_ne!((public.device, public.inode), (before.device, before.inode));
    assert_eq!(public.mode, 0o500);
    fixture.assert_non_namespace_unchanged();
}

#[derive(Clone, Copy, Debug)]
enum PostNormalizeBoundary {
    Payload,
    AccessAcl,
    DefaultAcl,
    ArbitraryUserXattr,
}

#[test]
fn startup_new_state_target_normalization_enforces_payload_acl_and_xattr_boundaries() {
    for boundary in [
        PostNormalizeBoundary::Payload,
        PostNormalizeBoundary::AccessAcl,
        PostNormalizeBoundary::DefaultAcl,
        PostNormalizeBoundary::ArbitraryUserXattr,
    ] {
        let fixture = normal_fixture(0o500);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = normalize_target_lease(&fixture, &journal, &reservation);
        let target = target_path(&fixture);
        let user_xattr_installed = Arc::new(AtomicBool::new(false));
        reset_effect_attempts();
        arm_before_new_state_target_normalize_reconciliation_capture({
            let target = target.clone();
            let user_xattr_installed = Arc::clone(&user_xattr_installed);
            move || match boundary {
                PostNormalizeBoundary::Payload => {
                    fs::write(target.join("foreign-payload"), b"foreign").unwrap();
                }
                PostNormalizeBoundary::AccessAcl => {
                    install_acl_or_payload(&target, c"system.posix_acl_access");
                }
                PostNormalizeBoundary::DefaultAcl => {
                    install_acl_or_payload(&target, c"system.posix_acl_default");
                }
                PostNormalizeBoundary::ArbitraryUserXattr => {
                    user_xattr_installed.store(install_user_xattr(&target), Ordering::Relaxed);
                }
            }
        });
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

        let result = lease.reconcile(&seal, &journal).unwrap();
        assert_effect_attempts(&fixture, 1);
        match boundary {
            PostNormalizeBoundary::Payload | PostNormalizeBoundary::AccessAcl | PostNormalizeBoundary::DefaultAcl => {
                assert!(matches!(
                    result,
                    UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::Ambiguous
                ));
            }
            PostNormalizeBoundary::ArbitraryUserXattr => {
                assert!(matches!(
                    result,
                    UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::RestartRequired(_)
                ));
                if !user_xattr_installed.load(Ordering::Relaxed) {
                    eprintln!("skipping arbitrary-xattr boundary assertion: fixture filesystem has no user xattrs");
                }
            }
        }
        // Installing a named access ACL may project its mask into ordinary
        // group mode bits. The semantic assertion above is the boundary: the
        // resulting target is ambiguous rather than canonical. Other changes
        // leave the descriptor-bound 0700 mode intact.
        if !matches!(boundary, PostNormalizeBoundary::AccessAcl) {
            assert_eq!(target_identity(&target).mode, 0o700, "{boundary:?}");
        }
        fixture.assert_non_namespace_unchanged();
    }
}
