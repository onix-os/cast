//! One-shot absent-target creation and mandatory restart contracts.

use std::{
    ffi::{CStr, CString},
    fs, io,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, PermissionsExt as _, symlink},
    },
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            NewStateTargetCreateFault, UsrRollbackCandidatePreserveAdmission,
            UsrRollbackCandidatePreserveApplyEffectSelection,
            UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation, arm_before_new_state_target_create_attempt,
            arm_before_new_state_target_create_reconciliation_capture,
            arm_before_usr_rollback_new_state_target_create_final_pre_capture, arm_new_state_target_create_fault,
            new_state_candidate_preserve_move_attempt_count, new_state_target_create_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count, reset_new_state_target_create_attempt_count,
        },
        startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
    },
    transition_journal::RollbackActionOutcome,
};

use super::{
    fixture::OperationKind,
    support::{CandidateLayout, CandidatePreserveFixture, CandidateSource, transition_quarantine_path},
};

macro_rules! create_target_lease {
    ($fixture:expr, $journal:expr, $reservation:expr) => {{
        let UsrRollbackCandidatePreserveAdmission::Apply(authority) = $fixture.capture($journal, $reservation) else {
            panic!("exact absent NewState target did not admit Apply authority");
        };
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
        let UsrRollbackCandidatePreserveApplyEffectSelection::CreateNewStateTarget(lease) =
            authority.into_effect_selection(&seal, $journal).unwrap()
        else {
            panic!("exact absent NewState target did not select its create lease");
        };
        lease
    }};
}

fn absent_fixture(source: CandidateSource, usr_outcome: RollbackActionOutcome) -> CandidatePreserveFixture {
    CandidatePreserveFixture::new(OperationKind::NewState, source, usr_outcome, CandidateLayout::Staged)
}

fn assert_candidate_never_moved(fixture: &CandidatePreserveFixture) {
    assert!(fixture.fixture.installation.staging_dir().join("usr").is_dir());
    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
}

#[test]
fn startup_new_state_target_creation_reconciles_raw_reports_semantically_and_requires_restart() {
    let cases = [
        (None, true),
        (Some(NewStateTargetCreateFault::ErrorAfterApply), true),
        (Some(NewStateTargetCreateFault::ErrorWithoutApply), false),
        (Some(NewStateTargetCreateFault::SuccessWithoutApply), false),
    ];

    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for (fault, expect_restart) in cases {
                let fixture = absent_fixture(source, usr_outcome);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let lease = create_target_lease!(&fixture, &journal, &reservation);
                let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
                let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
                reset_new_state_target_create_attempt_count();
                reset_new_state_candidate_preserve_move_attempt_count();
                if let Some(fault) = fault {
                    arm_new_state_target_create_fault(fault);
                }

                let result = lease.reconcile(&seal, &journal).unwrap();

                assert_eq!(
                    new_state_target_create_attempt_count(),
                    1,
                    "{source:?} {usr_outcome:?} {fault:?}"
                );
                match (expect_restart, result) {
                    (true, UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired) => {
                        let metadata = fs::symlink_metadata(&target).unwrap();
                        assert!(metadata.file_type().is_dir());
                        assert_eq!(metadata.uid(), nix::unistd::Uid::effective().as_raw());
                        assert_eq!(metadata.mode() & 0o7777 & !0o700, 0);
                        assert!(fs::read_dir(&target).unwrap().next().is_none());
                    }
                    (false, UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::NotApplied) => {
                        assert!(!target.exists());
                    }
                    (_, UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::Ambiguous) => {
                        panic!("stable semantic evidence was ambiguous for {source:?} {usr_outcome:?} {fault:?}");
                    }
                    (true, UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::NotApplied) => {
                        panic!("created target was classified NotApplied for {source:?} {usr_outcome:?} {fault:?}");
                    }
                    (false, UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired) => {
                        panic!("absent target was classified RestartRequired for {source:?} {usr_outcome:?} {fault:?}");
                    }
                }
                assert_candidate_never_moved(&fixture);
                fixture.assert_non_namespace_unchanged();
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ExactExistingTarget {
    EmptyPrivate,
    RestrictiveResidue,
}

#[test]
fn startup_new_state_target_creation_eexist_exact_post_state_requires_restart_without_claiming_apply() {
    for existing in [
        ExactExistingTarget::EmptyPrivate,
        ExactExistingTarget::RestrictiveResidue,
    ] {
        let fixture = absent_fixture(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = create_target_lease!(&fixture, &journal, &reservation);
        let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
        reset_new_state_target_create_attempt_count();
        reset_new_state_candidate_preserve_move_attempt_count();
        arm_before_new_state_target_create_attempt({
            let target = target.clone();
            move || {
                fs::create_dir(&target).unwrap();
                fs::set_permissions(
                    target,
                    fs::Permissions::from_mode(match existing {
                        ExactExistingTarget::EmptyPrivate => 0o700,
                        ExactExistingTarget::RestrictiveResidue => 0o500,
                    }),
                )
                .unwrap();
            }
        });
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

        assert!(matches!(
            lease.reconcile(&seal, &journal).unwrap(),
            UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired
        ));
        assert_eq!(new_state_target_create_attempt_count(), 1, "{existing:?}");
        assert_eq!(
            fs::symlink_metadata(target).unwrap().mode() & 0o7777,
            match existing {
                ExactExistingTarget::EmptyPrivate => 0o700,
                ExactExistingTarget::RestrictiveResidue => 0o500,
            }
        );
        assert_candidate_never_moved(&fixture);
        fixture.assert_non_namespace_unchanged();
    }
}

#[test]
fn startup_new_state_target_creation_accepts_every_restrictive_umask_residue_only_as_restart() {
    const CHILD: &str = "CAST_NEW_STATE_TARGET_CREATION_UMASK_CHILD";
    const TEST: &str = "client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_accepts_every_restrictive_umask_residue_only_as_restart";

    if let Some(mask) = std::env::var_os(CHILD) {
        let mask = u32::from_str_radix(mask.to_str().unwrap(), 8).unwrap();
        let fixture = absent_fixture(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = create_target_lease!(&fixture, &journal, &reservation);
        let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
        reset_new_state_target_create_attempt_count();
        reset_new_state_candidate_preserve_move_attempt_count();

        // SAFETY: the parent starts a separate process containing only this
        // exact test, and this branch restores its prior mask before checks.
        let previous = unsafe { nix::libc::umask(mask) };
        let result = lease.reconcile(&seal, &journal);
        // SAFETY: restore the child process mask immediately after creation.
        let retained = unsafe { nix::libc::umask(previous) };
        assert_eq!(retained, mask);

        assert!(matches!(
            result.unwrap(),
            UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired
        ));
        assert_eq!(new_state_target_create_attempt_count(), 1);
        assert_eq!(fs::symlink_metadata(target).unwrap().mode() & 0o7777, 0o700 & !mask);
        assert_candidate_never_moved(&fixture);
        fixture.assert_non_namespace_unchanged();
        return;
    }

    for mask in [0o100, 0o200, 0o300, 0o400, 0o500, 0o600, 0o700] {
        let output = Command::new(std::env::current_exe().unwrap())
            .arg(TEST)
            .arg("--exact")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(CHILD, format!("{mask:04o}"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "target-creation umask {mask:04o} child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn startup_new_state_target_creation_keeps_restrictive_residue_payload_opaque_and_requires_restart() {
    const CHILD: &str = "CAST_NEW_STATE_TARGET_CREATION_RESIDUE_CHILD";
    const TEST: &str = "client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_keeps_restrictive_residue_payload_opaque_and_requires_restart";

    if std::env::var_os(CHILD).is_some() {
        let fixture = absent_fixture(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = create_target_lease!(&fixture, &journal, &reservation);
        let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
        let payload = target.join("opaque-residue-payload");
        arm_before_new_state_target_create_reconciliation_capture({
            let payload = payload.clone();
            move || fs::write(payload, b"opaque").unwrap()
        });
        reset_new_state_target_create_attempt_count();
        reset_new_state_candidate_preserve_move_attempt_count();
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

        // Mode 0300 remains owner-writable while proving that the restrictive
        // residue is not reclassified as an inspected empty move target.
        // SAFETY: the parent starts an exact single-test child process.
        let previous = unsafe { nix::libc::umask(0o400) };
        let result = lease.reconcile(&seal, &journal);
        // SAFETY: restore the prior child-process mask before assertions.
        let retained = unsafe { nix::libc::umask(previous) };
        assert_eq!(retained, 0o400);

        assert!(matches!(
            result.unwrap(),
            UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired
        ));
        assert_eq!(new_state_target_create_attempt_count(), 1);
        assert_eq!(fs::symlink_metadata(&target).unwrap().mode() & 0o7777, 0o300);
        assert!(payload.exists());
        assert_candidate_never_moved(&fixture);
        fixture.assert_non_namespace_unchanged();
        return;
    }

    let output = Command::new(std::env::current_exe().unwrap())
        .arg(TEST)
        .arg("--exact")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "target-creation residue child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[derive(Clone, Copy, Debug)]
enum PostCreateChange {
    Payload,
    UnsafeMode,
    RegularFile,
    Symlink,
    Fifo,
    AccessAcl,
    DefaultAcl,
    TargetRemoved,
    TargetReplacedWithPayload,
    QuarantineParentRebound,
    UnrelatedNamespace,
}

#[test]
fn startup_new_state_target_creation_ambiguous_evidence_consumes_all_retry_capability() {
    for change in [
        PostCreateChange::Payload,
        PostCreateChange::UnsafeMode,
        PostCreateChange::RegularFile,
        PostCreateChange::Symlink,
        PostCreateChange::Fifo,
        PostCreateChange::AccessAcl,
        PostCreateChange::DefaultAcl,
        PostCreateChange::TargetRemoved,
        PostCreateChange::TargetReplacedWithPayload,
        PostCreateChange::QuarantineParentRebound,
        PostCreateChange::UnrelatedNamespace,
    ] {
        let fixture = absent_fixture(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = create_target_lease!(&fixture, &journal, &reservation);
        let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
        let hook: Box<dyn FnOnce()> = match change {
            PostCreateChange::Payload => {
                let target = target.clone();
                Box::new(move || fs::write(target.join("foreign-payload"), b"foreign").unwrap())
            }
            PostCreateChange::UnsafeMode => {
                let target = target.clone();
                Box::new(move || fs::set_permissions(target, fs::Permissions::from_mode(0o755)).unwrap())
            }
            PostCreateChange::RegularFile => {
                let target = target.clone();
                Box::new(move || {
                    fs::remove_dir(&target).unwrap();
                    fs::write(target, b"foreign type").unwrap();
                })
            }
            PostCreateChange::Symlink => {
                let target = target.clone();
                let external = fixture.fixture.installation.staging_dir();
                Box::new(move || {
                    fs::remove_dir(&target).unwrap();
                    symlink(external, target).unwrap();
                })
            }
            PostCreateChange::Fifo => {
                let target = target.clone();
                Box::new(move || {
                    fs::remove_dir(&target).unwrap();
                    let encoded = CString::new(target.as_os_str().as_bytes()).unwrap();
                    // SAFETY: the encoded absent fixture entry remains live.
                    assert_eq!(unsafe { nix::libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
                })
            }
            PostCreateChange::AccessAcl => {
                let target = target.clone();
                Box::new(move || install_acl_or_payload(&target, c"system.posix_acl_access"))
            }
            PostCreateChange::DefaultAcl => {
                let target = target.clone();
                Box::new(move || install_acl_or_payload(&target, c"system.posix_acl_default"))
            }
            PostCreateChange::TargetRemoved => {
                let target = target.clone();
                Box::new(move || fs::remove_dir(target).unwrap())
            }
            PostCreateChange::TargetReplacedWithPayload => {
                let target = target.clone();
                let displaced = target.with_file_name("candidate-target-create-displaced");
                Box::new(move || {
                    fs::rename(&target, displaced).unwrap();
                    fs::create_dir(&target).unwrap();
                    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
                    fs::write(target.join("replacement-payload"), b"foreign").unwrap();
                })
            }
            PostCreateChange::QuarantineParentRebound => {
                let quarantine = fixture.fixture.installation.state_quarantine_dir();
                let displaced = quarantine.with_file_name("quarantine-displaced-after-target-create");
                Box::new(move || {
                    fs::rename(&quarantine, displaced).unwrap();
                    fs::create_dir(&quarantine).unwrap();
                    fs::set_permissions(quarantine, fs::Permissions::from_mode(0o700)).unwrap();
                })
            }
            PostCreateChange::UnrelatedNamespace => {
                Box::new(fixture.namespace_change_hook("candidate-target-create-post-attempt-delta".to_owned()))
            }
        };
        arm_before_new_state_target_create_reconciliation_capture(hook);
        reset_new_state_target_create_attempt_count();
        reset_new_state_candidate_preserve_move_attempt_count();
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

        assert!(matches!(
            lease.reconcile(&seal, &journal).unwrap(),
            UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::Ambiguous
        ));
        assert_eq!(new_state_target_create_attempt_count(), 1, "{change:?}");
        assert_candidate_never_moved(&fixture);
        fixture.assert_non_namespace_unchanged();
    }
}

#[test]
fn startup_new_state_target_creation_absent_parent_metadata_delta_is_ambiguous() {
    let fixture = absent_fixture(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = create_target_lease!(&fixture, &journal, &reservation);
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    let transient = fixture
        .fixture
        .installation
        .state_quarantine_dir()
        .join("candidate-target-create-transient-parent-delta");
    arm_before_new_state_target_create_reconciliation_capture(move || {
        fs::create_dir(&transient).unwrap();
        fs::remove_dir(transient).unwrap();
    });
    reset_new_state_target_create_attempt_count();
    reset_new_state_candidate_preserve_move_attempt_count();
    arm_new_state_target_create_fault(NewStateTargetCreateFault::ErrorWithoutApply);
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(matches!(
        lease.reconcile(&seal, &journal).unwrap(),
        UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::Ambiguous
    ));
    assert_eq!(new_state_target_create_attempt_count(), 1);
    assert!(!target.exists());
    assert_candidate_never_moved(&fixture);
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_new_state_target_creation_accepts_an_exact_empty_replacement_only_as_restart() {
    let fixture = absent_fixture(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = create_target_lease!(&fixture, &journal, &reservation);
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    let displaced = target.with_file_name("candidate-target-create-safe-displaced");
    arm_before_new_state_target_create_reconciliation_capture({
        let target = target.clone();
        let displaced = displaced.clone();
        move || {
            fs::rename(&target, &displaced).unwrap();
            fs::create_dir(&target).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
            fs::remove_dir(displaced).unwrap();
        }
    });
    reset_new_state_target_create_attempt_count();
    reset_new_state_candidate_preserve_move_attempt_count();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(matches!(
        lease.reconcile(&seal, &journal).unwrap(),
        UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired
    ));
    assert_eq!(new_state_target_create_attempt_count(), 1);
    assert!(target.is_dir());
    assert!(!displaced.exists());
    assert_candidate_never_moved(&fixture);
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_new_state_target_creation_records_but_does_not_authorize_arbitrary_xattrs() {
    let fixture = absent_fixture(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = create_target_lease!(&fixture, &journal, &reservation);
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    let installed = Arc::new(AtomicBool::new(false));
    arm_before_new_state_target_create_reconciliation_capture({
        let target = target.clone();
        let installed = Arc::clone(&installed);
        move || installed.store(install_user_xattr(&target), Ordering::Relaxed)
    });
    reset_new_state_target_create_attempt_count();
    reset_new_state_candidate_preserve_move_attempt_count();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(matches!(
        lease.reconcile(&seal, &journal).unwrap(),
        UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired
    ));
    assert_eq!(new_state_target_create_attempt_count(), 1);
    if !installed.load(Ordering::Relaxed) {
        eprintln!("skipping arbitrary-xattr boundary assertion: fixture filesystem has no user xattrs");
    }
    assert_candidate_never_moved(&fixture);
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_new_state_target_creation_consumption_starts_with_the_open_journal_binding() {
    let fixture = absent_fixture(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
    let before = fixture.evidence_snapshots();
    let first = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = create_target_lease!(&fixture, &first, &reservation);
    drop(first);
    let second = fixture.open_journal();
    reset_new_state_target_create_attempt_count();
    reset_new_state_candidate_preserve_move_attempt_count();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(lease.reconcile(&seal, &second).is_err());
    assert_eq!(new_state_target_create_attempt_count(), 0);
    assert_candidate_never_moved(&fixture);
    fixture.assert_evidence_unchanged(&before);
}

#[derive(Clone, Copy, Debug)]
enum FinalPreRace {
    Database,
    Journal,
    Namespace,
    TargetAppeared,
    QuarantineParentRebound,
}

#[test]
fn startup_new_state_target_creation_final_pre_races_prevent_the_attempt() {
    for race in [
        FinalPreRace::Database,
        FinalPreRace::Journal,
        FinalPreRace::Namespace,
        FinalPreRace::TargetAppeared,
        FinalPreRace::QuarantineParentRebound,
    ] {
        let fixture = absent_fixture(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = create_target_lease!(&fixture, &journal, &reservation);
        let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
        let hook: Box<dyn FnOnce()> = match race {
            FinalPreRace::Database => Box::new(fixture.candidate_transition_clear_hook()),
            FinalPreRace::Journal => Box::new(fixture.journal_change_hook()),
            FinalPreRace::Namespace => {
                Box::new(fixture.namespace_change_hook("candidate-target-create-final-pre-delta".to_owned()))
            }
            FinalPreRace::TargetAppeared => {
                let target = target.clone();
                Box::new(move || {
                    fs::create_dir(&target).unwrap();
                    fs::set_permissions(target, fs::Permissions::from_mode(0o700)).unwrap();
                })
            }
            FinalPreRace::QuarantineParentRebound => {
                let quarantine = fixture.fixture.installation.state_quarantine_dir();
                let displaced = quarantine.with_file_name("quarantine-displaced-before-target-create");
                Box::new(move || {
                    fs::rename(&quarantine, displaced).unwrap();
                    fs::create_dir(&quarantine).unwrap();
                    fs::set_permissions(quarantine, fs::Permissions::from_mode(0o700)).unwrap();
                })
            }
        };
        arm_before_usr_rollback_new_state_target_create_final_pre_capture(hook);
        reset_new_state_target_create_attempt_count();
        reset_new_state_candidate_preserve_move_attempt_count();
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

        assert!(lease.reconcile(&seal, &journal).is_err(), "{race:?}");
        assert_eq!(new_state_target_create_attempt_count(), 0, "{race:?}");
        assert_candidate_never_moved(&fixture);
        assert_eq!(target.exists(), matches!(race, FinalPreRace::TargetAppeared));
    }
}

#[derive(Clone, Copy, Debug)]
enum TrailingEvidenceRace {
    Database,
    Journal,
}

#[test]
fn startup_new_state_target_creation_rechecks_database_and_journal_after_the_attempt() {
    for race in [TrailingEvidenceRace::Database, TrailingEvidenceRace::Journal] {
        let fixture = absent_fixture(CandidateSource::Exchanged, RollbackActionOutcome::Applied);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = create_target_lease!(&fixture, &journal, &reservation);
        let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
        let hook: Box<dyn FnOnce()> = match race {
            TrailingEvidenceRace::Database => Box::new(fixture.candidate_transition_clear_hook()),
            TrailingEvidenceRace::Journal => Box::new(fixture.journal_change_hook()),
        };
        arm_before_new_state_target_create_reconciliation_capture(hook);
        reset_new_state_target_create_attempt_count();
        reset_new_state_candidate_preserve_move_attempt_count();
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

        assert!(lease.reconcile(&seal, &journal).is_err(), "{race:?}");
        assert_eq!(new_state_target_create_attempt_count(), 1, "{race:?}");
        assert!(target.is_dir(), "{race:?}");
        assert_candidate_never_moved(&fixture);
    }
}

fn install_acl_or_payload(path: &Path, name: &CStr) {
    if !install_test_posix_acl(path, name) {
        fs::write(path.join("acl-unavailable-payload"), b"foreign").unwrap();
    }
}

fn install_test_posix_acl(path: &Path, name: &CStr) -> bool {
    const ACL_UNDEFINED_ID: u32 = u32::MAX;
    // One named-user entry prevents the kernel from collapsing this ACL into
    // ordinary mode bits, so the capture must reject the ACL itself.
    // SAFETY: geteuid has no arguments and cannot fail.
    let named_user = unsafe { nix::libc::geteuid() };
    let entries = [
        (0x01_u16, 0o7_u16, ACL_UNDEFINED_ID),
        (0x02, 0o4, named_user),
        (0x04, 0o5, ACL_UNDEFINED_ID),
        (0x10, 0o5, ACL_UNDEFINED_ID),
        (0x20, 0o5, ACL_UNDEFINED_ID),
    ];
    let mut value = Vec::with_capacity(4 + entries.len() * 8);
    value.extend_from_slice(&2_u32.to_le_bytes());
    for (tag, permissions, id) in entries {
        value.extend_from_slice(&tag.to_le_bytes());
        value.extend_from_slice(&permissions.to_le_bytes());
        value.extend_from_slice(&id.to_le_bytes());
    }
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: both C strings and the complete ACL value remain live.
    if unsafe { nix::libc::setxattr(path.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0) } == 0 {
        return true;
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(nix::libc::EOPNOTSUPP) | Some(nix::libc::EPERM)
    ) {
        eprintln!("skipping POSIX ACL assertion for {}: {error}", path.to_string_lossy());
        false
    } else {
        panic!("install target-creation test ACL: {error}");
    }
}

fn install_user_xattr(path: &Path) -> bool {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let value = b"diagnostic-only";
    // SAFETY: the path, static name, and complete value remain live.
    if unsafe {
        nix::libc::setxattr(
            path.as_ptr(),
            c"user.cast.target-create-boundary".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    } == 0
    {
        return true;
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(nix::libc::EOPNOTSUPP) | Some(nix::libc::EPERM)
    ) {
        false
    } else {
        panic!("install target-creation test user xattr: {error}");
    }
}
