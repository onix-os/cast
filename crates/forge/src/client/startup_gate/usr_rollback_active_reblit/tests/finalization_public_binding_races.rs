//! Adversarial public journal directory, lock, and entry-set substitutions.

use std::{
    fs,
    io::Write as _,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            UsrRollbackActiveReblitFinalizationError, UsrRollbackActiveReblitFinalizationVerificationError,
            arm_after_usr_rollback_active_reblit_finalization_delete,
            arm_before_usr_rollback_active_reblit_finalization_final_durable_inspection,
            arm_before_usr_rollback_active_reblit_finalization_final_revalidation, finalize_usr_rollback_active_reblit,
        },
    },
    transition_journal::{RollbackActionOutcome, StorageError, encode},
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, assert_no_candidate_effects, build_active, capture_finalization_ready,
        persist_rollback_complete, reset_candidate_effect_observers,
    },
};

#[derive(Clone, Copy, Debug)]
enum Timing {
    BeforeDelete,
    AfterDelete,
}

#[test]
fn startup_active_reblit_finalization_rejects_public_directory_and_lock_substitution() {
    for timing in [Timing::BeforeDelete, Timing::AfterDelete] {
        for directory in [true, false] {
            let fixture = build_active(
                Epoch::Current,
                CandidateSource::Intent,
                RollbackActionOutcome::Applied,
                CandidateOrigin::AlreadySatisfied,
            );
            let terminal = persist_rollback_complete(&fixture, CandidateOrigin::Applied);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
            let root = fixture.fixture.installation.root.clone();
            let displaced = root.join(if directory {
                ".cast/journal-displaced-by-active-finalization-test"
            } else {
                ".cast/journal-lock-displaced-by-active-finalization-test"
            });
            let hook_root = root.clone();
            let hook_displaced = displaced.clone();
            arm_at(timing, move || {
                if directory {
                    substitute_journal_directory(&hook_root, &hook_displaced);
                } else {
                    substitute_journal_lock(&hook_root, &hook_displaced);
                }
            });
            reset_candidate_effect_observers();

            let error = finalize_usr_rollback_active_reblit(journal, authority).unwrap_err();

            assert_binding_error(timing, &error, directory);
            assert!(displaced.exists());
            assert_no_candidate_effects();
        }
    }
}

#[test]
fn startup_active_reblit_finalization_rejects_hidden_entry_set_substitution() {
    for timing in [Timing::BeforeDelete, Timing::AfterDelete] {
        let fixture = build_active(
            Epoch::Historical,
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOrigin::AlreadySatisfied,
        );
        let terminal = persist_rollback_complete(&fixture, CandidateOrigin::AlreadySatisfied);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
        let canonical = fixture.fixture.installation.root.join(".cast/journal/state-transition");
        let displaced = fixture
            .fixture
            .installation
            .root
            .join(".cast/journal/state-transition-displaced-by-active-finalization-test");
        let exact_bytes = fs::read(&canonical).unwrap();
        let hook_canonical = canonical.clone();
        let hook_displaced = displaced.clone();
        let hook_bytes = exact_bytes.clone();
        arm_at(timing, move || match timing {
            Timing::BeforeDelete => {
                fs::rename(&hook_canonical, &hook_displaced).unwrap();
                write_new_private_file(&hook_canonical, &hook_bytes);
            }
            Timing::AfterDelete => write_new_private_file(&hook_displaced, &hook_bytes),
        });
        reset_candidate_effect_observers();

        let error = finalize_usr_rollback_active_reblit(journal, authority).unwrap_err();

        assert_entry_set_error(timing, &error);
        assert_eq!(fs::read(&displaced).unwrap(), exact_bytes);
        assert_no_candidate_effects();
    }
}

#[test]
fn startup_active_reblit_finalization_rejects_source_recreation_after_delete_and_after_absence_proof() {
    let fixture = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let terminal = persist_rollback_complete(&fixture, CandidateOrigin::Applied);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
    let canonical = fixture.fixture.installation.root.join(".cast/journal/state-transition");
    let bytes = encode(&terminal).unwrap();
    reset_candidate_effect_observers();
    arm_after_usr_rollback_active_reblit_finalization_delete(move || write_new_private_file(&canonical, &bytes));

    let recreated = finalize_usr_rollback_active_reblit(journal, authority).unwrap_err();

    assert!(matches!(
        recreated,
        UsrRollbackActiveReblitFinalizationError::DeleteSucceededButRecordPresent
    ));
    assert_eq!(fixture.fixture.canonical_record(), terminal);
    drop(reservation);

    let fixture = build_active(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let terminal = persist_rollback_complete(&fixture, CandidateOrigin::AlreadySatisfied);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
    let canonical = fixture.fixture.installation.root.join(".cast/journal/state-transition");
    let bytes = encode(&terminal).unwrap();
    reset_candidate_effect_observers();
    arm_before_usr_rollback_active_reblit_finalization_final_durable_inspection(move || {
        write_new_private_file(&canonical, &bytes)
    });

    let changed = finalize_usr_rollback_active_reblit(journal, authority).unwrap_err();

    assert!(matches!(
        changed,
        UsrRollbackActiveReblitFinalizationError::PostDeleteVerification(
            UsrRollbackActiveReblitFinalizationVerificationError::JournalChangedDuringVerification { .. }
        )
    ));
    assert_eq!(fixture.fixture.canonical_record(), terminal);
    assert_no_candidate_effects();
}

fn arm_at(timing: Timing, hook: impl FnOnce() + 'static) {
    match timing {
        Timing::BeforeDelete => arm_before_usr_rollback_active_reblit_finalization_final_revalidation(hook),
        Timing::AfterDelete => arm_after_usr_rollback_active_reblit_finalization_delete(hook),
    }
}

fn assert_binding_error(timing: Timing, error: &UsrRollbackActiveReblitFinalizationError, directory: bool) {
    let expected = |source: &UsrRollbackActiveReblitFinalizationVerificationError| {
        matches!(
            source,
            UsrRollbackActiveReblitFinalizationVerificationError::Journal(
                StorageError::JournalDirectoryBindingChanged
            ) if directory
        ) || matches!(
            source,
            UsrRollbackActiveReblitFinalizationVerificationError::Journal(StorageError::JournalLockBindingChanged)
                if !directory
        )
    };
    let matched = match (timing, error) {
        (Timing::BeforeDelete, UsrRollbackActiveReblitFinalizationError::PreDeleteVerification(source)) => {
            expected(source)
        }
        (Timing::AfterDelete, UsrRollbackActiveReblitFinalizationError::PostDeleteVerification(source)) => {
            expected(source)
        }
        _ => false,
    };
    assert!(matched, "timing={timing:?}, directory={directory}: {error:?}");
}

fn assert_entry_set_error(timing: Timing, error: &UsrRollbackActiveReblitFinalizationError) {
    let matched = matches!(
        (timing, error),
        (
            Timing::BeforeDelete,
            UsrRollbackActiveReblitFinalizationError::PreDeleteVerification(
                UsrRollbackActiveReblitFinalizationVerificationError::Journal(
                    StorageError::JournalEntrySetMismatch { .. }
                )
            )
        ) | (
            Timing::AfterDelete,
            UsrRollbackActiveReblitFinalizationError::PostDeleteVerification(
                UsrRollbackActiveReblitFinalizationVerificationError::Journal(
                    StorageError::JournalEntrySetMismatch { .. }
                )
            )
        )
    );
    assert!(matched, "timing={timing:?}: {error:?}");
}

fn substitute_journal_directory(root: &Path, displaced: &Path) {
    let journal = root.join(".cast/journal");
    fs::rename(&journal, displaced).unwrap();
    fs::create_dir(&journal).unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
    create_private_lock(journal.join("state-transition.lock"));
}

fn substitute_journal_lock(root: &Path, displaced: &Path) {
    let lock = root.join(".cast/journal/state-transition.lock");
    fs::rename(&lock, displaced).unwrap();
    create_private_lock(lock);
}

fn create_private_lock(path: PathBuf) {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o600)).unwrap();
    file.sync_all().unwrap();
}

fn write_new_private_file(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o600)).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}
