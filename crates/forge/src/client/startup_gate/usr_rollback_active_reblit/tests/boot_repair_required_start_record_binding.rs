//! Exact predecessor and successor record-binding races for the conservative
//! `BootRepairRequired -> BootRepairStarted` journal boundary.

use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{
    client::{
        startup_gate,
        startup_recovery::{
            DurableUsrRollbackActiveReblitBootRepairStartRecord,
            UsrRollbackActiveReblitBootRepairStartPersistenceError,
            UsrRollbackActiveReblitBootRepairStartSuccessorBindingError,
            arm_after_usr_rollback_active_reblit_boot_repair_start_successor_binding_check_before_reopen,
            arm_before_usr_rollback_active_reblit_boot_repair_start_successor_binding_revalidation,
        },
    },
    transition_journal::{
        PublicBindingRevalidationBoundary, TransitionRecord, arm_public_binding_revalidation_callback,
        assert_public_binding_revalidation_callback_consumed,
    },
};

use super::{
    super::{Error as ActiveReblitDispatchError, test_fixture::BootSyncStartedLayout},
    support::{
        BootRepairFixture, CandidateOrigin, Epoch, UsrRestoreOrigin, assert_boot_start_persistence_advance,
        assert_no_boot_synchronize_attempts, assert_no_candidate_effects, assert_pending_phase,
        boot_active_wrapper_path, build_boot_sync_started, drive_boot_sync_started_to_candidate_preserved, enter_boot,
        expected_boot_repair_required, expected_boot_repair_started, reset_boot_synchronize_observer,
        reset_candidate_effect_observers,
    },
};

fn canonical_journal(fixture: &BootRepairFixture) -> std::path::PathBuf {
    fixture
        .fixture
        .installation
        .root
        .join(".cast/journal/state-transition")
}

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn same_byte_different_inode_hook(fixture: &BootRepairFixture, label: String) -> impl FnOnce() + 'static {
    let canonical = canonical_journal(fixture);
    let displaced = fixture
        .fixture
        .installation
        .root
        .join(".cast/journal")
        .join(format!(".{label}-displaced"));
    move || {
        let bytes = fs::read(&canonical).unwrap();
        fs::rename(&canonical, &displaced).unwrap();
        let retained_identity = inode_identity(&displaced);
        fs::write(&canonical, &bytes).unwrap();
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::read(&canonical).unwrap(), bytes);
        assert_ne!(retained_identity, inode_identity(&canonical));
        fs::remove_file(displaced).unwrap();
    }
}

fn prepare_required(
    epoch: Epoch,
    usr_origin: UsrRestoreOrigin,
    candidate_origin: CandidateOrigin,
) -> (BootRepairFixture, TransitionRecord, TransitionRecord) {
    let fixture = build_boot_sync_started(epoch, BootSyncStartedLayout::Post);
    let preserved = drive_boot_sync_started_to_candidate_preserved(&fixture, usr_origin, candidate_origin);
    let required = expected_boot_repair_required(&preserved);

    let required_error = enter_boot(&fixture);

    assert_pending_phase(&required_error, crate::transition_journal::Phase::BootRepairRequired);
    assert_eq!(fixture.fixture.canonical_record(), required);
    let started = expected_boot_repair_started(&required);
    (fixture, required, started)
}

fn assert_start_only(
    fixture: &BootRepairFixture,
    database_before: &super::super::test_fixture::DatabaseSnapshot,
    namespace_before: &[super::super::test_fixture::NamespaceEntry],
) {
    assert_eq!(fixture.fixture.database_snapshot(), *database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert!(boot_active_wrapper_path(fixture).join("usr").is_dir());
    assert_no_candidate_effects();
    assert_no_boot_synchronize_attempts();
}

fn assert_successor_binding_changed(
    error: &startup_gate::Error,
    expected: DurableUsrRollbackActiveReblitBootRepairStartRecord,
) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackActiveReblitDispatch(
                ActiveReblitDispatchError::BootRepairStartPersistence(
                    UsrRollbackActiveReblitBootRepairStartPersistenceError::SuccessorRecordBinding {
                        durable,
                        source: UsrRollbackActiveReblitBootRepairStartSuccessorBindingError::Changed,
                    }
                )
            ) if *durable == expected
        ),
        "expected durable {expected:?} BootRepairStarted binding failure, got {error:?}"
    );
}

#[test]
fn startup_active_reblit_boot_repair_required_start_bound_advance_same_byte_replacements_never_succeed() {
    let mut cases = 0;
    for (boundary, expected_durable) in [
        (
            PublicBindingRevalidationBoundary::BeforeBoundAdvancePublish,
            DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairRequired,
        ),
        (
            PublicBindingRevalidationBoundary::BeforeBoundAdvanceFinalBinding,
            DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted,
        ),
    ] {
        for epoch in Epoch::ALL {
            for usr_origin in UsrRestoreOrigin::ALL {
                for candidate_origin in CandidateOrigin::ALL {
                    let (fixture, required, started) = prepare_required(epoch, usr_origin, candidate_origin);
                    let database_before = fixture.fixture.database_snapshot();
                    let namespace_before = fixture.fixture.namespace_snapshot();
                    reset_candidate_effect_observers();
                    reset_boot_synchronize_observer();
                    let hook = same_byte_different_inode_hook(
                        &fixture,
                        format!("boot-start-bound-{boundary:?}-{epoch:?}-{usr_origin:?}-{candidate_origin:?}"),
                    );
                    arm_public_binding_revalidation_callback(boundary, hook);

                    let error = enter_boot(&fixture);

                    assert_public_binding_revalidation_callback_consumed();
                    assert_boot_start_persistence_advance(&error, expected_durable);
                    match expected_durable {
                        DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairRequired => {
                            assert_eq!(fixture.fixture.canonical_record(), required)
                        }
                        DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted => {
                            assert_eq!(fixture.fixture.canonical_record(), started)
                        }
                    }
                    assert_start_only(&fixture, &database_before, &namespace_before);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 16);
}

#[test]
fn startup_active_reblit_boot_repair_required_start_same_byte_successor_replacement_fails_same_store_binding() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for usr_origin in UsrRestoreOrigin::ALL {
            for candidate_origin in CandidateOrigin::ALL {
                let (fixture, _required, started) = prepare_required(epoch, usr_origin, candidate_origin);
                let database_before = fixture.fixture.database_snapshot();
                let namespace_before = fixture.fixture.namespace_snapshot();
                reset_candidate_effect_observers();
                reset_boot_synchronize_observer();
                let hook = same_byte_different_inode_hook(
                    &fixture,
                    format!("boot-start-published-{epoch:?}-{usr_origin:?}-{candidate_origin:?}"),
                );
                arm_before_usr_rollback_active_reblit_boot_repair_start_successor_binding_revalidation(hook);

                let error = enter_boot(&fixture);

                assert_successor_binding_changed(
                    &error,
                    DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted,
                );
                assert_eq!(fixture.fixture.canonical_record(), started);
                assert_start_only(&fixture, &database_before, &namespace_before);
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 8);
}

#[test]
fn startup_active_reblit_boot_repair_required_start_same_byte_successor_replacement_fails_reopened_binding() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for usr_origin in UsrRestoreOrigin::ALL {
            for candidate_origin in CandidateOrigin::ALL {
                let (fixture, _required, started) = prepare_required(epoch, usr_origin, candidate_origin);
                let database_before = fixture.fixture.database_snapshot();
                let namespace_before = fixture.fixture.namespace_snapshot();
                reset_candidate_effect_observers();
                reset_boot_synchronize_observer();
                let hook = same_byte_different_inode_hook(
                    &fixture,
                    format!("boot-start-reopened-{epoch:?}-{usr_origin:?}-{candidate_origin:?}"),
                );
                arm_after_usr_rollback_active_reblit_boot_repair_start_successor_binding_check_before_reopen(hook);

                let error = enter_boot(&fixture);

                assert_successor_binding_changed(
                    &error,
                    DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted,
                );
                assert_eq!(fixture.fixture.canonical_record(), started);
                assert_start_only(&fixture, &database_before, &namespace_before);
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 8);
}
